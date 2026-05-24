use super::*;
use aiondb_plan::ScalarFunction;

use crate::{i64_to_f64, usize_to_f64};

pub(super) fn is_set_returning_function(func: &ScalarFunction) -> bool {
    match func {
        ScalarFunction::GenerateSeries
        | ScalarFunction::RegexpSplitToTable
        | ScalarFunction::Unnest => true,
        ScalarFunction::Generic(name) => {
            name.eq_ignore_ascii_case("generate_subscripts")
                || name.eq_ignore_ascii_case("graph_neighbors")
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
        }
        _ => false,
    }
}

// -------------------------------------------------------------------
// Hash-join key extraction helpers used by the physical builder.
// -------------------------------------------------------------------

/// Rough row count estimate for a physical plan node.
/// Used for join strategy costing.
///
/// Estimates are derived from the access path and plan structure rather than
/// catalog statistics (the physical builder has no catalog handle).  The
/// heuristics below are modelled after PostgreSQL's default assumptions when
/// no `ANALYZE` data is available.
fn estimate_access_path_rows(access_path: &ScanAccessPath) -> f64 {
    match access_path {
        ScanAccessPath::IndexEq { .. } => 1.0,
        ScanAccessPath::IndexEqComposite { values, .. } => {
            if values.len() >= 2 {
                1.0
            } else {
                5.0
            }
        }
        ScanAccessPath::IndexRange { .. } => 100.0,
        ScanAccessPath::IndexEqRangeComposite { eq_values, .. } => {
            if eq_values.is_empty() {
                100.0
            } else {
                10.0
            }
        }
        ScanAccessPath::GinContainment { .. } => 80.0,
        ScanAccessPath::SeqScan => 1000.0,
        ScanAccessPath::BitmapOr { paths } => paths
            .iter()
            .map(estimate_access_path_rows)
            .sum::<f64>()
            .clamp(1.0, 1000.0),
        ScanAccessPath::BitmapAnd { paths } => estimate_bitmap_and_rows(paths),
        ScanAccessPath::IndexOnlyScan { inner, .. } => estimate_access_path_rows(inner),
    }
}

fn estimate_bitmap_and_rows(paths: &[ScanAccessPath]) -> f64 {
    if paths.is_empty() {
        return 1.0;
    }
    let base_rows = 1000.0;
    let selectivity = paths
        .iter()
        .map(|path| (estimate_access_path_rows(path) / base_rows).clamp(0.001, 1.0))
        .product::<f64>();
    (base_rows * selectivity).clamp(1.0, base_rows)
}

fn filter_is_covered_by_access_path(access_path: &ScanAccessPath, filter: &TypedExpr) -> bool {
    match access_path {
        ScanAccessPath::IndexEq { .. } => filter_predicate_shape(filter) == PredicateShape::Eq,
        ScanAccessPath::IndexRange { .. } => {
            filter_predicate_shape(filter) == PredicateShape::Range
        }
        ScanAccessPath::IndexEqComposite { values, .. } => {
            let shape = predicate_shape_counts(filter);
            shape.eq_count == values.len() && shape.range_count == 0 && shape.other_count == 0
        }
        ScanAccessPath::IndexEqRangeComposite { eq_values, .. } => {
            let shape = predicate_shape_counts(filter);
            shape.eq_count == eq_values.len() && shape.range_count == 1 && shape.other_count == 0
        }
        ScanAccessPath::BitmapAnd { .. } => {
            let filter_shape = predicate_shape_counts(filter);
            access_path_predicate_shape_counts(access_path).is_some_and(|access_shape| {
                access_shape.eq_count == filter_shape.eq_count
                    && access_shape.range_count == filter_shape.range_count
                    && filter_shape.other_count == 0
            })
        }
        ScanAccessPath::BitmapOr { .. } => bitmap_or_filter_is_covered_by_access_path(filter),
        ScanAccessPath::IndexOnlyScan { inner, .. } => {
            filter_is_covered_by_access_path(inner, filter)
        }
        _ => false,
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum PredicateShape {
    Eq,
    Range,
    Other,
}

#[derive(Default)]
struct PredicateShapeCounts {
    eq_count: usize,
    range_count: usize,
    other_count: usize,
}

fn predicate_shape_counts(expr: &TypedExpr) -> PredicateShapeCounts {
    match &expr.kind {
        TypedExprKind::LogicalAnd { left, right } => {
            let mut left_counts = predicate_shape_counts(left);
            let right_counts = predicate_shape_counts(right);
            left_counts.eq_count += right_counts.eq_count;
            left_counts.range_count += right_counts.range_count;
            left_counts.other_count += right_counts.other_count;
            left_counts
        }
        _ => {
            let mut counts = PredicateShapeCounts::default();
            match filter_predicate_shape(expr) {
                PredicateShape::Eq => counts.eq_count = 1,
                PredicateShape::Range => counts.range_count = 1,
                PredicateShape::Other => counts.other_count = 1,
            }
            counts
        }
    }
}

fn access_path_predicate_shape_counts(
    access_path: &ScanAccessPath,
) -> Option<PredicateShapeCounts> {
    let mut counts = PredicateShapeCounts::default();
    match access_path {
        ScanAccessPath::IndexEq { .. } => counts.eq_count = 1,
        ScanAccessPath::IndexRange { .. } => counts.range_count = 1,
        ScanAccessPath::IndexEqComposite { values, .. } => counts.eq_count = values.len(),
        ScanAccessPath::IndexEqRangeComposite { eq_values, .. } => {
            counts.eq_count = eq_values.len();
            counts.range_count = 1;
        }
        ScanAccessPath::BitmapAnd { paths } => {
            for path in paths {
                let child_counts = access_path_predicate_shape_counts(path)?;
                counts.eq_count += child_counts.eq_count;
                counts.range_count += child_counts.range_count;
                counts.other_count += child_counts.other_count;
            }
        }
        ScanAccessPath::IndexOnlyScan { inner, .. } => {
            return access_path_predicate_shape_counts(inner);
        }
        ScanAccessPath::SeqScan
        | ScanAccessPath::GinContainment { .. }
        | ScanAccessPath::BitmapOr { .. } => {
            return None;
        }
    }
    Some(counts)
}

fn filter_predicate_shape(filter: &TypedExpr) -> PredicateShape {
    match &filter.kind {
        TypedExprKind::BinaryEq { .. } => PredicateShape::Eq,
        TypedExprKind::BinaryGe { .. }
        | TypedExprKind::BinaryGt { .. }
        | TypedExprKind::BinaryLe { .. }
        | TypedExprKind::BinaryLt { .. }
        | TypedExprKind::Between { negated: false, .. } => PredicateShape::Range,
        _ => PredicateShape::Other,
    }
}

fn bitmap_or_filter_is_covered_by_access_path(filter: &TypedExpr) -> bool {
    match &filter.kind {
        TypedExprKind::InList { negated, .. } => !*negated,
        TypedExprKind::LogicalOr { left, right } => {
            bitmap_or_filter_is_covered_by_access_path(left)
                && bitmap_or_filter_is_covered_by_access_path(right)
        }
        TypedExprKind::BinaryEq { .. } => true,
        _ => false,
    }
}

fn estimate_access_path_rows_with_filter(
    access_path: &ScanAccessPath,
    filter: &Option<TypedExpr>,
) -> f64 {
    let base = estimate_access_path_rows(access_path);
    if filter
        .as_ref()
        .is_some_and(|filter| filter_is_covered_by_access_path(access_path, filter))
    {
        return base;
    }
    apply_filter_selectivity(base, filter)
}

pub fn estimate_plan_rows(plan: &PhysicalPlan) -> f64 {
    match plan {
        // ----- ProjectTable: use access_path + filter + limit -----
        PhysicalPlan::ProjectTable {
            access_path,
            filter,
            limit,
            offset,
            ..
        }
        | PhysicalPlan::LockingProjectTable {
            access_path,
            filter,
            limit,
            offset,
            ..
        } => {
            let filtered = estimate_access_path_rows_with_filter(access_path, filter);
            let offset_rows = apply_offset(filtered, offset);
            apply_limit(offset_rows, limit)
        }
        PhysicalPlan::ProjectSource {
            source,
            filter,
            distinct,
            distinct_on,
            limit,
            offset,
            ..
        } => {
            let base = estimate_plan_rows(source);
            let filtered = apply_filter_selectivity(base, filter);
            let deduped = apply_distinct_reduction(filtered, *distinct, distinct_on);
            let offset_rows = apply_offset(deduped, offset);
            apply_limit(offset_rows, limit)
        }
        PhysicalPlan::AggregateSource {
            source,
            group_by,
            having,
            filter,
            distinct,
            distinct_on,
            limit,
            offset,
            ..
        } => {
            let filtered_source = apply_filter_selectivity(estimate_plan_rows(source), filter);
            let base = apply_group_reduction(filtered_source, group_by);
            let having_filtered = apply_filter_selectivity(base, having);
            let deduped = apply_distinct_reduction(having_filtered, *distinct, distinct_on);
            let offset_rows = apply_offset(deduped, offset);
            apply_limit(offset_rows, limit)
        }
        PhysicalPlan::Aggregate {
            access_path,
            group_by,
            having,
            filter,
            distinct,
            distinct_on,
            limit,
            offset,
            ..
        } => {
            let filtered_source = estimate_access_path_rows_with_filter(access_path, filter);
            let base = apply_group_reduction(filtered_source, group_by);
            let having_filtered = apply_filter_selectivity(base, having);
            let deduped = apply_distinct_reduction(having_filtered, *distinct, distinct_on);
            let offset_rows = apply_offset(deduped, offset);
            apply_limit(offset_rows, limit)
        }
        PhysicalPlan::HybridFunctionScan {
            function_name,
            args,
            ..
        } => estimate_hybrid_function_rows(function_name, args),

        // ----- Joins: estimate from children -----
        PhysicalPlan::NestedLoopJoin {
            left,
            right,
            join_type,
            condition,
            filter,
            distinct,
            distinct_on,
            limit,
            offset,
            ..
        } => {
            let left_rows = estimate_plan_rows(left);
            let right_rows = estimate_plan_rows(right);
            let selectivity = estimate_join_condition_selectivity(condition.as_ref());
            let joined = estimate_join_rows(left_rows, right_rows, selectivity, *join_type);
            apply_plan_output_shape(joined, filter, *distinct, distinct_on, offset, limit)
        }
        // HashJoin/MergeJoin are equi-joins by construction (left_keys ↔
        // right_keys), so the join condition is `col = col` and PG's
        // `DEFAULT_EQ_SEL = 0.005` is the right baseline. The historic
        // 0.1 over-estimated cardinality 20× and pushed the planner to
        // prefer NLJ over HashJoin on real workloads.
        PhysicalPlan::HashJoin {
            left,
            right,
            join_type,
            condition,
            filter,
            distinct,
            distinct_on,
            limit,
            offset,
            ..
        } => {
            let left_rows = estimate_plan_rows(left);
            let right_rows = estimate_plan_rows(right);
            let joined = estimate_join_rows(left_rows, right_rows, 0.005, *join_type);
            let residual_filtered = apply_filter_selectivity(joined, condition);
            apply_plan_output_shape(
                residual_filtered,
                filter,
                *distinct,
                distinct_on,
                offset,
                limit,
            )
        }
        PhysicalPlan::MergeJoin {
            left,
            right,
            join_type,
            residual,
            filter,
            distinct,
            distinct_on,
            limit,
            offset,
            ..
        } => {
            let left_rows = estimate_plan_rows(left);
            let right_rows = estimate_plan_rows(right);
            let joined = estimate_join_rows(left_rows, right_rows, 0.005, *join_type);
            let residual_filtered = apply_filter_selectivity(joined, residual);
            apply_plan_output_shape(
                residual_filtered,
                filter,
                *distinct,
                distinct_on,
                offset,
                limit,
            )
        }

        // ----- Parameterized index join: left_rows * ~1 match per lookup -----
        PhysicalPlan::NestedLoopIndexJoin {
            left,
            join_type,
            residual,
            filter,
            distinct,
            distinct_on,
            limit,
            offset,
            ..
        } => {
            let left_rows = estimate_plan_rows(left);
            // Index lookup typically returns 1 row per outer row.
            let joined = estimate_join_rows(left_rows, 1.0, 1.0, *join_type);
            let residual_filtered = apply_filter_selectivity(joined, residual);
            apply_plan_output_shape(
                residual_filtered,
                filter,
                *distinct,
                distinct_on,
                offset,
                limit,
            )
        }

        // ----- Other plan nodes -----
        PhysicalPlan::DistributedScan { filter, .. } => {
            // `DistributedScan` is a physical fan-out of one logical scan, so
            // preserve the logical scan estimate instead of multiplying by
            // node count.
            apply_filter_selectivity(1000.0, filter)
        }
        PhysicalPlan::PartialAggregate {
            source, group_by, ..
        } => apply_group_reduction(estimate_plan_rows(source), group_by),
        PhysicalPlan::FinalAggregate {
            partials,
            group_by,
            having,
            limit,
            offset,
            ..
        } => {
            let base = if group_by.is_empty() {
                1.0
            } else {
                partials
                    .iter()
                    .map(estimate_plan_rows)
                    .reduce(f64::max)
                    .unwrap_or(1.0)
                    .max(1.0)
            };
            let having_filtered = apply_filter_selectivity(base, having);
            let offset_rows = apply_offset(having_filtered, offset);
            apply_limit(offset_rows, limit)
        }
        PhysicalPlan::BroadcastHashJoin {
            broadcast,
            local,
            join_type,
            condition,
            ..
        } => {
            let local_rows = estimate_plan_rows(local);
            let broadcast_rows = estimate_plan_rows(broadcast);
            let joined = estimate_join_rows(local_rows, broadcast_rows, 0.005, *join_type);
            apply_filter_selectivity(joined, condition)
        }
        PhysicalPlan::HnswScan { limit, .. } => usize_to_f64((*limit).max(1)),
        PhysicalPlan::SetOperation {
            op,
            all,
            left,
            right,
            limit,
            offset,
            ..
        } => {
            let base = estimate_set_operation_rows(
                *op,
                *all,
                estimate_plan_rows(left),
                estimate_plan_rows(right),
            );
            let offset_rows = apply_offset(base, offset);
            apply_limit(offset_rows, limit)
        }
        PhysicalPlan::DistributedAppend {
            fragments,
            limit,
            offset,
            ..
        } => {
            let base = fragments.iter().map(estimate_plan_rows).sum::<f64>();
            let offset_rows = apply_offset(base, offset);
            apply_limit(offset_rows, limit)
        }
        PhysicalPlan::RecursiveCte {
            base,
            recursive,
            union_all,
            ..
        } => estimate_recursive_cte_rows(
            estimate_plan_rows(base),
            estimate_plan_rows(recursive),
            *union_all,
        ),
        PhysicalPlan::Gather { child, .. } => estimate_plan_rows(child),
        PhysicalPlan::ProjectValues {
            rows,
            limit,
            offset,
            ..
        } => {
            let base = usize_to_f64(rows.len());
            let offset_rows = apply_offset(base, offset);
            apply_limit(offset_rows, limit)
        }
        PhysicalPlan::ProjectOnce { limit, offset, .. } => {
            let offset_rows = apply_offset(1.0, offset);
            apply_limit(offset_rows, limit)
        }
        PhysicalPlan::SeqScan { .. } => 1000.0,
        PhysicalPlan::CreateTableAs {
            with_no_data,
            source,
            ..
        } => {
            if *with_no_data {
                0.0
            } else {
                estimate_plan_rows(source)
            }
        }
        PhysicalPlan::InsertValues { rows, .. } => usize_to_f64(rows.len()),
        PhysicalPlan::InsertSelect { source, .. } => estimate_plan_rows(source),
        PhysicalPlan::DeleteFromTable { filter, .. } | PhysicalPlan::UpdateTable { filter, .. } => {
            apply_filter_selectivity(1000.0, filter)
        }
        _ => 1000.0,
    }
}

fn estimate_recursive_cte_rows(base_rows: f64, recursive_rows: f64, union_all: bool) -> f64 {
    let iterations = if union_all { 10.0 } else { 5.0 };
    (base_rows + (recursive_rows * iterations)).max(1.0)
}

fn estimate_set_operation_rows(
    op: SetOperationType,
    all: bool,
    left_rows: f64,
    right_rows: f64,
) -> f64 {
    match op {
        SetOperationType::Union if all => left_rows + right_rows,
        SetOperationType::Union => {
            let input_rows = left_rows + right_rows;
            if input_rows <= 0.0 {
                0.0
            } else {
                (input_rows * 0.5).max(1.0)
            }
        }
        SetOperationType::Intersect if all => {
            let input_rows = left_rows.min(right_rows);
            if input_rows <= 0.0 {
                0.0
            } else {
                input_rows.max(1.0)
            }
        }
        SetOperationType::Intersect => {
            let input_rows = left_rows.min(right_rows);
            if input_rows <= 0.0 {
                0.0
            } else {
                (input_rows * 0.5).max(1.0)
            }
        }
        SetOperationType::Except => {
            if left_rows <= 0.0 {
                0.0
            } else {
                (left_rows * 0.5).max(1.0)
            }
        }
    }
}

fn apply_plan_output_shape(
    base: f64,
    filter: &Option<TypedExpr>,
    distinct: bool,
    distinct_on: &[TypedExpr],
    offset: &Option<TypedExpr>,
    limit: &Option<TypedExpr>,
) -> f64 {
    let filtered = apply_filter_selectivity(base, filter);
    let deduped = apply_distinct_reduction(filtered, distinct, distinct_on);
    let offset_rows = apply_offset(deduped, offset);
    apply_limit(offset_rows, limit)
}

/// Estimate join output rows accounting for join type semantics.
///
/// For SEMI joins the output cannot exceed the outer (left) side - each
/// left row either matches or does not.  For ANTI joins the output is
/// the complement: left rows that have *no* match.
fn estimate_join_rows(
    left_rows: f64,
    right_rows: f64,
    selectivity: f64,
    join_type: JoinType,
) -> f64 {
    match join_type {
        JoinType::Semi => {
            if left_rows <= 0.0 || right_rows <= 0.0 {
                return 0.0;
            }
            // At most left_rows; reduced by join selectivity.
            (left_rows * selectivity).clamp(1.0, left_rows)
        }
        JoinType::Anti => {
            if left_rows <= 0.0 {
                return 0.0;
            }
            if right_rows <= 0.0 {
                return left_rows;
            }
            // Left rows that do NOT match.
            (left_rows * (1.0 - selectivity)).max(1.0)
        }
        JoinType::Inner => {
            if left_rows <= 0.0 || right_rows <= 0.0 {
                return 0.0;
            }
            // INNER / LEFT / RIGHT / FULL: cross-product × selectivity.
            (left_rows * right_rows * selectivity).max(1.0)
        }
        JoinType::Left => {
            if left_rows <= 0.0 {
                return 0.0;
            }
            if right_rows <= 0.0 {
                return left_rows;
            }
            (left_rows * right_rows * selectivity).max(left_rows)
        }
        JoinType::Right => {
            if right_rows <= 0.0 {
                return 0.0;
            }
            if left_rows <= 0.0 {
                return right_rows;
            }
            (left_rows * right_rows * selectivity).max(right_rows)
        }
        JoinType::Full => {
            if left_rows <= 0.0 {
                return right_rows.max(0.0);
            }
            if right_rows <= 0.0 {
                return left_rows.max(0.0);
            }
            (left_rows * right_rows * selectivity).max(left_rows.max(right_rows))
        }
    }
}

/// Argument layout of a `vector_top_k_*` family special function, used to
/// drive the row estimator without baking ad-hoc index constants into each
/// call site.
#[derive(Clone, Copy)]
struct VectorTopKLayout {
    /// Index of the `k` literal argument.
    k_idx: usize,
    /// Index of the `distance_threshold` argument, when present in this
    /// function family.
    distance_threshold_idx: Option<usize>,
    /// Index of the `score_threshold` argument, when present in this
    /// function family.
    score_threshold_idx: Option<usize>,
    /// Index of the trailing `options` JSON / JSONB argument, when present.
    options_idx: Option<usize>,
}

impl VectorTopKLayout {
    const STANDARD: Self = Self {
        k_idx: 3,
        distance_threshold_idx: Some(6),
        score_threshold_idx: Some(8),
        options_idx: Some(9),
    };
    const PREFETCH: Self = Self {
        k_idx: 4,
        distance_threshold_idx: None,
        score_threshold_idx: None,
        options_idx: None,
    };
    const RECOMMEND: Self = Self {
        k_idx: 4,
        distance_threshold_idx: None,
        score_threshold_idx: None,
        options_idx: None,
    };
}

#[derive(Default)]
struct VectorTopKOptionHints {
    limit: Option<u64>,
    offset: u64,
    has_filter: bool,
    has_threshold: bool,
}

fn vector_top_k_option_hints(expr: &TypedExpr) -> VectorTopKOptionHints {
    let TypedExprKind::Literal(value) = &expr.kind else {
        return VectorTopKOptionHints::default();
    };
    match value {
        Value::Jsonb(json) => json.as_object().map(hints_from_object).unwrap_or_default(),
        Value::Text(text) => serde_json::from_str::<serde_json::Value>(text)
            .ok()
            .as_ref()
            .and_then(|json| json.as_object())
            .map(hints_from_object)
            .unwrap_or_default(),
        _ => VectorTopKOptionHints::default(),
    }
}

fn hints_from_object(object: &serde_json::Map<String, serde_json::Value>) -> VectorTopKOptionHints {
    let mut hints = VectorTopKOptionHints::default();
    for (raw_key, raw_value) in object {
        match raw_key.to_ascii_lowercase().as_str() {
            "limit" => {
                hints.limit = raw_value
                    .as_u64()
                    .or_else(|| raw_value.as_i64().map(|limit| limit.max(0) as u64));
            }
            "offset" => {
                if let Some(offset) = raw_value.as_u64() {
                    hints.offset = offset;
                } else if let Some(offset) = raw_value.as_i64() {
                    hints.offset = u64::try_from(offset.max(0)).unwrap_or(0);
                }
            }
            "filter" => {
                if !raw_value.is_null() {
                    hints.has_filter = true;
                }
            }
            "distance_threshold" | "score_threshold" => {
                if !raw_value.is_null() {
                    hints.has_threshold = true;
                }
            }
            "vector_distance_threshold" | "vector_score_threshold" | "text_score_threshold" => {
                if !raw_value.is_null() {
                    hints.has_threshold = true;
                }
            }
            _ => {}
        }
    }
    hints
}

fn estimate_top_k_rows(k: f64, hints: VectorTopKOptionHints, threshold_in_args: bool) -> f64 {
    let base = hints.limit.map_or(k, u64_to_f64_saturating);
    if base <= 0.0 {
        return 0.0;
    }
    let _offset = hints.offset;
    // Payload filter or explicit threshold prunes some candidates. Without
    // catalog statistics we apply a conservative 0.5 default per hint, so
    // the join planner sees a tighter cardinality without underestimating
    // by more than an order of magnitude.
    let filter_multiplier = if hints.has_filter { 0.5 } else { 1.0 };
    let threshold_multiplier = if hints.has_threshold || threshold_in_args {
        0.5
    } else {
        1.0
    };
    (base * filter_multiplier * threshold_multiplier).max(1.0)
}

/// Estimate the row count for a `vector_top_k_*` special function.
///
/// Without table statistics we cannot compute the true intersection of the
/// vector top-K cap and the payload filter selectivity, but we can still
/// fold in: the literal `k`, the offset slice consumed before output, an
/// explicit distance / score threshold, and whether a payload filter is
/// present. Each restrictive option scales the estimate down so downstream
/// join planning sees a tighter cardinality.
fn estimate_vector_top_k_rows(args: &[TypedExpr], layout: VectorTopKLayout) -> f64 {
    let k = args
        .get(layout.k_idx)
        .and_then(literal_row_count_hint)
        .unwrap_or(10.0);
    let threshold_in_args = [layout.distance_threshold_idx, layout.score_threshold_idx]
        .into_iter()
        .flatten()
        .any(|idx| {
            args.get(idx).is_some_and(|arg| {
                !matches!(&arg.kind, TypedExprKind::Literal(Value::Null))
                    && !matches!(&arg.kind, TypedExprKind::Literal(Value::Boolean(_)))
            })
        });
    let hints = layout
        .options_idx
        .and_then(|idx| args.get(idx))
        .map(vector_top_k_option_hints)
        .unwrap_or_default();
    estimate_top_k_rows(k, hints, threshold_in_args)
}

fn u64_to_f64_saturating(value: u64) -> f64 {
    if value > (i64::MAX as u64) {
        i64::MAX as f64
    } else {
        i64_to_f64(value as i64)
    }
}

pub(crate) fn estimate_hybrid_function_rows(function_name: &str, args: &[TypedExpr]) -> f64 {
    if function_name.eq_ignore_ascii_case("vector_top_k_ids")
        || function_name.eq_ignore_ascii_case("vector_top_k_hits")
    {
        return estimate_vector_top_k_rows(args, VectorTopKLayout::STANDARD);
    }
    if function_name.eq_ignore_ascii_case("vector_prefetch_top_k_hits") {
        return estimate_vector_top_k_rows(args, VectorTopKLayout::PREFETCH);
    }
    if function_name.eq_ignore_ascii_case("vector_recommend_top_k_hits") {
        return estimate_vector_top_k_rows(args, VectorTopKLayout::RECOMMEND);
    }
    if function_name.eq_ignore_ascii_case("full_text_top_k_hits") {
        let k = args.get(3).and_then(literal_row_count_hint).unwrap_or(10.0);
        let threshold_in_args = args
            .get(6)
            .is_some_and(|arg| !matches!(&arg.kind, TypedExprKind::Literal(Value::Null)));
        let hints = args
            .get(7)
            .map(vector_top_k_option_hints)
            .unwrap_or_default();
        return estimate_top_k_rows(k, hints, threshold_in_args);
    }
    if function_name.eq_ignore_ascii_case("hybrid_search_top_k_hits") {
        let k = args.get(5).and_then(literal_row_count_hint).unwrap_or(10.0);
        let hints = args
            .get(6)
            .map(vector_top_k_option_hints)
            .unwrap_or_default();
        return estimate_top_k_rows(k, hints, false);
    }
    if function_name.eq_ignore_ascii_case("hybrid_fuse_rrf_hits") {
        return args.get(2).and_then(literal_row_count_hint).unwrap_or(10.0);
    }
    if function_name.eq_ignore_ascii_case("hybrid_fuse_dbsf_hits") {
        return args.get(2).and_then(literal_row_count_hint).unwrap_or(10.0);
    }
    if function_name.eq_ignore_ascii_case("hybrid_group_hits_by") {
        return args.get(2).and_then(literal_row_count_hint).unwrap_or(10.0);
    }
    if function_name.eq_ignore_ascii_case("graph_neighbors") {
        // Single-hop adjacency expansion is usually small enough that the
        // surrounding SQL join strategy should treat it as a selective source.
        return graph_neighbors_limit_hint(args).map_or(32.0, |limit| limit.min(32.0));
    }
    1000.0
}

pub(super) fn graph_neighbors_limit_hint(args: &[TypedExpr]) -> Option<f64> {
    let positional_limit = match args.len() {
        3 => args.get(2).and_then(literal_row_count_hint),
        4 => args.get(3).and_then(literal_row_count_hint),
        _ => None,
    };
    positional_limit.or_else(|| {
        args.last().and_then(|arg| {
            vector_top_k_option_hints(arg)
                .limit
                .map(u64_to_f64_saturating)
        })
    })
}

pub(super) fn apply_filter_selectivity(base: f64, filter: &Option<TypedExpr>) -> f64 {
    if base <= 0.0 {
        return 0.0;
    }
    filter.as_ref().map_or(base, |expr| {
        let selectivity = estimate_filter_selectivity(expr);
        if selectivity <= 0.0 {
            0.0
        } else {
            (base * selectivity).max(1.0)
        }
    })
}

pub(super) fn apply_distinct_reduction(
    base: f64,
    distinct: bool,
    distinct_on: &[TypedExpr],
) -> f64 {
    if base <= 0.0 {
        return 0.0;
    }
    if distinct || !distinct_on.is_empty() {
        (base * 0.5).max(1.0)
    } else {
        base
    }
}

fn apply_group_reduction(base: f64, group_by: &[TypedExpr]) -> f64 {
    if base <= 0.0 && !group_by.is_empty() {
        return 0.0;
    }
    if group_by.is_empty() {
        1.0
    } else {
        (base * 0.1).max(1.0)
    }
}

pub(crate) fn estimate_filter_selectivity(expr: &TypedExpr) -> f64 {
    match &expr.kind {
        TypedExprKind::Literal(Value::Boolean(true)) => 1.0,
        TypedExprKind::Literal(Value::Boolean(false) | Value::Null) => 0.0,
        // `col = const` is the dominant equality shape in real workloads
        // and matches PG's 0.005 default `eq_selectivity` for unknown
        // distributions. Fall back to the historic 0.1 estimate for
        // shapes we cannot classify (col=col, expr=expr) so we don't
        // over-shrink join cardinalities.
        TypedExprKind::BinaryEq { left, right } => {
            if is_col_const_pair(left, right) {
                0.005
            } else {
                0.1
            }
        }
        TypedExprKind::BinaryNe { left, right } => {
            if is_col_const_pair(left, right) {
                // 1 - 0.005 = 0.995, but PG additionally subtracts the
                // null fraction; absent stats we keep a conservative
                // 0.99 so the planner still recognises this as
                // near-everything (reusing the historic flat 0.9
                // would dramatically under-estimate).
                0.99
            } else {
                0.9
            }
        }
        TypedExprKind::BinaryGe { .. }
        | TypedExprKind::BinaryGt { .. }
        | TypedExprKind::BinaryLe { .. }
        | TypedExprKind::BinaryLt { .. } => 0.3,
        TypedExprKind::LogicalAnd { left, right } => clamp_selectivity(
            estimate_filter_selectivity(left) * estimate_filter_selectivity(right),
        ),
        TypedExprKind::LogicalOr { left, right } => {
            let left_sel = estimate_filter_selectivity(left);
            let right_sel = estimate_filter_selectivity(right);
            clamp_selectivity(left_sel + right_sel - (left_sel * right_sel))
        }
        TypedExprKind::LogicalNot { expr } => {
            clamp_selectivity(1.0 - estimate_filter_selectivity(expr))
        }
        TypedExprKind::IsNull { negated, .. } => {
            if *negated {
                0.9
            } else {
                0.1
            }
        }
        TypedExprKind::IsDistinctFrom { negated, .. } => {
            if *negated {
                0.1
            } else {
                0.9
            }
        }
        TypedExprKind::Like {
            pattern, negated, ..
        } => {
            let base = like_pattern_selectivity(pattern);
            if *negated {
                clamp_selectivity(1.0 - base)
            } else {
                base
            }
        }
        TypedExprKind::InList { list, negated, .. } => {
            // Treat each constant as `eq_selectivity` (matches PG's
            // ~1/200 default for unknown distributions). The previous
            // 0.1-per-item factor saturated `clamp_selectivity` to 1.0
            // for any list with 10+ items, so the planner thought
            // `WHERE id IN (50 ints)` returned every row in the
            // table --- a regression on a very common ORM batch shape.
            // Cap at 0.5 because an IN-list rarely selects the
            // majority of a relation.
            const PER_ITEM_EQ_SELECTIVITY: f64 = 0.005;
            let base =
                clamp_selectivity((usize_to_f64(list.len()) * PER_ITEM_EQ_SELECTIVITY).min(0.5));
            if *negated {
                clamp_selectivity(1.0 - base)
            } else {
                base
            }
        }
        TypedExprKind::Between { negated, .. } => {
            if *negated {
                0.8
            } else {
                0.2
            }
        }
        TypedExprKind::Cast { expr, .. } => estimate_filter_selectivity(expr),
        _ => 0.3,
    }
}

pub(super) fn like_pattern_selectivity(pattern: &TypedExpr) -> f64 {
    match &pattern.kind {
        TypedExprKind::Literal(Value::Text(value)) => {
            let wildcard_count = value.chars().filter(|ch| matches!(ch, '%' | '_')).count();
            if wildcard_count == 0 {
                0.1
            } else if wildcard_count == 1
                && value.ends_with('%')
                && !value[..value.len() - 1].contains('_')
            {
                0.12
            } else {
                0.25
            }
        }
        _ => 0.25,
    }
}

/// Return `true` when one side of a binary comparison is a `ColumnRef`
/// (or a cast thereof) and the other is a constant expression - the
/// shape `col = const` / `const = col`.
fn is_col_const_pair(left: &TypedExpr, right: &TypedExpr) -> bool {
    (is_column_like(left) && is_const_expr(right)) || (is_column_like(right) && is_const_expr(left))
}

/// Return `true` when both sides of a binary comparison are column
/// references - the shape `col = col` (e.g. an equi-join condition).
fn is_col_col_pair(left: &TypedExpr, right: &TypedExpr) -> bool {
    is_column_like(left) && is_column_like(right)
}

fn is_column_like(expr: &TypedExpr) -> bool {
    match &expr.kind {
        TypedExprKind::ColumnRef { .. } | TypedExprKind::OuterColumnRef { .. } => true,
        TypedExprKind::Cast { expr, .. } => is_column_like(expr),
        _ => false,
    }
}

/// PostgreSQL-style default selectivity for a join condition expression
/// when no statistics are available. Returns `0.005` for the equi-join
/// shape `col = col` (PG's `DEFAULT_EQ_SEL`), `0.1` for other shapes,
/// and `1.0` when there is no condition.
pub fn estimate_join_condition_selectivity(condition: Option<&TypedExpr>) -> f64 {
    let Some(cond) = condition else {
        return 1.0;
    };
    match &cond.kind {
        TypedExprKind::BinaryEq { left, right } if is_col_col_pair(left, right) => 0.005,
        _ => 0.1,
    }
}

pub(super) fn clamp_selectivity(selectivity: f64) -> f64 {
    if selectivity.is_finite() {
        if selectivity <= 0.0 {
            0.0
        } else {
            selectivity.clamp(0.01, 1.0)
        }
    } else {
        0.3
    }
}

pub(super) fn literal_row_count_hint(expr: &TypedExpr) -> Option<f64> {
    match &expr.kind {
        TypedExprKind::Literal(Value::Int(value)) => Some(f64::from((*value).max(0))),
        TypedExprKind::Literal(Value::BigInt(value)) => Some(i64_to_f64((*value).max(0))),
        _ => None,
    }
}

pub(super) fn apply_offset(base: f64, offset: &Option<TypedExpr>) -> f64 {
    match offset {
        Some(expr) => match &expr.kind {
            TypedExprKind::Literal(Value::Int(n)) => (base - f64::from((*n).max(0))).max(0.0),
            TypedExprKind::Literal(Value::BigInt(n)) => (base - i64_to_f64((*n).max(0))).max(0.0),
            _ => base,
        },
        None => base,
    }
}

/// Cap an estimated row count when a LIMIT clause is present.
/// If the limit expression is a constant integer, use it as an upper bound;
/// otherwise return the base estimate unchanged.
pub(super) fn apply_limit(base: f64, limit: &Option<TypedExpr>) -> f64 {
    match limit {
        Some(expr) => {
            if let TypedExprKind::Literal(Value::Int(n)) = &expr.kind {
                base.min(f64::from((*n).max(0)))
            } else if let TypedExprKind::Literal(Value::BigInt(n)) = &expr.kind {
                base.min(i64_to_f64((*n).max(0)))
            } else {
                base
            }
        }
        None => base,
    }
}

/// Estimate the output width of a physical plan used as a join child.
pub(super) fn physical_plan_child_width(plan: &PhysicalPlan) -> usize {
    match plan {
        PhysicalPlan::NestedLoopJoin {
            left,
            right,
            outputs,
            ..
        }
        | PhysicalPlan::HashJoin {
            left,
            right,
            outputs,
            ..
        }
        | PhysicalPlan::MergeJoin {
            left,
            right,
            outputs,
            ..
        } if outputs.is_empty() => {
            physical_plan_child_width(left).saturating_add(physical_plan_child_width(right))
        }
        other => other.output_fields().len(),
    }
}

pub(crate) fn exposed_plan_width(
    plan: &PhysicalPlan,
    remap: Option<JoinSwapOrdinalRemap>,
) -> usize {
    remap.map_or_else(
        || physical_plan_child_width(plan),
        JoinSwapOrdinalRemap::total_width,
    )
}

pub(super) fn join_child_widths(
    left: &PhysicalPlan,
    right: &PhysicalPlan,
    condition: Option<&TypedExpr>,
    filter: Option<&TypedExpr>,
    outputs: &[ProjectionExpr],
    order_by: &[SortExpr],
    distinct_on: &[TypedExpr],
    logical_input_widths: Option<(usize, usize)>,
) -> (usize, usize) {
    let mut left_width = physical_plan_child_width(left);
    let mut right_width = physical_plan_child_width(right);
    if let Some((logical_left_width, logical_right_width)) = logical_input_widths {
        left_width = left_width.max(logical_left_width);
        right_width = right_width.max(logical_right_width);
    }
    if left_width == 0 || right_width == 0 {
        let inferred_total = condition
            .into_iter()
            .chain(filter)
            .chain(outputs.iter().map(|projection| &projection.expr))
            .chain(order_by.iter().map(|sort| &sort.expr))
            .chain(distinct_on.iter())
            .filter_map(max_column_ordinal)
            .max()
            .map_or(0, |ordinal| ordinal.saturating_add(1));
        if left_width == 0 && right_width > 0 && inferred_total > right_width {
            left_width = inferred_total.saturating_sub(right_width);
        }
        if right_width == 0 && left_width > 0 && inferred_total > left_width {
            right_width = inferred_total.saturating_sub(left_width);
        }
    }
    (left_width, right_width)
}

pub(super) fn max_column_ordinal(expr: &TypedExpr) -> Option<usize> {
    let mut max = None;
    let mut stack = vec![expr];
    while let Some(expr) = stack.pop() {
        match &expr.kind {
            TypedExprKind::ColumnRef { ordinal, .. }
            | TypedExprKind::OuterColumnRef { ordinal, .. } => {
                max = Some(max.map_or(*ordinal, |current: usize| current.max(*ordinal)));
            }
            _ => crate::predicate_pushdown::for_each_child_expr(expr, &mut |child| {
                stack.push(child);
            }),
        }
    }
    max
}

/// Return the leading column ordinals on which a plan node produces
/// already-sorted output (ascending, nulls-last -- the default BTree order).
/// Returns an empty vec when the ordering is unknown.
pub(crate) fn plan_sorted_prefix(plan: &PhysicalPlan) -> Vec<usize> {
    if let Some(order_by) = explicit_plan_order_by(plan) {
        return sorted_prefix_from_order_by(order_by);
    }

    match plan {
        // An index-range or index-eq scan on a BTree index produces rows
        // sorted by the index key columns.
        PhysicalPlan::ProjectTable { access_path, .. }
        | PhysicalPlan::LockingProjectTable { access_path, .. } => {
            access_path_sorted_prefix(access_path)
        }
        PhysicalPlan::ProjectSource {
            source,
            outputs,
            distinct,
            distinct_on,
            ..
        } => project_source_sorted_prefix(source, outputs, *distinct, distinct_on),
        PhysicalPlan::Gather {
            child,
            preserve_order: true,
            ..
        } => plan_sorted_prefix(child),
        _ => Vec::new(),
    }
}

fn explicit_plan_order_by(plan: &PhysicalPlan) -> Option<&[SortExpr]> {
    match plan {
        PhysicalPlan::ProjectOnce { order_by, .. }
        | PhysicalPlan::ProjectTable { order_by, .. }
        | PhysicalPlan::LockingProjectTable { order_by, .. }
        | PhysicalPlan::ProjectSource { order_by, .. }
        | PhysicalPlan::NestedLoopJoin { order_by, .. }
        | PhysicalPlan::NestedLoopIndexJoin { order_by, .. }
        | PhysicalPlan::HashJoin { order_by, .. }
        | PhysicalPlan::MergeJoin { order_by, .. }
        | PhysicalPlan::Aggregate { order_by, .. }
        | PhysicalPlan::AggregateSource { order_by, .. }
        | PhysicalPlan::SetOperation { order_by, .. }
        | PhysicalPlan::DistributedAppend { order_by, .. }
        | PhysicalPlan::ProjectValues { order_by, .. }
        | PhysicalPlan::FinalAggregate { order_by, .. }
            if !order_by.is_empty() =>
        {
            Some(order_by)
        }
        _ => None,
    }
}

fn sorted_prefix_from_order_by(order_by: &[SortExpr]) -> Vec<usize> {
    let mut cols = Vec::new();
    for sort in order_by {
        if sort.descending {
            break;
        }
        if let Some((_, ordinal)) = sort.expr.kind.as_column_ref() {
            cols.push(ordinal);
        } else {
            break;
        }
    }
    cols
}

fn access_path_sorted_prefix(access_path: &ScanAccessPath) -> Vec<usize> {
    match access_path {
        ScanAccessPath::IndexRange { .. }
        | ScanAccessPath::IndexEqRangeComposite { .. }
        | ScanAccessPath::IndexEq { .. }
        | ScanAccessPath::IndexEqComposite { .. } => {
            // For the common single-column BTree index case, column 0 is
            // sorted.  A more sophisticated version would inspect the
            // catalog to derive the full key column list.
            vec![0]
        }
        ScanAccessPath::IndexOnlyScan { inner, .. } => access_path_sorted_prefix(inner),
        _ => Vec::new(),
    }
}

fn project_source_sorted_prefix(
    source: &PhysicalPlan,
    outputs: &[ProjectionExpr],
    distinct: bool,
    distinct_on: &[TypedExpr],
) -> Vec<usize> {
    if distinct || !distinct_on.is_empty() {
        return Vec::new();
    }
    let child_sorted = plan_sorted_prefix(source);
    if outputs.is_empty() {
        return child_sorted;
    }
    let mut parent_sorted = Vec::new();
    for child_ordinal in child_sorted {
        match outputs.iter().position(|output| {
            output
                .expr
                .kind
                .as_column_ref()
                .is_some_and(|(_, ordinal)| ordinal == child_ordinal)
        }) {
            Some(parent_ordinal) => parent_sorted.push(parent_ordinal),
            None => break,
        }
    }
    parent_sorted
}

/// Returns `true` when both physical plans produce output sorted on
/// (at least) the specified equi-join key ordinals, making a MergeJoin
/// valid.
pub(super) fn inputs_sorted_on_keys(
    left: &PhysicalPlan,
    left_keys: &[usize],
    right: &PhysicalPlan,
    right_keys: &[usize],
) -> bool {
    if left_keys.is_empty() {
        return false;
    }
    plan_sorted_on_keys(left, left_keys) && plan_sorted_on_keys(right, right_keys)
}

pub(crate) fn plan_sorted_on_keys(plan: &PhysicalPlan, keys: &[usize]) -> bool {
    let sorted_prefix = plan_sorted_prefix(plan);
    sorted_prefix_matches_keys(&sorted_prefix, keys)
}

pub(super) fn sorted_prefix_matches_keys(sorted_prefix: &[usize], keys: &[usize]) -> bool {
    !keys.is_empty()
        && keys.len() <= sorted_prefix.len()
        && keys
            .iter()
            .zip(sorted_prefix.iter())
            .all(|(expected_key, sorted_ordinal)| expected_key == sorted_ordinal)
}

/// Extract equi-join key ordinals from a join condition.
/// Returns `(left_keys, right_keys, residual)` where `residual` holds any
/// non-equality conjuncts.  Returns `None` if no equi-keys are found.
pub(super) fn extract_equi_join_keys(
    condition: Option<&TypedExpr>,
    left_width: usize,
    right_width: usize,
) -> Option<(Vec<usize>, Vec<usize>, Option<TypedExpr>)> {
    let condition = condition?;
    if left_width == 0 || right_width == 0 {
        return None;
    }
    let total_width = left_width.checked_add(right_width)?;
    let mut conjuncts = Vec::new();
    flatten_and(condition, &mut conjuncts);
    let mut left_keys = Vec::new();
    let mut right_keys = Vec::new();
    let mut residual = Vec::new();
    for c in conjuncts {
        if let Some((lk, rk)) = classify_equi_pair(&c, left_width, total_width) {
            left_keys.push(lk);
            right_keys.push(rk);
        } else {
            residual.push(c);
        }
    }
    if left_keys.is_empty() {
        return None;
    }
    let residual_expr = {
        let mut it = residual.into_iter();
        it.next()
            .map(|first| it.fold(first, TypedExpr::logical_and))
    };
    Some((left_keys, right_keys, residual_expr))
}

pub(super) fn flatten_and(expr: &TypedExpr, out: &mut Vec<TypedExpr>) {
    let mut stack = vec![expr];
    while let Some(expr) = stack.pop() {
        match &expr.kind {
            TypedExprKind::LogicalAnd { left, right } => {
                stack.push(right);
                stack.push(left);
            }
            _ => out.push(expr.clone()),
        }
    }
}

pub(super) fn classify_equi_pair(
    expr: &TypedExpr,
    left_width: usize,
    total_width: usize,
) -> Option<(usize, usize)> {
    let TypedExprKind::BinaryEq { left, right } = &expr.kind else {
        return None;
    };
    let lo = join_key_column_ordinal(left)?;
    let ro = join_key_column_ordinal(right)?;
    if lo < left_width && (left_width..total_width).contains(&ro) {
        return Some((lo, ro - left_width));
    }
    if ro < left_width && (left_width..total_width).contains(&lo) {
        return Some((ro, lo - left_width));
    }
    None
}

pub(super) fn join_key_column_ordinal(expr: &TypedExpr) -> Option<usize> {
    match &expr.kind {
        TypedExprKind::ColumnRef { ordinal, .. } => Some(*ordinal),
        TypedExprKind::Cast { expr, target_type }
            if is_hash_safe_join_key_cast(&expr.data_type, target_type) =>
        {
            join_key_column_ordinal(expr)
        }
        _ => None,
    }
}

pub(super) fn is_hash_safe_join_key_cast(source_type: &DataType, target_type: &DataType) -> bool {
    source_type == target_type
        || (is_exact_numeric_type(source_type) && is_exact_numeric_type(target_type))
}

pub(super) fn is_exact_numeric_type(data_type: &DataType) -> bool {
    matches!(
        data_type,
        DataType::Int | DataType::BigInt | DataType::Numeric
    )
}

/// Returns `true` when the expression tree contains no column references,
/// outer column references, aggregates, subqueries, user functions, or
/// window functions - i.e. it can be evaluated without any row context.
pub fn is_const_expr(expr: &TypedExpr) -> bool {
    match &expr.kind {
        TypedExprKind::Literal(_) => true,
        TypedExprKind::ColumnRef { .. }
        | TypedExprKind::OuterColumnRef { .. }
        | TypedExprKind::NextValue { .. } => false,
        // Binary comparisons
        TypedExprKind::BinaryEq { left, right }
        | TypedExprKind::BinaryNe { left, right }
        | TypedExprKind::BinaryGe { left, right }
        | TypedExprKind::BinaryGt { left, right }
        | TypedExprKind::BinaryLe { left, right }
        | TypedExprKind::BinaryLt { left, right }
        // Logical operators
        | TypedExprKind::LogicalAnd { left, right }
        | TypedExprKind::LogicalOr { left, right }
        // Arithmetic
        | TypedExprKind::ArithAdd { left, right }
        | TypedExprKind::ArithSub { left, right }
        | TypedExprKind::ArithMul { left, right }
        | TypedExprKind::ArithDiv { left, right }
        | TypedExprKind::ArithMod { left, right }
        // String / JSON / Array ops
        | TypedExprKind::Concat { left, right }
        | TypedExprKind::Nullif { left, right }
        | TypedExprKind::IsDistinctFrom { left, right, .. }
        | TypedExprKind::ArrayContains { left, right }
        | TypedExprKind::ArrayContainedBy { left, right }
        | TypedExprKind::ArrayOverlap { left, right } => {
            is_const_expr(left) && is_const_expr(right)
        }
        TypedExprKind::LogicalNot { expr }
        | TypedExprKind::Negate { expr }
        | TypedExprKind::IsNull { expr, .. }
        | TypedExprKind::Cast { expr, .. } => is_const_expr(expr),
        TypedExprKind::Like { expr, pattern, .. } => {
            is_const_expr(expr) && is_const_expr(pattern)
        }
        TypedExprKind::InList { expr, list, .. } => {
            is_const_expr(expr) && list.iter().all(is_const_expr)
        }
        TypedExprKind::Between {
            expr, low, high, ..
        } => is_const_expr(expr) && is_const_expr(low) && is_const_expr(high),
        TypedExprKind::CaseWhen {
            conditions,
            results,
            else_result,
        } => {
            conditions.iter().all(is_const_expr)
                && results.iter().all(is_const_expr)
                && else_result.as_ref().map_or(true, |e| is_const_expr(e))
        }
        TypedExprKind::Coalesce { args } => args.iter().all(is_const_expr),
        TypedExprKind::ScalarFunction { args, func } => {
            !is_volatile_function(func) && args.iter().all(is_const_expr)
        }
        TypedExprKind::ArrayConstruct { elements } => elements.iter().all(is_const_expr),
        // Aggregates, subqueries, user functions, window functions, etc.
        // are never constant-foldable.
        _ => false,
    }
}

/// Returns `true` for functions whose result depends on time, randomness,
/// session state, or that have side effects.  These must never be
/// constant-folded because their value can change between planning and
/// execution (or between successive evaluations).
pub(super) fn is_volatile_function(func: &ScalarFunction) -> bool {
    match func {
        // Random / non-deterministic
        ScalarFunction::Random
        | ScalarFunction::ArrayShuffle
        | ScalarFunction::ArraySample
        // be folded at optimiser time)
        | ScalarFunction::Now
        | ScalarFunction::CurrentTimestamp
        | ScalarFunction::CurrentDate
        | ScalarFunction::CurrentTime
        | ScalarFunction::Localtime
        | ScalarFunction::ClockTimestamp
        | ScalarFunction::StatementTimestamp
        | ScalarFunction::TransactionTimestamp
        // Set-returning functions are never foldable either
        | ScalarFunction::GenerateSeries
        | ScalarFunction::Unnest => true,

        // Generic functions: check the name for known volatile families.
        ScalarFunction::Generic(name) => is_volatile_generic_name(name),

        _ => false,
    }
}

/// Check generic function names for known volatile/non-deterministic
pub(super) fn is_volatile_generic_name(name: &str) -> bool {
    matches!(
        name,
        // Random / non-deterministic
        "gen_random_uuid"
        | "random_normal"
        | "setseed"
        // Time-dependent
        | "localtimestamp"
        | "timeofday"
        | "clock_timestamp"
        // Sequence functions (side effects)
        | "setval"
        | "currval"
        | "lastval"
        | "nextval"
        // Session / config state
        | "current_setting"
        | "row_security_active"
        | "set_config"
        | "pg_backend_pid"
        | "pg_get_userbyid"
        | "to_regclass"
        | "__aiondb_regclass_cast"
        | "__aiondb_regclass_out"
        | "to_regrole"
        | "__aiondb_regrole_cast"
        | "__aiondb_regrole_out"
    )
}
