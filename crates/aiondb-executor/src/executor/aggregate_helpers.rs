use aiondb_core::{DbError, DbResult, Row};
use aiondb_plan::*;

use crate::executor::{expr_contains_aggregate, exprs_structurally_equal};

pub(super) fn can_skip_scalar_group_input_scan(
    group_by: &[TypedExpr],
    aggregates: &[aiondb_plan::ProjectionExpr],
    having: Option<&TypedExpr>,
    order_by: &[aiondb_plan::SortExpr],
) -> bool {
    group_by.is_empty()
        && aggregates.iter().all(|projection| {
            !expr_contains_aggregate(&projection.expr)
                && !expr_contains_input_reference(&projection.expr)
        })
        && having.map_or(true, |expr| {
            !expr_contains_aggregate(expr) && !expr_contains_input_reference(expr)
        })
        && order_by.iter().all(|sort| {
            !expr_contains_aggregate(&sort.expr) && !expr_contains_input_reference(&sort.expr)
        })
}

pub(super) fn expr_contains_input_reference(expr: &TypedExpr) -> bool {
    match &expr.kind {
        TypedExprKind::ColumnRef { .. } | TypedExprKind::OuterColumnRef { .. } => true,
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
            expr_contains_input_reference(left) || expr_contains_input_reference(right)
        }
        TypedExprKind::LogicalNot { expr }
        | TypedExprKind::Negate { expr }
        | TypedExprKind::IsNull { expr, .. }
        | TypedExprKind::Cast { expr, .. } => expr_contains_input_reference(expr),
        TypedExprKind::Like { expr, pattern, .. } => {
            expr_contains_input_reference(expr) || expr_contains_input_reference(pattern)
        }
        TypedExprKind::InList { expr, list, .. } => {
            expr_contains_input_reference(expr) || list.iter().any(expr_contains_input_reference)
        }
        TypedExprKind::Between {
            expr, low, high, ..
        } => {
            expr_contains_input_reference(expr)
                || expr_contains_input_reference(low)
                || expr_contains_input_reference(high)
        }
        TypedExprKind::CaseWhen {
            conditions,
            results,
            else_result,
        } => {
            conditions.iter().any(expr_contains_input_reference)
                || results.iter().any(expr_contains_input_reference)
                || else_result
                    .as_ref()
                    .is_some_and(|expr| expr_contains_input_reference(expr))
        }
        TypedExprKind::Coalesce { args }
        | TypedExprKind::ScalarFunction { args, .. }
        | TypedExprKind::ArrayConstruct { elements: args }
        | TypedExprKind::UserFunction { args, .. }
        | TypedExprKind::WindowFunction { args, .. } => {
            args.iter().any(expr_contains_input_reference)
        }
        TypedExprKind::InSubquery { expr, .. } => expr_contains_input_reference(expr),
        TypedExprKind::Literal(_)
        | TypedExprKind::NextValue { .. }
        | TypedExprKind::AggCount { .. }
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
        | TypedExprKind::ScalarSubquery { .. }
        | TypedExprKind::ArraySubquery { .. }
        | TypedExprKind::ExistsSubquery { .. } => false,
    }
}

#[allow(clippy::struct_field_names)]
pub(super) struct GroupingOutputPlan {
    pub(super) output_group_by_match: Vec<Option<usize>>,
    pub(super) output_referenced_group_by: Vec<Vec<usize>>,
    pub(super) output_has_aggregate: Vec<bool>,
}

pub(super) fn build_grouping_output_plan(
    aggregates: &[aiondb_plan::ProjectionExpr],
    group_by: &[TypedExpr],
) -> GroupingOutputPlan {
    let output_group_by_match: Vec<Option<usize>> = aggregates
        .iter()
        .map(|proj| {
            group_by
                .iter()
                .position(|gb| exprs_structurally_equal(gb, &proj.expr))
        })
        .collect();

    let output_has_aggregate: Vec<bool> = aggregates
        .iter()
        .map(|proj| expr_contains_aggregate(&proj.expr))
        .collect();

    let output_referenced_group_by: Vec<Vec<usize>> = aggregates
        .iter()
        .enumerate()
        .map(|(out_idx, proj)| {
            if output_group_by_match[out_idx].is_some() || output_has_aggregate[out_idx] {
                return Vec::new();
            }
            group_by
                .iter()
                .enumerate()
                .filter_map(|(gb_idx, gb)| expr_contains_subexpr(&proj.expr, gb).then_some(gb_idx))
                .collect()
        })
        .collect();

    GroupingOutputPlan {
        output_group_by_match,
        output_referenced_group_by,
        output_has_aggregate,
    }
}

pub(super) fn build_active_group_positions(
    active_set: &[usize],
    group_by_len: usize,
) -> Vec<Option<usize>> {
    let mut positions = vec![None; group_by_len];
    for (active_pos, &gb_idx) in active_set.iter().enumerate() {
        if gb_idx < group_by_len {
            positions[gb_idx] = Some(active_pos);
        }
    }
    positions
}

/// Returns true if `haystack` contains `needle` as a sub-expression (or equals it).
pub(super) fn expr_contains_subexpr(haystack: &TypedExpr, needle: &TypedExpr) -> bool {
    if exprs_structurally_equal(haystack, needle) {
        return true;
    }
    match &haystack.kind {
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
            expr_contains_subexpr(left, needle) || expr_contains_subexpr(right, needle)
        }
        TypedExprKind::LogicalNot { expr }
        | TypedExprKind::Negate { expr }
        | TypedExprKind::IsNull { expr, .. }
        | TypedExprKind::Cast { expr, .. } => expr_contains_subexpr(expr, needle),
        TypedExprKind::Like { expr, pattern, .. } => {
            expr_contains_subexpr(expr, needle) || expr_contains_subexpr(pattern, needle)
        }
        TypedExprKind::InList { expr, list, .. } => {
            expr_contains_subexpr(expr, needle)
                || list.iter().any(|e| expr_contains_subexpr(e, needle))
        }
        TypedExprKind::Between {
            expr, low, high, ..
        } => {
            expr_contains_subexpr(expr, needle)
                || expr_contains_subexpr(low, needle)
                || expr_contains_subexpr(high, needle)
        }
        TypedExprKind::CaseWhen {
            conditions,
            results,
            else_result,
        } => {
            conditions.iter().any(|e| expr_contains_subexpr(e, needle))
                || results.iter().any(|e| expr_contains_subexpr(e, needle))
                || else_result
                    .as_ref()
                    .is_some_and(|e| expr_contains_subexpr(e, needle))
        }
        TypedExprKind::Coalesce { args }
        | TypedExprKind::ScalarFunction { args, .. }
        | TypedExprKind::ArrayConstruct { elements: args }
        | TypedExprKind::UserFunction { args, .. } => {
            args.iter().any(|e| expr_contains_subexpr(e, needle))
        }
        _ => false,
    }
}

pub(super) fn coerce_set_operation_rows(
    rows: Vec<Row>,
    output_fields: &[aiondb_plan::ResultField],
) -> DbResult<Vec<Row>> {
    rows.into_iter()
        .map(|row| {
            if row.values.len() != output_fields.len() {
                return Err(DbError::internal(
                    "set operation branch produced unexpected column count",
                ));
            }

            let values = row
                .values
                .into_iter()
                .zip(output_fields.iter())
                .map(|(value, field)| aiondb_eval::coerce_value(value, &field.data_type))
                .collect::<DbResult<Vec<_>>>()?;

            Ok(Row::new(values))
        })
        .collect()
}
