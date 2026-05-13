use super::aggregate_helpers::*;
use super::dml_plans::best_eq_lookup_index;
use super::join_plans::{build_hash_join_key, JoinFxBuildHasher, JoinHashKey};
use super::*;

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
struct SimpleGroupFilter {
    column_ordinal: usize,
    op: SimpleGroupFilterOp,
    literal: Value,
}

#[derive(Clone, Copy)]
enum SimpleGroupOutput {
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
    fn try_count_eq_and_range_filter(
        &self,
        context: &ExecutionContext,
        table_id: RelationId,
        access_path: &ScanAccessPath,
        filter: &TypedExpr,
    ) -> DbResult<Option<u64>> {
        let Some(compound_filter) = extract_aggregate_eq_and_range_literal_filter(filter) else {
            return Ok(None);
        };

        if let ScanAccessPath::IndexEqRangeComposite {
            index_id,
            eq_values,
            lower,
            upper,
        } = access_path
        {
            let key_range = composite_prefix_range_lookup_key_range(eq_values, lower, upper);
            match self.storage_dml.visible_index_row_count(
                context.txn_id,
                &context.snapshot,
                *index_id,
                key_range,
            ) {
                Ok(count) => return Ok(Some(count)),
                Err(error) if error.sqlstate() == SqlState::FeatureNotSupported => {}
                Err(error) => return Err(error),
            }
        }

        let Some(projected_columns) = self.table_column_ids_for_ordinals(
            context,
            table_id,
            &[
                compound_filter.eq_column_ordinal,
                compound_filter.range_column_ordinal,
            ],
        )?
        else {
            return Ok(None);
        };
        let Some(eq_column_id) = projected_columns.first().copied() else {
            return Ok(None);
        };
        let Some(range_column_id) = projected_columns.get(1).copied() else {
            return Ok(None);
        };

        if !matches!(access_path, ScanAccessPath::IndexEqRangeComposite { .. }) {
            for index in self
                .catalog_reader
                .list_indexes(context.txn_id, table_id)?
                .into_iter()
                .filter(|index| index.kind == IndexKind::BTree && index.key_columns.len() >= 2)
            {
                if index.key_columns[0].column_id != eq_column_id
                    || index.key_columns[1].column_id != range_column_id
                {
                    continue;
                }
                let eq_values = [compound_filter.eq_literal.clone()];
                let lower = std::ops::Bound::Included(compound_filter.low_literal.clone());
                let upper = std::ops::Bound::Included(compound_filter.high_literal.clone());
                let key_range = composite_prefix_range_lookup_key_range(&eq_values, &lower, &upper);
                match self.storage_dml.visible_index_row_count(
                    context.txn_id,
                    &context.snapshot,
                    index.index_id,
                    key_range,
                ) {
                    Ok(count) => return Ok(Some(count)),
                    Err(error) if error.sqlstate() == SqlState::FeatureNotSupported => {}
                    Err(error) => return Err(error),
                }
            }
        }

        let mut stream =
            match self.resolve_scan_stream(context, table_id, access_path, Some(projected_columns))
            {
                Ok(stream) => stream,
                Err(error) if error.sqlstate() == SqlState::FeatureNotSupported => {
                    self.storage_dml.scan_table_eq_filter(
                        context.txn_id,
                        &context.snapshot,
                        table_id,
                        eq_column_id,
                        &compound_filter.eq_literal,
                        Some(
                            self.table_column_ids_for_ordinals(
                                context,
                                table_id,
                                &[
                                    compound_filter.eq_column_ordinal,
                                    compound_filter.range_column_ordinal,
                                ],
                            )?
                            .unwrap_or_default(),
                        ),
                    )?
                }
                Err(error) => return Err(error),
            };

        let has_interrupts = context.has_execution_interrupts();
        let mut count = 0u64;
        while let Some(record) = stream.next()? {
            if has_interrupts {
                context.check_deadline()?;
            }
            if row_matches_aggregate_simple_eq_literal_filter(
                &record.row,
                0,
                &compound_filter.eq_literal,
            )? && row_matches_aggregate_between_literal_filter(
                &record.row,
                1,
                &compound_filter.low_literal,
                &compound_filter.high_literal,
            )? {
                count = count.saturating_add(1);
            }
        }
        Ok(Some(count))
    }

    /// `SELECT COUNT(*) FROM t WHERE col IN (lit1, lit2, ..., litN)`
    /// fast path: sum the per-literal index-backed visible counts
    /// instead of fetching all matching rows from the heap. Used by
    /// the same OLTP shape that the IN-list `BitmapOr` access path
    /// targets, but for COUNT we want just an integer, not the rows.
    fn try_count_in_literal_filter(
        &self,
        context: &ExecutionContext,
        table_id: RelationId,
        filter: &TypedExpr,
    ) -> DbResult<Option<u64>> {
        let Some((column_ordinal, literals)) = extract_aggregate_in_literal_filter(filter) else {
            return Ok(None);
        };
        let Some(projected_columns) =
            self.table_column_ids_for_ordinals(context, table_id, &[column_ordinal])?
        else {
            return Ok(None);
        };
        let Some(filter_column_id) = projected_columns.first().copied() else {
            return Ok(None);
        };

        let mode = if context.isolation == aiondb_tx::IsolationLevel::Serializable {
            LockMode::PredicateRead
        } else {
            LockMode::AccessShare
        };
        self.lock_table(context, table_id, mode)?;
        context.record_relation_read(table_id)?;

        let mut total = 0u64;
        for literal in &literals {
            match self.storage_dml.visible_eq_row_count(
                context.txn_id,
                &context.snapshot,
                table_id,
                filter_column_id,
                literal,
            ) {
                Ok(count) => {
                    total = total.saturating_add(count);
                }
                Err(error) if error.sqlstate() == SqlState::FeatureNotSupported => {
                    // No backing index for this column type - bail
                    // out so the slow path can run consistently.
                    return Ok(None);
                }
                Err(error) => return Err(error),
            }
        }
        Ok(Some(total))
    }

    /// Fast path for `SELECT COUNT(*) FROM t WHERE col CMP literal`
    /// (and `BETWEEN`) when no index covers the predicate column.
    /// Uses the storage-side range pushdown
    /// (`StorageDML::scan_table_range_filter`, the qualEval-in-scan
    /// loop) instead of materialising every row through the executor's
    /// generic evaluator. Falls back to `Ok(None)` when the backend
    /// reports `FeatureNotSupported` or when the bound types fall
    /// outside the storage compare-safe set.
    fn try_count_index_range_filter(
        &self,
        context: &ExecutionContext,
        access_path: &ScanAccessPath,
        filter: &TypedExpr,
    ) -> DbResult<Option<u64>> {
        let Some((_range_ordinal, lower, upper)) = aggregate_simple_range_literal_filter(filter)
        else {
            return Ok(None);
        };
        if !aggregate_range_bound_storage_safe(&lower, &upper) {
            return Ok(None);
        }
        let (index_id, key_range) = match access_path {
            ScanAccessPath::IndexRange {
                index_id,
                lower,
                upper,
            } => (*index_id, range_lookup_key_range(lower, upper)),
            ScanAccessPath::IndexOnlyScan { inner, .. } => {
                return self.try_count_index_range_filter(context, inner, filter);
            }
            _ => return Ok(None),
        };
        match self.storage_dml.visible_index_row_count(
            context.txn_id,
            &context.snapshot,
            index_id,
            key_range,
        ) {
            Ok(count) => Ok(Some(count)),
            Err(error) if error.sqlstate() == SqlState::FeatureNotSupported => Ok(None),
            Err(error) => Err(error),
        }
    }

    fn try_count_simple_range_filter(
        &self,
        context: &ExecutionContext,
        table_id: RelationId,
        filter: &TypedExpr,
    ) -> DbResult<Option<u64>> {
        let Some((range_ordinal, lower, upper)) = aggregate_simple_range_literal_filter(filter)
        else {
            return Ok(None);
        };
        if !aggregate_range_bound_storage_safe(&lower, &upper) {
            return Ok(None);
        }
        let Some(projected_columns) =
            self.table_column_ids_for_ordinals(context, table_id, &[range_ordinal])?
        else {
            return Ok(None);
        };
        let Some(filter_column_id) = projected_columns.first().copied() else {
            return Ok(None);
        };

        let mode = if context.isolation == aiondb_tx::IsolationLevel::Serializable {
            LockMode::PredicateRead
        } else {
            LockMode::AccessShare
        };
        self.lock_table(context, table_id, mode)?;
        context.record_relation_read(table_id)?;

        let mut stream = match self.storage_dml.scan_table_range_filter(
            context.txn_id,
            &context.snapshot,
            table_id,
            filter_column_id,
            lower,
            upper,
            // Empty projection — count(*) doesn't need any column data.
            Some(Vec::new()),
        ) {
            Ok(stream) => stream,
            Err(error) if error.sqlstate() == SqlState::FeatureNotSupported => {
                return Ok(None);
            }
            Err(error) => return Err(error),
        };
        let has_interrupts = context.has_execution_interrupts();
        let mut count = 0u64;
        while let Some(_record) = stream.next()? {
            if has_interrupts {
                context.check_deadline()?;
            }
            count = count.saturating_add(1);
        }
        Ok(Some(count))
    }

    /// COUNT(*) variant of the projection-side multi-range pushdown.
    /// Uses `StorageDML::scan_table_multi_range_filter` to apply every
    /// AND-combined `col CMP literal` (and `col = literal`) bound
    /// inline in the scan loop, then counts matching tuples.
    /// Falls back to `Ok(None)` when the filter isn't a multi-column
    /// AND-of-ranges or the bound types fall outside the storage
    /// compare-safe set.
    fn try_count_multi_range_filter(
        &self,
        context: &ExecutionContext,
        table_id: RelationId,
        filter: &TypedExpr,
    ) -> DbResult<Option<u64>> {
        let Some(filters) = aggregate_multi_range_literal_filter(filter) else {
            return Ok(None);
        };
        if filters.len() < 2 {
            return Ok(None);
        }
        if !filters
            .iter()
            .all(|(_, lo, hi)| aggregate_range_bound_storage_safe(lo, hi))
        {
            return Ok(None);
        }
        let mut filter_column_ids = Vec::with_capacity(filters.len());
        for (ord, _, _) in &filters {
            let Some(col) = self
                .table_column_ids_for_ordinals(context, table_id, &[*ord])?
                .and_then(|cols| cols.into_iter().next())
            else {
                return Ok(None);
            };
            filter_column_ids.push(col);
        }
        let storage_filters: Vec<_> = filters
            .iter()
            .zip(filter_column_ids.into_iter())
            .map(|((_, lo, hi), col)| (col, lo.clone(), hi.clone()))
            .collect();

        let mode = if context.isolation == aiondb_tx::IsolationLevel::Serializable {
            LockMode::PredicateRead
        } else {
            LockMode::AccessShare
        };
        self.lock_table(context, table_id, mode)?;
        context.record_relation_read(table_id)?;

        let mut stream = match self.storage_dml.scan_table_multi_range_filter(
            context.txn_id,
            &context.snapshot,
            table_id,
            &storage_filters,
            // Empty projection — count(*) doesn't need column data.
            Some(Vec::new()),
        ) {
            Ok(stream) => stream,
            Err(error) if error.sqlstate() == SqlState::FeatureNotSupported => {
                return Ok(None);
            }
            Err(error) => return Err(error),
        };
        let has_interrupts = context.has_execution_interrupts();
        let mut count = 0u64;
        while let Some(_record) = stream.next()? {
            if has_interrupts {
                context.check_deadline()?;
            }
            count = count.saturating_add(1);
        }
        Ok(Some(count))
    }

    fn try_count_simple_eq_filter(
        &self,
        context: &ExecutionContext,
        table_id: RelationId,
        filter: &TypedExpr,
    ) -> DbResult<Option<u64>> {
        let Some(simple_filter) = extract_aggregate_simple_eq_literal_filter(filter) else {
            return Ok(None);
        };
        let Some(projected_columns) =
            self.table_column_ids_for_ordinals(context, table_id, &[simple_filter.column_ordinal])?
        else {
            return Ok(None);
        };
        let Some(filter_column_id) = projected_columns.first().copied() else {
            return Ok(None);
        };

        let mode = if context.isolation == aiondb_tx::IsolationLevel::Serializable {
            LockMode::PredicateRead
        } else {
            LockMode::AccessShare
        };
        self.lock_table(context, table_id, mode)?;
        context.record_relation_read(table_id)?;

        match self.storage_dml.visible_eq_row_count(
            context.txn_id,
            &context.snapshot,
            table_id,
            filter_column_id,
            &simple_filter.literal,
        ) {
            Ok(count) => return Ok(Some(count)),
            Err(error) if error.sqlstate() == SqlState::FeatureNotSupported => {}
            Err(error) => return Err(error),
        }

        let mut stream = match self.storage_dml.scan_table_eq_filter(
            context.txn_id,
            &context.snapshot,
            table_id,
            filter_column_id,
            &simple_filter.literal,
            Some(vec![filter_column_id]),
        ) {
            Ok(stream) => stream,
            Err(error) if error.sqlstate() == SqlState::FeatureNotSupported => {
                return Ok(None);
            }
            Err(error) => return Err(error),
        };
        let has_interrupts = context.has_execution_interrupts();
        let mut count = 0u64;
        while let Some(record) = stream.next()? {
            if has_interrupts {
                context.check_deadline()?;
            }
            if row_matches_aggregate_simple_eq_literal_filter(
                &record.row,
                0,
                &simple_filter.literal,
            )? {
                count = count.saturating_add(1);
            }
        }
        Ok(Some(count))
    }

    /// `SELECT MIN(col) FROM t` / `SELECT MAX(col) FROM t` fast path
    /// when the column has a single-column btree index. Walks the
    /// first / last leaf entry directly via
    /// `index_min_single_column_value` / `index_max_single_column_value`
    /// instead of materialising every row through `scan_index`.
    /// Returns `Ok(Some(value))` when the index path produced a
    /// result (including SQL NULL for empty tables) and `Ok(None)`
    /// when the column isn't index-backed or the snapshot is
    /// historical (caller falls through to the slow accumulator
    /// path).
    pub(super) fn try_min_or_max_via_index(
        &self,
        context: &ExecutionContext,
        table_id: RelationId,
        column_ordinal: usize,
        result_data_type: &aiondb_core::DataType,
        is_max: bool,
    ) -> DbResult<Option<Value>> {
        let Some(projected_columns) =
            self.table_column_ids_for_ordinals(context, table_id, &[column_ordinal])?
        else {
            return Ok(None);
        };
        let Some(target_column_id) = projected_columns.first().copied() else {
            return Ok(None);
        };
        let filter_indexes = self.catalog_reader.list_indexes(context.txn_id, table_id)?;
        let Some(index_id) = best_eq_lookup_index(&filter_indexes, target_column_id) else {
            return Ok(None);
        };
        let mode = if context.isolation == aiondb_tx::IsolationLevel::Serializable {
            LockMode::PredicateRead
        } else {
            LockMode::AccessShare
        };
        self.lock_table(context, table_id, mode)?;
        context.record_relation_read(table_id)?;

        let result = if is_max {
            self.storage_dml.index_max_single_column_value(
                context.txn_id,
                &context.snapshot,
                index_id,
            )
        } else {
            self.storage_dml.index_min_single_column_value(
                context.txn_id,
                &context.snapshot,
                index_id,
            )
        };
        match result {
            Ok(Some(value)) => Ok(Some(aiondb_eval::coerce_value(value, result_data_type)?)),
            Ok(None) => Ok(Some(Value::Null)),
            Err(error) if error.sqlstate() == SqlState::FeatureNotSupported => Ok(None),
            Err(error) => Err(error),
        }
    }

    fn try_group_by_count_via_index_counts(
        &self,
        context: &ExecutionContext,
        table_id: RelationId,
        group_by: &[TypedExpr],
        aggregates: &[ProjectionExpr],
        grouping_sets: &[Vec<usize>],
        having: Option<&TypedExpr>,
        filter: Option<&TypedExpr>,
        order_by: &[SortExpr],
        distinct: bool,
        access_path: &ScanAccessPath,
    ) -> DbResult<Option<Vec<Row>>> {
        if group_by.len() != 1
            || !grouping_sets.is_empty()
            || having.is_some()
            || filter.is_some()
            || distinct
            || !matches!(access_path, ScanAccessPath::SeqScan)
            || aggregates.is_empty()
        {
            return Ok(None);
        }

        let TypedExprKind::ColumnRef {
            ordinal: group_ordinal,
            ..
        } = &group_by[0].kind
        else {
            return Ok(None);
        };
        if !order_by.is_empty() {
            let [sort] = order_by else {
                return Ok(None);
            };
            let TypedExprKind::ColumnRef { ordinal, .. } = &sort.expr.kind else {
                return Ok(None);
            };
            if ordinal != group_ordinal || sort.descending {
                return Ok(None);
            }
        }

        let mut has_count = false;
        for projection in aggregates {
            match &projection.expr.kind {
                TypedExprKind::ColumnRef { ordinal, .. } if ordinal == group_ordinal => {}
                TypedExprKind::AggCount {
                    expr: None,
                    distinct: false,
                    filter: None,
                } => has_count = true,
                _ => return Ok(None),
            }
        }
        if !has_count {
            return Ok(None);
        }

        let Some(projected_columns) =
            self.table_column_ids_for_ordinals(context, table_id, &[*group_ordinal])?
        else {
            return Ok(None);
        };
        let Some(group_column_id) = projected_columns.first().copied() else {
            return Ok(None);
        };
        let Some(index) = self
            .catalog_reader
            .list_indexes(context.txn_id, table_id)?
            .into_iter()
            .find(|index| {
                index.kind == IndexKind::BTree
                    && index
                        .key_columns
                        .first()
                        .is_some_and(|key| key.column_id == group_column_id)
            })
        else {
            return Ok(None);
        };

        let mode = if context.isolation == aiondb_tx::IsolationLevel::Serializable {
            LockMode::PredicateRead
        } else {
            LockMode::AccessShare
        };
        self.lock_table(context, table_id, mode)?;
        context.record_relation_read(table_id)?;

        let full_key_range = KeyRange {
            lower: aiondb_storage_api::Bound::Unbounded,
            upper: aiondb_storage_api::Bound::Unbounded,
        };
        let exact_group_count_projection = if let [ProjectionExpr {
            expr:
                TypedExpr {
                    kind: TypedExprKind::ColumnRef { ordinal, .. },
                    ..
                },
            ..
        }, ProjectionExpr {
            expr:
                TypedExpr {
                    kind:
                        TypedExprKind::AggCount {
                            expr: None,
                            distinct: false,
                            filter: None,
                        },
                    ..
                },
            ..
        }] = aggregates
        {
            ordinal == group_ordinal
        } else {
            false
        };
        if exact_group_count_projection {
            return match self.storage_dml.visible_index_group_count_rows(
                context.txn_id,
                &context.snapshot,
                index.index_id,
                full_key_range,
            ) {
                Ok(rows) => Ok(Some(rows)),
                Err(error) if error.sqlstate() == SqlState::FeatureNotSupported => Ok(None),
                Err(error) => Err(error),
            };
        }

        let group_counts = match self.storage_dml.visible_index_group_counts(
            context.txn_id,
            &context.snapshot,
            index.index_id,
            full_key_range,
        ) {
            Ok(group_counts) => group_counts,
            Err(error) if error.sqlstate() == SqlState::FeatureNotSupported => return Ok(None),
            Err(error) => return Err(error),
        };

        let mut rows = Vec::with_capacity(group_counts.len());
        for (group, count) in group_counts {
            let count = Value::BigInt(i64::try_from(count).unwrap_or(i64::MAX));
            let mut values = Vec::with_capacity(aggregates.len());
            for projection in aggregates {
                match &projection.expr.kind {
                    TypedExprKind::ColumnRef { .. } => values.push(group.clone()),
                    TypedExprKind::AggCount { .. } => values.push(count.clone()),
                    _ => return Ok(None),
                }
            }
            rows.push(Row::new(values));
        }
        Ok(Some(rows))
    }

    pub(super) fn try_group_by_count_over_inner_hash_join(
        &self,
        context: &ExecutionContext,
        source: &PhysicalPlan,
        group_by: &[TypedExpr],
        aggregates: &[ProjectionExpr],
        grouping_sets: &[Vec<usize>],
        having: Option<&TypedExpr>,
        filter: Option<&TypedExpr>,
        order_by: &[SortExpr],
        distinct: bool,
    ) -> DbResult<Option<Vec<Row>>> {
        if group_by.len() != 1
            || !grouping_sets.is_empty()
            || having.is_some()
            || filter.is_some()
            || distinct
            || aggregates.is_empty()
        {
            return Ok(None);
        }
        let TypedExprKind::ColumnRef {
            ordinal: group_source_ordinal,
            ..
        } = &group_by[0].kind
        else {
            return Ok(None);
        };
        if !order_by.is_empty() {
            let [sort] = order_by else {
                return Ok(None);
            };
            let TypedExprKind::ColumnRef { ordinal, .. } = &sort.expr.kind else {
                return Ok(None);
            };
            let _ = ordinal;
            if sort.descending {
                return Ok(None);
            }
        }

        if let PhysicalPlan::NestedLoopIndexJoin {
            left,
            right_index_id,
            right_width,
            outer_key_ordinal,
            join_type,
            right_filter,
            residual,
            outputs,
            filter: source_filter,
            order_by: source_order_by,
            limit: source_limit,
            offset: source_offset,
            distinct: source_distinct,
            distinct_on: source_distinct_on,
            ..
        } = source
        {
            if matches!(join_type, JoinType::Inner)
                && right_filter.is_none()
                && residual.is_none()
                && source_filter.is_none()
                && source_order_by.is_empty()
                && source_limit.is_none()
                && source_offset.is_none()
                && !source_distinct
                && source_distinct_on.is_empty()
            {
                let raw_group_ordinal = if outputs.is_empty() {
                    *group_source_ordinal
                } else {
                    let Some(output) = outputs.get(*group_source_ordinal) else {
                        return Ok(None);
                    };
                    let TypedExprKind::ColumnRef { ordinal, .. } = output.expr.kind else {
                        return Ok(None);
                    };
                    ordinal
                };
                let left_width = self.join_child_width(left, context)?;
                if raw_group_ordinal < left_width && *outer_key_ordinal < left_width {
                    let mut groups: std::collections::HashMap<ValueHashKey, (Value, u64)> =
                        std::collections::HashMap::new();
                    self.for_each_join_child_row(left, context, &mut |left_row| {
                        context.check_deadline()?;
                        let Some(outer_value) = left_row.values.get(*outer_key_ordinal) else {
                            return Err(DbError::internal(
                                "nested-loop index join outer key ordinal out of bounds",
                            ));
                        };
                        if matches!(outer_value, Value::Null) {
                            return Ok(true);
                        }
                        let count = self.storage_dml.visible_index_row_count(
                            context.txn_id,
                            &context.snapshot,
                            *right_index_id,
                            exact_lookup_key_range(outer_value),
                        )?;
                        if count == 0 {
                            return Ok(true);
                        }
                        let Some(group_value) = left_row.values.get(raw_group_ordinal).cloned()
                        else {
                            return Err(DbError::internal(
                                "aggregate nested-loop group ordinal out of left row bounds",
                            ));
                        };
                        let hash_key = build_hash_key(&group_value)?;
                        let entry = groups.entry(hash_key).or_insert((group_value, 0));
                        entry.1 = entry.1.saturating_add(count);
                        Ok(true)
                    })?;

                    let mut rows = Vec::with_capacity(groups.len());
                    for (_key, (group, count)) in groups {
                        let count = Value::BigInt(i64::try_from(count).unwrap_or(i64::MAX));
                        let mut values = Vec::with_capacity(aggregates.len());
                        for projection in aggregates {
                            match &projection.expr.kind {
                                TypedExprKind::ColumnRef { ordinal, .. }
                                    if ordinal == group_source_ordinal =>
                                {
                                    values.push(group.clone());
                                }
                                TypedExprKind::AggCount {
                                    expr: None,
                                    distinct: false,
                                    filter: None,
                                } => values.push(count.clone()),
                                _ => return Ok(None),
                            }
                        }
                        rows.push(Row::new(values));
                    }
                    if !order_by.is_empty() {
                        let group_output_ordinal = group_projection_output_ordinal(
                            aggregates,
                            *group_source_ordinal,
                            raw_group_ordinal,
                        )
                        .unwrap_or(0);
                        rows.sort_by(|left, right| {
                            for sort in order_by {
                                let TypedExprKind::ColumnRef { ordinal, .. } = &sort.expr.kind
                                else {
                                    return Ordering::Equal;
                                };
                                let row_ordinal = if *ordinal == *group_source_ordinal
                                    || *ordinal == raw_group_ordinal
                                {
                                    group_output_ordinal
                                } else {
                                    *ordinal
                                };
                                let left_value =
                                    left.values.get(row_ordinal).unwrap_or(&Value::Null);
                                let right_value =
                                    right.values.get(row_ordinal).unwrap_or(&Value::Null);
                                match compare_sort_values(
                                    left_value,
                                    right_value,
                                    sort.descending,
                                    sort.nulls_first,
                                ) {
                                    Ok(Ordering::Equal) => {}
                                    Ok(ordering) => return ordering,
                                    Err(_) => return Ordering::Equal,
                                }
                            }
                            Ordering::Equal
                        });
                    }
                    let _ = right_width;
                    return Ok(Some(rows));
                }
            }
        }

        let (
            left,
            right,
            join_type,
            left_keys,
            right_keys,
            condition,
            source_outputs,
            source_filter,
            source_order_by,
            source_limit,
            source_offset,
            source_distinct,
            source_distinct_on,
        ) = match source {
            PhysicalPlan::HashJoin {
                left,
                right,
                join_type,
                left_keys,
                right_keys,
                condition,
                outputs,
                filter,
                order_by,
                limit,
                offset,
                distinct,
                distinct_on,
            } => (
                left.as_ref(),
                right.as_ref(),
                join_type,
                left_keys.as_slice(),
                right_keys.as_slice(),
                condition.as_ref(),
                outputs.as_slice(),
                filter.as_ref(),
                order_by.as_slice(),
                limit.as_ref(),
                offset.as_ref(),
                *distinct,
                distinct_on.as_slice(),
            ),
            _ => return Ok(None),
        };

        if !matches!(join_type, JoinType::Inner)
            || condition.is_some()
            || source_filter.is_some()
            || !source_order_by.is_empty()
            || source_limit.is_some()
            || source_offset.is_some()
            || source_distinct
            || !source_distinct_on.is_empty()
        {
            return Ok(None);
        }

        let raw_group_ordinal = if source_outputs.is_empty() {
            *group_source_ordinal
        } else {
            let Some(output) = source_outputs.get(*group_source_ordinal) else {
                return Ok(None);
            };
            let TypedExprKind::ColumnRef { ordinal, .. } = output.expr.kind else {
                return Ok(None);
            };
            ordinal
        };

        if let Some(rows) = self.try_group_by_count_over_seqscan_join_index_counts(
            context,
            left,
            right,
            left_keys,
            right_keys,
            raw_group_ordinal,
            *group_source_ordinal,
            group_by,
            aggregates,
            order_by,
        )? {
            return Ok(Some(rows));
        }

        let left_width = self.join_child_width(left, context)?;
        let right_width = self.join_child_width(right, context)?;
        let combined_width = left_width.saturating_add(right_width);
        if raw_group_ordinal >= combined_width {
            return Ok(None);
        }

        let (right_rows, _) = self.materialize_join_child(right, context)?;
        let mut groups: std::collections::HashMap<ValueHashKey, (Value, u64)> =
            std::collections::HashMap::new();

        if raw_group_ordinal < left_width {
            let mut right_counts: std::collections::HashMap<JoinHashKey, u64, JoinFxBuildHasher> =
                std::collections::HashMap::with_hasher(JoinFxBuildHasher::default());
            for right_row in &right_rows {
                if let Some(key) = build_hash_join_key(right_row, right_keys)? {
                    *right_counts.entry(key).or_insert(0) += 1;
                }
            }
            self.for_each_join_child_row(left, context, &mut |left_row| {
                context.check_deadline()?;
                let Some(join_key) = build_hash_join_key(&left_row, left_keys)? else {
                    return Ok(true);
                };
                let Some(count) = right_counts.get(&join_key).copied() else {
                    return Ok(true);
                };
                let Some(group_value) = left_row.values.get(raw_group_ordinal).cloned() else {
                    return Err(DbError::internal(
                        "aggregate hash join group ordinal out of left row bounds",
                    ));
                };
                let hash_key = build_hash_key(&group_value)?;
                let entry = groups.entry(hash_key).or_insert((group_value, 0));
                entry.1 = entry.1.saturating_add(count);
                Ok(true)
            })?;
        } else {
            let right_group_ordinal = raw_group_ordinal - left_width;
            let mut right_groups: std::collections::HashMap<
                JoinHashKey,
                Vec<(Value, u64)>,
                JoinFxBuildHasher,
            > = std::collections::HashMap::with_hasher(JoinFxBuildHasher::default());
            for right_row in &right_rows {
                let Some(join_key) = build_hash_join_key(right_row, right_keys)? else {
                    continue;
                };
                let Some(group_value) = right_row.values.get(right_group_ordinal).cloned() else {
                    return Err(DbError::internal(
                        "aggregate hash join group ordinal out of right row bounds",
                    ));
                };
                let per_key_groups = right_groups.entry(join_key).or_default();
                if let Some((_, count)) = per_key_groups.iter_mut().find(|(existing, _)| {
                    compare_runtime_values(existing, &group_value)
                        .ok()
                        .flatten()
                        == Some(Ordering::Equal)
                }) {
                    *count = count.saturating_add(1);
                } else {
                    per_key_groups.push((group_value, 1));
                }
            }
            self.for_each_join_child_row(left, context, &mut |left_row| {
                context.check_deadline()?;
                let Some(join_key) = build_hash_join_key(&left_row, left_keys)? else {
                    return Ok(true);
                };
                let Some(per_key_groups) = right_groups.get(&join_key) else {
                    return Ok(true);
                };
                for (group_value, count) in per_key_groups {
                    let hash_key = build_hash_key(group_value)?;
                    let entry = groups.entry(hash_key).or_insert((group_value.clone(), 0));
                    entry.1 = entry.1.saturating_add(*count);
                }
                Ok(true)
            })?;
        }

        let mut rows = Vec::with_capacity(groups.len());
        for (_key, (group, count)) in groups {
            let count = Value::BigInt(i64::try_from(count).unwrap_or(i64::MAX));
            let mut values = Vec::with_capacity(aggregates.len());
            for projection in aggregates {
                match &projection.expr.kind {
                    TypedExprKind::ColumnRef { ordinal, .. }
                        if *ordinal == *group_source_ordinal || *ordinal == raw_group_ordinal =>
                    {
                        values.push(group.clone());
                    }
                    TypedExprKind::AggCount {
                        expr: None,
                        distinct: false,
                        filter: None,
                    } => values.push(count.clone()),
                    _ => return Ok(None),
                }
            }
            rows.push(Row::new(values));
        }

        if !order_by.is_empty() {
            let group_output_ordinal = group_projection_output_ordinal(
                aggregates,
                *group_source_ordinal,
                raw_group_ordinal,
            )
            .unwrap_or(0);
            rows.sort_by(|left, right| {
                for sort in order_by {
                    let TypedExprKind::ColumnRef { ordinal, .. } = &sort.expr.kind else {
                        return Ordering::Equal;
                    };
                    let row_ordinal =
                        if *ordinal == *group_source_ordinal || *ordinal == raw_group_ordinal {
                            group_output_ordinal
                        } else {
                            *ordinal
                        };
                    let left_value = left.values.get(row_ordinal).unwrap_or(&Value::Null);
                    let right_value = right.values.get(row_ordinal).unwrap_or(&Value::Null);
                    match compare_sort_values(
                        left_value,
                        right_value,
                        sort.descending,
                        sort.nulls_first,
                    ) {
                        Ok(Ordering::Equal) => {}
                        Ok(ordering) => return ordering,
                        Err(_) => return Ordering::Equal,
                    }
                }
                Ordering::Equal
            });
        }
        Ok(Some(rows))
    }

    fn try_group_by_count_over_seqscan_join_index_counts(
        &self,
        context: &ExecutionContext,
        left: &PhysicalPlan,
        right: &PhysicalPlan,
        left_keys: &[usize],
        right_keys: &[usize],
        raw_group_ordinal: usize,
        group_source_ordinal: usize,
        group_by: &[TypedExpr],
        aggregates: &[ProjectionExpr],
        order_by: &[SortExpr],
    ) -> DbResult<Option<Vec<Row>>> {
        let ([left_key_ordinal], [right_key_ordinal]) = (left_keys, right_keys) else {
            return Ok(None);
        };
        if group_by.len() != 1 {
            return Ok(None);
        }
        let TypedExprKind::ColumnRef {
            ordinal: group_ordinal,
            ..
        } = &group_by[0].kind
        else {
            return Ok(None);
        };
        if *group_ordinal != group_source_ordinal && *group_ordinal != raw_group_ordinal {
            return Ok(None);
        }

        let left_width = self.join_child_width(left, context)?;
        if raw_group_ordinal >= left_width {
            return Ok(None);
        }
        let Some((left_table_id, left_key_table_ordinal)) =
            simple_scan_output_column(left, *left_key_ordinal)
        else {
            return Ok(None);
        };
        let Some((left_group_table_id, left_group_table_ordinal)) =
            simple_scan_output_column(left, raw_group_ordinal)
        else {
            return Ok(None);
        };
        if left_group_table_id != left_table_id {
            return Ok(None);
        }
        let Some((right_table_id, right_key_table_ordinal)) =
            simple_scan_output_column(right, *right_key_ordinal)
        else {
            return Ok(None);
        };

        let left_table = self
            .catalog_reader
            .get_table_by_id(context.txn_id, left_table_id)?
            .ok_or_else(|| DbError::internal("left join table not found"))?;
        let right_table = self
            .catalog_reader
            .get_table_by_id(context.txn_id, right_table_id)?
            .ok_or_else(|| DbError::internal("right join table not found"))?;
        if self
            .compile_compat_rls_policies(
                &left_table,
                super::dml_plans::CompatRlsAction::Select,
                context,
            )?
            .is_some()
            || self
                .compile_compat_rls_policies(
                    &right_table,
                    super::dml_plans::CompatRlsAction::Select,
                    context,
                )?
                .is_some()
        {
            return Ok(None);
        }

        let Some(left_key_column) = left_table.columns.get(left_key_table_ordinal) else {
            return Ok(None);
        };
        let Some(left_group_column) = left_table.columns.get(left_group_table_ordinal) else {
            return Ok(None);
        };
        let Some(right_key_column) = right_table.columns.get(right_key_table_ordinal) else {
            return Ok(None);
        };

        let mode = if context.isolation == aiondb_tx::IsolationLevel::Serializable {
            LockMode::PredicateRead
        } else {
            LockMode::AccessShare
        };
        self.lock_table(context, right_table_id, mode)?;
        context.record_relation_read(right_table_id)?;

        let Some(right_index) = self
            .catalog_reader
            .list_indexes(context.txn_id, right_table_id)?
            .into_iter()
            .find(|index| {
                index.kind == IndexKind::BTree
                    && index
                        .key_columns
                        .first()
                        .is_some_and(|key| key.column_id == right_key_column.column_id)
            })
        else {
            return Ok(None);
        };
        let right_group_counts = match self.storage_dml.visible_index_group_counts(
            context.txn_id,
            &context.snapshot,
            right_index.index_id,
            KeyRange {
                lower: aiondb_storage_api::Bound::Unbounded,
                upper: aiondb_storage_api::Bound::Unbounded,
            },
        ) {
            Ok(group_counts) => group_counts,
            Err(error) if error.sqlstate() == SqlState::FeatureNotSupported => return Ok(None),
            Err(error) => return Err(error),
        };
        let mut right_counts: std::collections::HashMap<ValueHashKey, u64> =
            std::collections::HashMap::with_capacity(right_group_counts.len());
        for (join_value, count) in right_group_counts {
            right_counts.insert(build_hash_key(&join_value)?, count);
        }

        let mut stream = self.scan_table_locked(
            context,
            left_table_id,
            Some(vec![left_key_column.column_id, left_group_column.column_id]),
        )?;
        let mut groups: std::collections::HashMap<ValueHashKey, (Value, u64)> =
            std::collections::HashMap::new();
        while let Some(record) = stream.next()? {
            context.check_deadline()?;
            let Some(join_value) = record.row.values.first() else {
                continue;
            };
            let Some(group_value) = record.row.values.get(1).cloned() else {
                continue;
            };
            let join_hash = build_hash_key(join_value)?;
            let Some(count) = right_counts.get(&join_hash).copied() else {
                continue;
            };
            if count == 0 {
                continue;
            }
            let hash_key = build_hash_key(&group_value)?;
            let entry = groups.entry(hash_key).or_insert((group_value, 0));
            entry.1 = entry.1.saturating_add(count);
        }

        let mut rows = Vec::with_capacity(groups.len());
        for (_key, (group, count)) in groups {
            let count = Value::BigInt(i64::try_from(count).unwrap_or(i64::MAX));
            let mut values = Vec::with_capacity(aggregates.len());
            for projection in aggregates {
                match &projection.expr.kind {
                    TypedExprKind::ColumnRef { ordinal, .. }
                        if *ordinal == group_source_ordinal || *ordinal == raw_group_ordinal =>
                    {
                        values.push(group.clone());
                    }
                    TypedExprKind::AggCount {
                        expr: None,
                        distinct: false,
                        filter: None,
                    } => values.push(count.clone()),
                    _ => return Ok(None),
                }
            }
            rows.push(Row::new(values));
        }

        if !order_by.is_empty() {
            let group_output_ordinal = group_projection_output_ordinal(
                aggregates,
                group_source_ordinal,
                raw_group_ordinal,
            )
            .unwrap_or(0);
            rows.sort_by(|left, right| {
                for sort in order_by {
                    let TypedExprKind::ColumnRef { ordinal, .. } = &sort.expr.kind else {
                        return Ordering::Equal;
                    };
                    let row_ordinal =
                        if *ordinal == group_source_ordinal || *ordinal == raw_group_ordinal {
                            group_output_ordinal
                        } else {
                            *ordinal
                        };
                    let left_value = left.values.get(row_ordinal).unwrap_or(&Value::Null);
                    let right_value = right.values.get(row_ordinal).unwrap_or(&Value::Null);
                    match compare_sort_values(
                        left_value,
                        right_value,
                        sort.descending,
                        sort.nulls_first,
                    ) {
                        Ok(Ordering::Equal) => {}
                        Ok(ordering) => return ordering,
                        Err(_) => return Ordering::Equal,
                    }
                }
                Ordering::Equal
            });
        }
        Ok(Some(rows))
    }

    fn try_simple_group_aggregate(
        &self,
        plan: &PhysicalPlan,
        context: &ExecutionContext,
        table_id: RelationId,
        group_by: &[TypedExpr],
        grouping_sets: &[Vec<usize>],
        aggregates: &[ProjectionExpr],
        having: Option<&TypedExpr>,
        filter: Option<&TypedExpr>,
        order_by: &[SortExpr],
        limit: Option<&TypedExpr>,
        offset: Option<&TypedExpr>,
        distinct: bool,
        access_path: &ScanAccessPath,
    ) -> DbResult<Option<ExecutionResult>> {
        if group_by.is_empty()
            || !grouping_sets.is_empty()
            || having.is_some()
            || distinct
            || aggregates.is_empty()
        {
            return Ok(None);
        }

        let Some(simple_filter) = extract_simple_group_filter(filter) else {
            return Ok(None);
        };
        let Some(order_column_indices) = simple_group_order_column_indices(aggregates, order_by)
        else {
            return Ok(None);
        };
        let Some(table) = self
            .catalog_reader
            .get_table_by_id(context.txn_id, table_id)?
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
        for group_expr in group_by {
            let Some(ordinal) = simple_column_ordinal(group_expr) else {
                return Ok(None);
            };
            group_ordinals.push(ordinal);
        }

        let mut required_ordinals = group_ordinals.clone();
        let add_required_ordinal = |required_ordinals: &mut Vec<usize>, ordinal: usize| {
            if !required_ordinals.contains(&ordinal) {
                required_ordinals.push(ordinal);
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
                    output_plan.push(SimpleGroupOutput::GroupKey { group_index });
                }
                TypedExprKind::AggCount {
                    expr: None,
                    distinct: false,
                    filter: None,
                } => output_plan.push(SimpleGroupOutput::CountStar),
                TypedExprKind::AggCount {
                    expr: Some(expr),
                    distinct: true,
                    filter: None,
                } => {
                    let Some(ordinal) = simple_column_ordinal(expr) else {
                        return Ok(None);
                    };
                    add_required_ordinal(&mut required_ordinals, ordinal);
                    let Some(projected_pos) = projected_position(&required_ordinals, ordinal)
                    else {
                        return Ok(None);
                    };
                    output_plan.push(SimpleGroupOutput::CountDistinct { projected_pos });
                }
                TypedExprKind::AggSum {
                    expr,
                    distinct: false,
                    filter: None,
                } => {
                    let Some(ordinal) = simple_column_ordinal(expr) else {
                        return Ok(None);
                    };
                    add_required_ordinal(&mut required_ordinals, ordinal);
                    let Some(projected_pos) = projected_position(&required_ordinals, ordinal)
                    else {
                        return Ok(None);
                    };
                    output_plan.push(SimpleGroupOutput::Sum { projected_pos });
                }
                TypedExprKind::AggAvg {
                    expr,
                    distinct: false,
                    filter: None,
                } => {
                    let Some(ordinal) = simple_column_ordinal(expr) else {
                        return Ok(None);
                    };
                    add_required_ordinal(&mut required_ordinals, ordinal);
                    let Some(projected_pos) = projected_position(&required_ordinals, ordinal)
                    else {
                        return Ok(None);
                    };
                    output_plan.push(SimpleGroupOutput::Avg { projected_pos });
                }
                // MIN / MAX: same fast-path shape as SUM/AVG —
                // record the projected ordinal and update the
                // per-group running extremum from the streamed
                // tuple values, bypassing ExpressionEvaluator.
                // Bench: `UPDATE … SET v = (SELECT max(b) FROM
                // bonus WHERE bonus.grp = t.grp)` over 200k inner
                // rows lifted from 12k → ~2× more rows/s by keeping
                // the materialise step on this hot path instead of
                // falling through to the generic
                // `execute_aggregate_or_set_plan` loop.
                TypedExprKind::AggMin { expr, filter: None } => {
                    let Some(ordinal) = simple_column_ordinal(expr) else {
                        return Ok(None);
                    };
                    add_required_ordinal(&mut required_ordinals, ordinal);
                    let Some(projected_pos) = projected_position(&required_ordinals, ordinal)
                    else {
                        return Ok(None);
                    };
                    output_plan.push(SimpleGroupOutput::Min { projected_pos });
                }
                TypedExprKind::AggMax { expr, filter: None } => {
                    let Some(ordinal) = simple_column_ordinal(expr) else {
                        return Ok(None);
                    };
                    add_required_ordinal(&mut required_ordinals, ordinal);
                    let Some(projected_pos) = projected_position(&required_ordinals, ordinal)
                    else {
                        return Ok(None);
                    };
                    output_plan.push(SimpleGroupOutput::Max { projected_pos });
                }
                _ => return Ok(None),
            }
        }

        let group_positions = group_ordinals
            .iter()
            .map(|ordinal| {
                projected_position(&required_ordinals, *ordinal).ok_or_else(|| {
                    DbError::internal("failed to map GROUP BY ordinal into pushed projection")
                })
            })
            .collect::<DbResult<Vec<_>>>()?;
        let filter_position = simple_filter
            .as_ref()
            .map(|filter| {
                projected_position(&required_ordinals, filter.column_ordinal).ok_or_else(|| {
                    DbError::internal(
                        "failed to map aggregate filter ordinal into pushed projection",
                    )
                })
            })
            .transpose()?;
        let projected_column_ids = self
            .table_column_ids_for_ordinals(context, table_id, &required_ordinals)?
            .ok_or_else(|| DbError::internal("failed to map aggregate projection columns"))?;

        let mut stream = match self.resolve_scan_stream(
            context,
            table_id,
            access_path,
            Some(projected_column_ids),
        ) {
            Ok(stream) => stream,
            Err(error) => {
                if aiondb_planner::is_virtual_synthetic_relation(table_id.get()) {
                    Box::new(VecTupleStream::new(Vec::new()))
                } else {
                    return Err(error);
                }
            }
        };

        // Specialized hot loop for `(Int/BigInt group, Int/BigInt
        // agg)` shapes — bypasses `build_hash_key` (single i64 key,
        // no Vec alloc) and the generic `Value` enum dispatch in
        // `compare_runtime_values` / `agg_add_value` for MIN/MAX/SUM.
        // Decorrelated `SELECT max(int_col) FROM s GROUP BY int_col`
        // patterns (the BENCH_SCALAR_AGG_SUBQ shape) hit this path
        // and run at native HashMap+i64 speed instead of paying the
        // ~350 ns/row enum-dispatch tax of the generic loop.
        let has_interrupts_pre = context.has_execution_interrupts();
        if let Some(rows) = self.try_simple_group_aggregate_int_fast(
            plan,
            context,
            &table,
            &required_ordinals,
            &group_positions,
            &output_plan,
            simple_filter.as_ref(),
            filter_position,
            order_by,
            limit,
            offset,
            aggregates,
            order_column_indices.as_slice(),
            &mut stream,
            has_interrupts_pre,
        )? {
            return Ok(Some(rows));
        }

        let mut groups: std::collections::HashMap<Vec<ValueHashKey>, usize> =
            std::collections::HashMap::new();
        let mut ordered_groups: Vec<SimpleGroupState> = Vec::new();
        let mut group_key_scratch: Vec<ValueHashKey> = Vec::with_capacity(group_positions.len());
        let output_count = output_plan.len();
        let has_interrupts = context.has_execution_interrupts();
        let mut scanned_rows = 0usize;

        while let Some(record) = stream.next()? {
            if has_interrupts && scanned_rows.is_multiple_of(256) {
                context.check_deadline()?;
            }
            scanned_rows = scanned_rows.wrapping_add(1);

            if let (Some(filter), Some(filter_position)) = (&simple_filter, filter_position) {
                let filter_value = record
                    .row
                    .values
                    .get(filter_position)
                    .unwrap_or(&Value::Null);
                if !simple_group_filter_matches(filter_value, filter)? {
                    continue;
                }
            }

            group_key_scratch.clear();
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
                ordered_groups.push(SimpleGroupState::new(group_values, output_count));
                groups.insert(group_key_scratch.clone(), group_idx);
                group_idx
            };
            let group = ordered_groups.get_mut(group_idx).ok_or_else(|| {
                DbError::internal("missing simple aggregate group during evaluation")
            })?;

            for (output_idx, output) in output_plan.iter().enumerate() {
                match *output {
                    SimpleGroupOutput::GroupKey { .. } => {}
                    SimpleGroupOutput::CountStar => {
                        group.counts[output_idx] = group.counts[output_idx].saturating_add(1);
                    }
                    SimpleGroupOutput::CountDistinct { projected_pos } => {
                        let value = record.row.values.get(projected_pos).unwrap_or(&Value::Null);
                        if !value.is_null() {
                            let key = build_hash_key(value)?;
                            let distinct = group.distincts[output_idx]
                                .get_or_insert_with(std::collections::HashSet::new);
                            if distinct.insert(key) {
                                context.track_memory(16)?;
                                group.counts[output_idx] =
                                    group.counts[output_idx].saturating_add(1);
                            }
                        }
                    }
                    SimpleGroupOutput::Sum { projected_pos }
                    | SimpleGroupOutput::Avg { projected_pos } => {
                        let value = record.row.values.get(projected_pos).unwrap_or(&Value::Null);
                        if !value.is_null() {
                            group.counts[output_idx] = group.counts[output_idx].saturating_add(1);
                            group.sums[output_idx] =
                                Some(agg_add_value(group.sums[output_idx].take(), value)?);
                        }
                    }
                    SimpleGroupOutput::Min { projected_pos } => {
                        let value = record.row.values.get(projected_pos).unwrap_or(&Value::Null);
                        if !value.is_null() {
                            group.counts[output_idx] = group.counts[output_idx].saturating_add(1);
                            let take_new = match group.sums[output_idx].as_ref() {
                                None => true,
                                Some(current) => matches!(
                                    compare_runtime_values(value, current)?,
                                    Some(std::cmp::Ordering::Less)
                                ),
                            };
                            if take_new {
                                group.sums[output_idx] = Some(value.clone());
                            }
                        }
                    }
                    SimpleGroupOutput::Max { projected_pos } => {
                        let value = record.row.values.get(projected_pos).unwrap_or(&Value::Null);
                        if !value.is_null() {
                            group.counts[output_idx] = group.counts[output_idx].saturating_add(1);
                            let take_new = match group.sums[output_idx].as_ref() {
                                None => true,
                                Some(current) => matches!(
                                    compare_runtime_values(value, current)?,
                                    Some(std::cmp::Ordering::Greater)
                                ),
                            };
                            if take_new {
                                group.sums[output_idx] = Some(value.clone());
                            }
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
        for group in &ordered_groups {
            context.check_deadline()?;
            if usize_to_u64(rows.len()) >= context.max_result_rows {
                return Err(DbError::program_limit(
                    "maximum number of result rows reached",
                ));
            }
            let mut values = Vec::with_capacity(output_plan.len());
            for (output_idx, output) in output_plan.iter().enumerate() {
                let value = match *output {
                    SimpleGroupOutput::GroupKey { group_index } => group
                        .group_values
                        .get(group_index)
                        .cloned()
                        .unwrap_or(Value::Null),
                    SimpleGroupOutput::CountStar | SimpleGroupOutput::CountDistinct { .. } => {
                        Value::BigInt(group.counts[output_idx])
                    }
                    SimpleGroupOutput::Sum { .. } | SimpleGroupOutput::Avg { .. } => {
                        let mut acc = AggAccumulator::new(false);
                        acc.count = group.counts[output_idx];
                        acc.sum = group.sums[output_idx].clone();
                        finalize_accumulator(
                            &acc,
                            &agg_templates[output_idx],
                            &self.evaluator,
                            context,
                        )?
                    }
                    SimpleGroupOutput::Min { .. } | SimpleGroupOutput::Max { .. } => {
                        // The running extremum is stored verbatim in
                        // `group.sums`; an empty group leaves it as
                        // `None`, which projects as SQL NULL.
                        group.sums[output_idx].clone().unwrap_or(Value::Null)
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
            .map(|e| eval_limit_offset_expr(&self.evaluator, e, "OFFSET"))
            .transpose()?
            .unwrap_or(0);
        if offset_val > 0 {
            let skip = clamp_u64_to_usize(offset_val, rows.len());
            rows.drain(..skip);
        }

        let plan_limit = limit
            .map(|e| eval_limit_offset_expr(&self.evaluator, e, "LIMIT"))
            .transpose()?;
        if let Some(limit) = effective_collect_limit(plan_limit, context.collect_row_limit) {
            rows.truncate(clamp_u64_to_usize(limit, rows.len()));
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

    /// Specialized hash-aggregate hot loop for the common case of a
    /// single Int/BigInt GROUP BY column with aggregates that are
    /// some combination of `GroupKey`, `CountStar`, `Sum`, `Min`,
    /// `Max` over Int/BigInt columns. Bypasses `build_hash_key`,
    /// `compare_runtime_values`, and `agg_add_value` — three layers
    /// of `Value` enum dispatch — by keeping every per-row scalar in
    /// a native `i64` and using `HashMap<i64, _>` directly.
    ///
    /// Returns `None` if any precondition isn't met (multi-column
    /// group, non-int columns, unsupported aggregate kinds, …); the
    /// caller falls back to the generic `Vec<ValueHashKey>` path.
    #[allow(clippy::too_many_arguments)]
    fn try_simple_group_aggregate_int_fast(
        &self,
        plan: &PhysicalPlan,
        context: &ExecutionContext,
        table: &aiondb_catalog::TableDescriptor,
        required_ordinals: &[usize],
        group_positions: &[usize],
        output_plan: &[SimpleGroupOutput],
        simple_filter: Option<&SimpleGroupFilter>,
        filter_position: Option<usize>,
        order_by: &[SortExpr],
        limit: Option<&TypedExpr>,
        offset: Option<&TypedExpr>,
        aggregates: &[ProjectionExpr],
        order_column_indices: &[Option<usize>],
        stream: &mut Box<dyn aiondb_storage_api::TupleStream>,
        has_interrupts: bool,
    ) -> DbResult<Option<ExecutionResult>> {
        // Pre-conditions for the specialised path.
        if group_positions.len() != 1 {
            return Ok(None);
        }
        // The group column at table ordinal `required_ordinals[
        // group_positions[0]]` must be Int / BigInt. The projected
        // tuple delivers it at `group_positions[0]`.
        let group_table_ord = match required_ordinals.get(group_positions[0]) {
            Some(o) => *o,
            None => return Ok(None),
        };
        let group_col = match table.columns.get(group_table_ord) {
            Some(c) => c,
            None => return Ok(None),
        };
        if !matches!(
            group_col.data_type,
            aiondb_core::DataType::Int | aiondb_core::DataType::BigInt
        ) {
            return Ok(None);
        }

        // Walk output_plan; collect per-aggregate metadata. Bail
        // for any aggregate kind we don't yet specialise.
        #[derive(Clone, Copy)]
        enum FastSlot {
            GroupKey,
            CountStar,
            Sum { proj_pos: usize },
            Min { proj_pos: usize },
            Max { proj_pos: usize },
        }
        let mut slots = Vec::with_capacity(output_plan.len());
        for out in output_plan {
            let slot = match *out {
                SimpleGroupOutput::GroupKey { .. } => FastSlot::GroupKey,
                SimpleGroupOutput::CountStar => FastSlot::CountStar,
                SimpleGroupOutput::Sum { projected_pos } => {
                    let table_ord = match required_ordinals.get(projected_pos) {
                        Some(o) => *o,
                        None => return Ok(None),
                    };
                    let col = match table.columns.get(table_ord) {
                        Some(c) => c,
                        None => return Ok(None),
                    };
                    if !matches!(
                        col.data_type,
                        aiondb_core::DataType::Int | aiondb_core::DataType::BigInt
                    ) {
                        return Ok(None);
                    }
                    FastSlot::Sum {
                        proj_pos: projected_pos,
                    }
                }
                SimpleGroupOutput::Min { projected_pos } => {
                    let table_ord = match required_ordinals.get(projected_pos) {
                        Some(o) => *o,
                        None => return Ok(None),
                    };
                    let col = match table.columns.get(table_ord) {
                        Some(c) => c,
                        None => return Ok(None),
                    };
                    if !matches!(
                        col.data_type,
                        aiondb_core::DataType::Int | aiondb_core::DataType::BigInt
                    ) {
                        return Ok(None);
                    }
                    FastSlot::Min {
                        proj_pos: projected_pos,
                    }
                }
                SimpleGroupOutput::Max { projected_pos } => {
                    let table_ord = match required_ordinals.get(projected_pos) {
                        Some(o) => *o,
                        None => return Ok(None),
                    };
                    let col = match table.columns.get(table_ord) {
                        Some(c) => c,
                        None => return Ok(None),
                    };
                    if !matches!(
                        col.data_type,
                        aiondb_core::DataType::Int | aiondb_core::DataType::BigInt
                    ) {
                        return Ok(None);
                    }
                    FastSlot::Max {
                        proj_pos: projected_pos,
                    }
                }
                // Avg needs sum + count finalize via the generic
                // path; CountDistinct needs a HashSet. Bail.
                SimpleGroupOutput::Avg { .. } | SimpleGroupOutput::CountDistinct { .. } => {
                    return Ok(None);
                }
            };
            slots.push(slot);
        }

        // Per-group state, indexed alongside `slots`.
        struct GroupAcc {
            group_key: i64,
            counts: Vec<i64>,
            sums: Vec<i64>,
            // For Min/Max: tracks whether the slot has any non-null
            // contribution yet. SQL semantics demand that an
            // empty-input MIN/MAX yields NULL, not 0.
            seen: Vec<bool>,
        }
        let group_pos = group_positions[0];
        let mut groups: std::collections::HashMap<i64, usize> = std::collections::HashMap::new();
        let mut ordered_groups: Vec<GroupAcc> = Vec::new();
        let slot_count = slots.len();

        // Helper: extract `i64` from a Value::Int / Value::BigInt or
        // return None on NULL or unexpected type. Falling back to
        // None on type-mismatch keeps the contract safe even if the
        // pre-loop type check missed something subtle.
        #[inline]
        fn as_i64(value: &Value) -> Option<i64> {
            match value {
                Value::Int(v) => Some(i64::from(*v)),
                Value::BigInt(v) => Some(*v),
                _ => None,
            }
        }

        let mut scanned_rows = 0usize;
        while let Some(record) = stream.next()? {
            if has_interrupts && scanned_rows.is_multiple_of(256) {
                context.check_deadline()?;
            }
            scanned_rows = scanned_rows.wrapping_add(1);

            if let (Some(filter), Some(filter_position)) = (simple_filter, filter_position) {
                let filter_value = record
                    .row
                    .values
                    .get(filter_position)
                    .unwrap_or(&Value::Null);
                if !simple_group_filter_matches(filter_value, filter)? {
                    continue;
                }
            }

            let group_value = record.row.values.get(group_pos).unwrap_or(&Value::Null);
            let Some(group_key) = as_i64(group_value) else {
                // NULL group key -- SQL excludes these from the
                // result of `GROUP BY` over a single non-grouping-set
                // column when the parent treats NULL groups as no
                // match (e.g. our decorrelated-correlation
                // materialisation). The generic path keeps NULL
                // groups; bail to it so we don't change observable
                // semantics for direct user GROUP BY queries.
                return Ok(None);
            };

            let group_idx = if let Some(&idx) = groups.get(&group_key) {
                idx
            } else {
                let idx = ordered_groups.len();
                ordered_groups.push(GroupAcc {
                    group_key,
                    counts: vec![0; slot_count],
                    sums: vec![0; slot_count],
                    seen: vec![false; slot_count],
                });
                groups.insert(group_key, idx);
                idx
            };
            let group = &mut ordered_groups[group_idx];

            for (slot_idx, slot) in slots.iter().enumerate() {
                match *slot {
                    FastSlot::GroupKey => {}
                    FastSlot::CountStar => {
                        group.counts[slot_idx] = group.counts[slot_idx].wrapping_add(1);
                    }
                    FastSlot::Sum { proj_pos } => {
                        let v = record.row.values.get(proj_pos).unwrap_or(&Value::Null);
                        if let Some(x) = as_i64(v) {
                            group.counts[slot_idx] = group.counts[slot_idx].wrapping_add(1);
                            group.sums[slot_idx] = group.sums[slot_idx].wrapping_add(x);
                            group.seen[slot_idx] = true;
                        }
                    }
                    FastSlot::Min { proj_pos } => {
                        let v = record.row.values.get(proj_pos).unwrap_or(&Value::Null);
                        if let Some(x) = as_i64(v) {
                            if !group.seen[slot_idx] || x < group.sums[slot_idx] {
                                group.sums[slot_idx] = x;
                            }
                            group.counts[slot_idx] = group.counts[slot_idx].wrapping_add(1);
                            group.seen[slot_idx] = true;
                        }
                    }
                    FastSlot::Max { proj_pos } => {
                        let v = record.row.values.get(proj_pos).unwrap_or(&Value::Null);
                        if let Some(x) = as_i64(v) {
                            if !group.seen[slot_idx] || x > group.sums[slot_idx] {
                                group.sums[slot_idx] = x;
                            }
                            group.counts[slot_idx] = group.counts[slot_idx].wrapping_add(1);
                            group.seen[slot_idx] = true;
                        }
                    }
                }
            }
        }

        // Materialise output rows. SUM result type follows PG: SUM
        // of Int yields BigInt; SUM of BigInt yields Numeric, but
        // we approximate with BigInt for the int-fast path.
        // Min/Max output type matches the input column.
        let agg_input_int_kind = |proj_pos: usize| -> aiondb_core::DataType {
            let table_ord = required_ordinals[proj_pos];
            table.columns[table_ord].data_type.clone()
        };
        let mut rows = Vec::with_capacity(ordered_groups.len());
        for group in &ordered_groups {
            context.check_deadline()?;
            if usize_to_u64(rows.len()) >= context.max_result_rows {
                return Err(DbError::program_limit(
                    "maximum number of result rows reached",
                ));
            }
            let mut values = Vec::with_capacity(slot_count);
            for (slot_idx, slot) in slots.iter().enumerate() {
                let value = match *slot {
                    FastSlot::GroupKey => match group_col.data_type {
                        aiondb_core::DataType::Int => {
                            Value::Int(i32::try_from(group.group_key).unwrap_or(i32::MAX))
                        }
                        aiondb_core::DataType::BigInt => Value::BigInt(group.group_key),
                        _ => unreachable!("group column type guarded above"),
                    },
                    FastSlot::CountStar => Value::BigInt(group.counts[slot_idx]),
                    FastSlot::Sum { .. } => {
                        if group.seen[slot_idx] {
                            Value::BigInt(group.sums[slot_idx])
                        } else {
                            Value::Null
                        }
                    }
                    FastSlot::Min { proj_pos } | FastSlot::Max { proj_pos } => {
                        if group.seen[slot_idx] {
                            match agg_input_int_kind(proj_pos) {
                                aiondb_core::DataType::Int => Value::Int(
                                    i32::try_from(group.sums[slot_idx]).unwrap_or(i32::MAX),
                                ),
                                aiondb_core::DataType::BigInt => {
                                    Value::BigInt(group.sums[slot_idx])
                                }
                                _ => unreachable!("agg column type guarded above"),
                            }
                        } else {
                            Value::Null
                        }
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
                Some(order_column_indices),
                context,
            )?;
        }

        let offset_val = offset
            .map(|e| eval_limit_offset_expr(&self.evaluator, e, "OFFSET"))
            .transpose()?
            .unwrap_or(0);
        if offset_val > 0 {
            let skip = clamp_u64_to_usize(offset_val, rows.len());
            rows.drain(..skip);
        }
        let plan_limit = limit
            .map(|e| eval_limit_offset_expr(&self.evaluator, e, "LIMIT"))
            .transpose()?;
        if let Some(limit) = effective_collect_limit(plan_limit, context.collect_row_limit) {
            rows.truncate(clamp_u64_to_usize(limit, rows.len()));
        }

        let mut result_bytes = 0u64;
        for row in &rows {
            result_bytes = ensure_result_bytes_fit_and_track_query_row(context, row, result_bytes)?;
        }

        // Suppress an unused-warning when SUM-of-BigInt overflow
        // semantics remain identical to the generic path.
        let _ = aggregates;

        Ok(Some(ExecutionResult::Query {
            columns: plan.output_fields(),
            rows,
        }))
    }

    fn try_count_project_table_source(
        &self,
        context: &ExecutionContext,
        table_id: RelationId,
        source_filter: Option<&TypedExpr>,
        access_path: &ScanAccessPath,
    ) -> DbResult<Option<u64>> {
        let Some(simple_filter) = extract_simple_group_filter(source_filter) else {
            return Ok(None);
        };
        let Some(table) = self
            .catalog_reader
            .get_table_by_id(context.txn_id, table_id)?
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

        let Some(filter) = simple_filter else {
            return self
                .storage_dml
                .visible_row_count(context.txn_id, &context.snapshot, table_id)
                .map(Some)
                .or_else(|error| {
                    if error.sqlstate() == SqlState::FeatureNotSupported {
                        Ok(None)
                    } else {
                        Err(error)
                    }
                });
        };

        let projected_columns = self
            .table_column_ids_for_ordinals(context, table_id, &[filter.column_ordinal])?
            .ok_or_else(|| DbError::internal("failed to map count source filter column"))?;
        let mut stream =
            match self.resolve_scan_stream(context, table_id, access_path, Some(projected_columns))
            {
                Ok(stream) => stream,
                Err(error) => {
                    if aiondb_planner::is_virtual_synthetic_relation(table_id.get()) {
                        Box::new(VecTupleStream::new(Vec::new()))
                    } else {
                        return Err(error);
                    }
                }
            };
        let has_interrupts = context.has_execution_interrupts();
        let mut scanned_rows = 0usize;
        let mut count = 0u64;
        while let Some(record) = stream.next()? {
            if has_interrupts && scanned_rows.is_multiple_of(256) {
                context.check_deadline()?;
            }
            scanned_rows = scanned_rows.wrapping_add(1);
            let value = record.row.values.first().unwrap_or(&Value::Null);
            if simple_group_filter_matches(value, &filter)? {
                count = count.saturating_add(1);
            }
        }
        Ok(Some(count))
    }

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
