use aiondb_core::Row;
use aiondb_plan::*;

use super::expr::{expr_contains_outer_refs, substitute_outer_column_refs};

pub(crate) fn physical_plan_contains_outer_refs(plan: &PhysicalPlan) -> bool {
    match plan {
        PhysicalPlan::ProjectOnce {
            outputs,
            filter,
            order_by,
            limit,
            offset,
            distinct_on,
            ..
        } => {
            outputs
                .iter()
                .any(|output| expr_contains_outer_refs(&output.expr))
                || filter.as_ref().is_some_and(expr_contains_outer_refs)
                || order_by
                    .iter()
                    .any(|sort| expr_contains_outer_refs(&sort.expr))
                || limit.as_ref().is_some_and(expr_contains_outer_refs)
                || offset.as_ref().is_some_and(expr_contains_outer_refs)
                || distinct_on.iter().any(expr_contains_outer_refs)
        }
        PhysicalPlan::ProjectTable {
            outputs,
            filter,
            order_by,
            limit,
            offset,
            distinct_on,
            ..
        } => {
            outputs
                .iter()
                .any(|output| expr_contains_outer_refs(&output.expr))
                || filter.as_ref().is_some_and(expr_contains_outer_refs)
                || order_by
                    .iter()
                    .any(|sort| expr_contains_outer_refs(&sort.expr))
                || limit.as_ref().is_some_and(expr_contains_outer_refs)
                || offset.as_ref().is_some_and(expr_contains_outer_refs)
                || distinct_on.iter().any(expr_contains_outer_refs)
        }
        PhysicalPlan::ProjectSource {
            source,
            outputs,
            filter,
            order_by,
            limit,
            offset,
            distinct_on,
            ..
        } => {
            physical_plan_contains_outer_refs(source)
                || outputs
                    .iter()
                    .any(|output| expr_contains_outer_refs(&output.expr))
                || filter.as_ref().is_some_and(expr_contains_outer_refs)
                || order_by
                    .iter()
                    .any(|sort| expr_contains_outer_refs(&sort.expr))
                || limit.as_ref().is_some_and(expr_contains_outer_refs)
                || offset.as_ref().is_some_and(expr_contains_outer_refs)
                || distinct_on.iter().any(expr_contains_outer_refs)
        }
        PhysicalPlan::NestedLoopJoin {
            left,
            right,
            condition,
            outputs,
            filter,
            order_by,
            limit,
            offset,
            distinct_on,
            ..
        }
        | PhysicalPlan::HashJoin {
            left,
            right,
            condition,
            outputs,
            filter,
            order_by,
            limit,
            offset,
            distinct_on,
            ..
        } => {
            physical_plan_contains_outer_refs(left)
                || physical_plan_contains_outer_refs(right)
                || condition.as_ref().is_some_and(expr_contains_outer_refs)
                || outputs
                    .iter()
                    .any(|output| expr_contains_outer_refs(&output.expr))
                || filter.as_ref().is_some_and(expr_contains_outer_refs)
                || order_by
                    .iter()
                    .any(|sort| expr_contains_outer_refs(&sort.expr))
                || limit.as_ref().is_some_and(expr_contains_outer_refs)
                || offset.as_ref().is_some_and(expr_contains_outer_refs)
                || distinct_on.iter().any(expr_contains_outer_refs)
        }
        PhysicalPlan::MergeJoin {
            left,
            right,
            residual,
            outputs,
            filter,
            order_by,
            limit,
            offset,
            distinct_on,
            ..
        } => {
            physical_plan_contains_outer_refs(left)
                || physical_plan_contains_outer_refs(right)
                || residual.as_ref().is_some_and(expr_contains_outer_refs)
                || outputs
                    .iter()
                    .any(|output| expr_contains_outer_refs(&output.expr))
                || filter.as_ref().is_some_and(expr_contains_outer_refs)
                || order_by
                    .iter()
                    .any(|sort| expr_contains_outer_refs(&sort.expr))
                || limit.as_ref().is_some_and(expr_contains_outer_refs)
                || offset.as_ref().is_some_and(expr_contains_outer_refs)
                || distinct_on.iter().any(expr_contains_outer_refs)
        }
        PhysicalPlan::Aggregate {
            group_by,
            grouping_sets: _,
            aggregates,
            having,
            filter,
            order_by,
            limit,
            offset,
            distinct_on,
            ..
        } => {
            group_by.iter().any(expr_contains_outer_refs)
                || aggregates
                    .iter()
                    .any(|output| expr_contains_outer_refs(&output.expr))
                || having.as_ref().is_some_and(expr_contains_outer_refs)
                || filter.as_ref().is_some_and(expr_contains_outer_refs)
                || order_by
                    .iter()
                    .any(|sort| expr_contains_outer_refs(&sort.expr))
                || limit.as_ref().is_some_and(expr_contains_outer_refs)
                || offset.as_ref().is_some_and(expr_contains_outer_refs)
                || distinct_on.iter().any(expr_contains_outer_refs)
        }
        PhysicalPlan::AggregateSource {
            source,
            group_by,
            grouping_sets: _,
            aggregates,
            having,
            filter,
            order_by,
            limit,
            offset,
            distinct_on,
            ..
        } => {
            physical_plan_contains_outer_refs(source)
                || group_by.iter().any(expr_contains_outer_refs)
                || aggregates
                    .iter()
                    .any(|output| expr_contains_outer_refs(&output.expr))
                || having.as_ref().is_some_and(expr_contains_outer_refs)
                || filter.as_ref().is_some_and(expr_contains_outer_refs)
                || order_by
                    .iter()
                    .any(|sort| expr_contains_outer_refs(&sort.expr))
                || limit.as_ref().is_some_and(expr_contains_outer_refs)
                || offset.as_ref().is_some_and(expr_contains_outer_refs)
                || distinct_on.iter().any(expr_contains_outer_refs)
        }
        PhysicalPlan::SetOperation {
            left,
            right,
            order_by,
            limit,
            offset,
            ..
        } => {
            physical_plan_contains_outer_refs(left)
                || physical_plan_contains_outer_refs(right)
                || order_by
                    .iter()
                    .any(|sort| expr_contains_outer_refs(&sort.expr))
                || limit.as_ref().is_some_and(expr_contains_outer_refs)
                || offset.as_ref().is_some_and(expr_contains_outer_refs)
        }
        PhysicalPlan::DistributedAppend {
            fragments,
            order_by,
            limit,
            offset,
            ..
        } => {
            fragments.iter().any(physical_plan_contains_outer_refs)
                || order_by
                    .iter()
                    .any(|sort| expr_contains_outer_refs(&sort.expr))
                || limit.as_ref().is_some_and(expr_contains_outer_refs)
                || offset.as_ref().is_some_and(expr_contains_outer_refs)
        }
        PhysicalPlan::ProjectValues {
            rows,
            order_by,
            limit,
            offset,
            ..
        } => {
            rows.iter().flatten().any(expr_contains_outer_refs)
                || order_by
                    .iter()
                    .any(|sort| expr_contains_outer_refs(&sort.expr))
                || limit.as_ref().is_some_and(expr_contains_outer_refs)
                || offset.as_ref().is_some_and(expr_contains_outer_refs)
        }
        PhysicalPlan::RecursiveCte {
            base, recursive, ..
        } => {
            physical_plan_contains_outer_refs(base) || physical_plan_contains_outer_refs(recursive)
        }
        PhysicalPlan::HybridFunctionScan { args, .. } => args.iter().any(expr_contains_outer_refs),
        _ => false,
    }
}

pub(crate) fn substitute_outer_refs_in_physical_plan(
    plan: &PhysicalPlan,
    outer_row: &Row,
) -> PhysicalPlan {
    match plan {
        PhysicalPlan::ProjectOnce {
            outputs,
            filter,
            order_by,
            limit,
            offset,
            distinct,
            distinct_on,
        } => PhysicalPlan::ProjectOnce {
            outputs: outputs
                .iter()
                .map(|output| aiondb_plan::ProjectionExpr {
                    field: output.field.clone(),
                    expr: substitute_outer_column_refs(&output.expr, outer_row),
                })
                .collect(),
            filter: filter
                .as_ref()
                .map(|expr| substitute_outer_column_refs(expr, outer_row)),
            order_by: order_by
                .iter()
                .map(|sort| aiondb_plan::SortExpr {
                    expr: substitute_outer_column_refs(&sort.expr, outer_row),
                    descending: sort.descending,
                    nulls_first: sort.nulls_first,
                })
                .collect(),
            limit: limit
                .as_ref()
                .map(|expr| substitute_outer_column_refs(expr, outer_row)),
            offset: offset
                .as_ref()
                .map(|expr| substitute_outer_column_refs(expr, outer_row)),
            distinct: *distinct,
            distinct_on: distinct_on
                .iter()
                .map(|expr| substitute_outer_column_refs(expr, outer_row))
                .collect(),
        },
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
        } => {
            let new_filter = filter
                .as_ref()
                .map(|expr| substitute_outer_column_refs(expr, outer_row));
            // When the filter had outer refs that have been replaced with
            // concrete values, update the access path so index lookups use
            // the new value instead of the stale one from the original
            // compilation.
            let new_access_path =
                rewrite_access_path_for_substituted_filter(access_path, new_filter.as_ref());
            PhysicalPlan::ProjectTable {
                table_id: *table_id,
                outputs: outputs
                    .iter()
                    .map(|output| aiondb_plan::ProjectionExpr {
                        field: output.field.clone(),
                        expr: substitute_outer_column_refs(&output.expr, outer_row),
                    })
                    .collect(),
                filter: new_filter,
                order_by: order_by
                    .iter()
                    .map(|sort| aiondb_plan::SortExpr {
                        expr: substitute_outer_column_refs(&sort.expr, outer_row),
                        descending: sort.descending,
                        nulls_first: sort.nulls_first,
                    })
                    .collect(),
                limit: limit
                    .as_ref()
                    .map(|expr| substitute_outer_column_refs(expr, outer_row)),
                offset: offset
                    .as_ref()
                    .map(|expr| substitute_outer_column_refs(expr, outer_row)),
                distinct: *distinct,
                distinct_on: distinct_on
                    .iter()
                    .map(|expr| substitute_outer_column_refs(expr, outer_row))
                    .collect(),
                access_path: new_access_path,
            }
        }
        PhysicalPlan::ProjectSource {
            source,
            outputs,
            filter,
            order_by,
            limit,
            offset,
            distinct,
            distinct_on,
        } => PhysicalPlan::ProjectSource {
            source: Box::new(substitute_outer_refs_in_physical_plan(source, outer_row)),
            outputs: outputs
                .iter()
                .map(|output| aiondb_plan::ProjectionExpr {
                    field: output.field.clone(),
                    expr: substitute_outer_column_refs(&output.expr, outer_row),
                })
                .collect(),
            filter: filter
                .as_ref()
                .map(|expr| substitute_outer_column_refs(expr, outer_row)),
            order_by: order_by
                .iter()
                .map(|sort| aiondb_plan::SortExpr {
                    expr: substitute_outer_column_refs(&sort.expr, outer_row),
                    descending: sort.descending,
                    nulls_first: sort.nulls_first,
                })
                .collect(),
            limit: limit
                .as_ref()
                .map(|expr| substitute_outer_column_refs(expr, outer_row)),
            offset: offset
                .as_ref()
                .map(|expr| substitute_outer_column_refs(expr, outer_row)),
            distinct: *distinct,
            distinct_on: distinct_on
                .iter()
                .map(|expr| substitute_outer_column_refs(expr, outer_row))
                .collect(),
        },
        PhysicalPlan::HybridFunctionScan {
            function_name,
            args,
            output_fields,
        } => PhysicalPlan::HybridFunctionScan {
            function_name: function_name.clone(),
            args: args
                .iter()
                .map(|expr| substitute_outer_column_refs(expr, outer_row))
                .collect(),
            output_fields: output_fields.clone(),
        },
        PhysicalPlan::NestedLoopJoin {
            left,
            right,
            join_type,
            condition,
            outputs,
            filter,
            order_by,
            limit,
            offset,
            distinct,
            distinct_on,
        } => PhysicalPlan::NestedLoopJoin {
            left: Box::new(substitute_outer_refs_in_physical_plan(left, outer_row)),
            right: Box::new(substitute_outer_refs_in_physical_plan(right, outer_row)),
            join_type: *join_type,
            condition: condition
                .as_ref()
                .map(|expr| substitute_outer_column_refs(expr, outer_row)),
            outputs: outputs
                .iter()
                .map(|output| aiondb_plan::ProjectionExpr {
                    field: output.field.clone(),
                    expr: substitute_outer_column_refs(&output.expr, outer_row),
                })
                .collect(),
            filter: filter
                .as_ref()
                .map(|expr| substitute_outer_column_refs(expr, outer_row)),
            order_by: order_by
                .iter()
                .map(|sort| aiondb_plan::SortExpr {
                    expr: substitute_outer_column_refs(&sort.expr, outer_row),
                    descending: sort.descending,
                    nulls_first: sort.nulls_first,
                })
                .collect(),
            limit: limit
                .as_ref()
                .map(|expr| substitute_outer_column_refs(expr, outer_row)),
            offset: offset
                .as_ref()
                .map(|expr| substitute_outer_column_refs(expr, outer_row)),
            distinct: *distinct,
            distinct_on: distinct_on
                .iter()
                .map(|expr| substitute_outer_column_refs(expr, outer_row))
                .collect(),
        },
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
        } => PhysicalPlan::HashJoin {
            left: Box::new(substitute_outer_refs_in_physical_plan(left, outer_row)),
            right: Box::new(substitute_outer_refs_in_physical_plan(right, outer_row)),
            join_type: *join_type,
            left_keys: left_keys.clone(),
            right_keys: right_keys.clone(),
            condition: condition
                .as_ref()
                .map(|expr| substitute_outer_column_refs(expr, outer_row)),
            outputs: outputs
                .iter()
                .map(|output| aiondb_plan::ProjectionExpr {
                    field: output.field.clone(),
                    expr: substitute_outer_column_refs(&output.expr, outer_row),
                })
                .collect(),
            filter: filter
                .as_ref()
                .map(|expr| substitute_outer_column_refs(expr, outer_row)),
            order_by: order_by
                .iter()
                .map(|sort| aiondb_plan::SortExpr {
                    expr: substitute_outer_column_refs(&sort.expr, outer_row),
                    descending: sort.descending,
                    nulls_first: sort.nulls_first,
                })
                .collect(),
            limit: limit
                .as_ref()
                .map(|expr| substitute_outer_column_refs(expr, outer_row)),
            offset: offset
                .as_ref()
                .map(|expr| substitute_outer_column_refs(expr, outer_row)),
            distinct: *distinct,
            distinct_on: distinct_on
                .iter()
                .map(|expr| substitute_outer_column_refs(expr, outer_row))
                .collect(),
        },
        PhysicalPlan::MergeJoin {
            left,
            right,
            join_type,
            left_keys,
            right_keys,
            residual,
            outputs,
            filter,
            order_by,
            limit,
            offset,
            distinct,
            distinct_on,
        } => PhysicalPlan::MergeJoin {
            left: Box::new(substitute_outer_refs_in_physical_plan(left, outer_row)),
            right: Box::new(substitute_outer_refs_in_physical_plan(right, outer_row)),
            join_type: *join_type,
            left_keys: left_keys.clone(),
            right_keys: right_keys.clone(),
            residual: residual
                .as_ref()
                .map(|expr| substitute_outer_column_refs(expr, outer_row)),
            outputs: outputs
                .iter()
                .map(|output| aiondb_plan::ProjectionExpr {
                    field: output.field.clone(),
                    expr: substitute_outer_column_refs(&output.expr, outer_row),
                })
                .collect(),
            filter: filter
                .as_ref()
                .map(|expr| substitute_outer_column_refs(expr, outer_row)),
            order_by: order_by
                .iter()
                .map(|sort| aiondb_plan::SortExpr {
                    expr: substitute_outer_column_refs(&sort.expr, outer_row),
                    descending: sort.descending,
                    nulls_first: sort.nulls_first,
                })
                .collect(),
            limit: limit
                .as_ref()
                .map(|expr| substitute_outer_column_refs(expr, outer_row)),
            offset: offset
                .as_ref()
                .map(|expr| substitute_outer_column_refs(expr, outer_row)),
            distinct: *distinct,
            distinct_on: distinct_on
                .iter()
                .map(|expr| substitute_outer_column_refs(expr, outer_row))
                .collect(),
        },
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
            distinct_on,
            access_path,
        } => PhysicalPlan::Aggregate {
            table_id: *table_id,
            group_by: group_by
                .iter()
                .map(|expr| substitute_outer_column_refs(expr, outer_row))
                .collect(),
            grouping_sets: grouping_sets.clone(),
            aggregates: aggregates
                .iter()
                .map(|output| aiondb_plan::ProjectionExpr {
                    field: output.field.clone(),
                    expr: substitute_outer_column_refs(&output.expr, outer_row),
                })
                .collect(),
            having: having
                .as_ref()
                .map(|expr| substitute_outer_column_refs(expr, outer_row)),
            filter: filter
                .as_ref()
                .map(|expr| substitute_outer_column_refs(expr, outer_row)),
            order_by: order_by
                .iter()
                .map(|sort| aiondb_plan::SortExpr {
                    expr: substitute_outer_column_refs(&sort.expr, outer_row),
                    descending: sort.descending,
                    nulls_first: sort.nulls_first,
                })
                .collect(),
            limit: limit
                .as_ref()
                .map(|expr| substitute_outer_column_refs(expr, outer_row)),
            offset: offset
                .as_ref()
                .map(|expr| substitute_outer_column_refs(expr, outer_row)),
            distinct: *distinct,
            distinct_on: distinct_on
                .iter()
                .map(|expr| substitute_outer_column_refs(expr, outer_row))
                .collect(),
            access_path: access_path.clone(),
        },
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
            distinct_on,
        } => PhysicalPlan::AggregateSource {
            source: Box::new(substitute_outer_refs_in_physical_plan(source, outer_row)),
            group_by: group_by
                .iter()
                .map(|expr| substitute_outer_column_refs(expr, outer_row))
                .collect(),
            grouping_sets: grouping_sets.clone(),
            aggregates: aggregates
                .iter()
                .map(|output| aiondb_plan::ProjectionExpr {
                    field: output.field.clone(),
                    expr: substitute_outer_column_refs(&output.expr, outer_row),
                })
                .collect(),
            having: having
                .as_ref()
                .map(|expr| substitute_outer_column_refs(expr, outer_row)),
            filter: filter
                .as_ref()
                .map(|expr| substitute_outer_column_refs(expr, outer_row)),
            order_by: order_by
                .iter()
                .map(|sort| aiondb_plan::SortExpr {
                    expr: substitute_outer_column_refs(&sort.expr, outer_row),
                    descending: sort.descending,
                    nulls_first: sort.nulls_first,
                })
                .collect(),
            limit: limit
                .as_ref()
                .map(|expr| substitute_outer_column_refs(expr, outer_row)),
            offset: offset
                .as_ref()
                .map(|expr| substitute_outer_column_refs(expr, outer_row)),
            distinct: *distinct,
            distinct_on: distinct_on
                .iter()
                .map(|expr| substitute_outer_column_refs(expr, outer_row))
                .collect(),
        },
        PhysicalPlan::SetOperation {
            op,
            all,
            left,
            right,
            output_fields,
            order_by,
            limit,
            offset,
        } => PhysicalPlan::SetOperation {
            op: *op,
            all: *all,
            left: Box::new(substitute_outer_refs_in_physical_plan(left, outer_row)),
            right: Box::new(substitute_outer_refs_in_physical_plan(right, outer_row)),
            output_fields: output_fields.clone(),
            order_by: order_by
                .iter()
                .map(|sort| aiondb_plan::SortExpr {
                    expr: substitute_outer_column_refs(&sort.expr, outer_row),
                    descending: sort.descending,
                    nulls_first: sort.nulls_first,
                })
                .collect(),
            limit: limit
                .as_ref()
                .map(|expr| substitute_outer_column_refs(expr, outer_row)),
            offset: offset
                .as_ref()
                .map(|expr| substitute_outer_column_refs(expr, outer_row)),
        },
        PhysicalPlan::DistributedAppend {
            fragments,
            output_fields,
            order_by,
            limit,
            offset,
        } => PhysicalPlan::DistributedAppend {
            fragments: fragments
                .iter()
                .map(|fragment| substitute_outer_refs_in_physical_plan(fragment, outer_row))
                .collect(),
            output_fields: output_fields.clone(),
            order_by: order_by
                .iter()
                .map(|sort| aiondb_plan::SortExpr {
                    expr: substitute_outer_column_refs(&sort.expr, outer_row),
                    descending: sort.descending,
                    nulls_first: sort.nulls_first,
                })
                .collect(),
            limit: limit
                .as_ref()
                .map(|expr| substitute_outer_column_refs(expr, outer_row)),
            offset: offset
                .as_ref()
                .map(|expr| substitute_outer_column_refs(expr, outer_row)),
        },
        PhysicalPlan::ProjectValues {
            output_fields,
            rows,
            order_by,
            limit,
            offset,
        } => PhysicalPlan::ProjectValues {
            output_fields: output_fields.clone(),
            rows: rows
                .iter()
                .map(|row| {
                    row.iter()
                        .map(|expr| substitute_outer_column_refs(expr, outer_row))
                        .collect()
                })
                .collect(),
            order_by: order_by
                .iter()
                .map(|sort| aiondb_plan::SortExpr {
                    expr: substitute_outer_column_refs(&sort.expr, outer_row),
                    descending: sort.descending,
                    nulls_first: sort.nulls_first,
                })
                .collect(),
            limit: limit
                .as_ref()
                .map(|expr| substitute_outer_column_refs(expr, outer_row)),
            offset: offset
                .as_ref()
                .map(|expr| substitute_outer_column_refs(expr, outer_row)),
        },
        PhysicalPlan::RecursiveCte {
            base,
            recursive,
            union_all,
            output_fields,
        } => PhysicalPlan::RecursiveCte {
            base: Box::new(substitute_outer_refs_in_physical_plan(base, outer_row)),
            recursive: Box::new(substitute_outer_refs_in_physical_plan(recursive, outer_row)),
            union_all: *union_all,
            output_fields: output_fields.clone(),
        },
        other => other.clone(),
    }
}

/// When a `ProjectTable` access path was chosen as `IndexEq` or
/// `IndexEqComposite` based on a previous literal value in the filter,
/// update the lookup value(s) to match the new (substituted) filter.
///
/// This keeps index lookups correct after outer-ref substitution without
/// re-running the full optimizer.
fn rewrite_access_path_for_substituted_filter(
    access_path: &ScanAccessPath,
    filter: Option<&TypedExpr>,
) -> ScanAccessPath {
    match access_path {
        ScanAccessPath::IndexEq { index_id, .. } => {
            if let Some(new_value) = extract_first_literal_from_eq(filter) {
                ScanAccessPath::IndexEq {
                    index_id: *index_id,
                    value: new_value,
                }
            } else {
                access_path.clone()
            }
        }
        ScanAccessPath::IndexEqComposite { index_id, values } => {
            let new_values = extract_literals_from_eq_chain(filter, values.len());
            if new_values.len() == values.len() {
                ScanAccessPath::IndexEqComposite {
                    index_id: *index_id,
                    values: new_values,
                }
            } else {
                access_path.clone()
            }
        }
        _ => access_path.clone(),
    }
}

/// Extract the first `Literal` value from a `BinaryEq` or the first
/// equality in a `LogicalAnd` chain.
fn extract_first_literal_from_eq(filter: Option<&TypedExpr>) -> Option<aiondb_core::Value> {
    let filter = filter?;
    match &filter.kind {
        TypedExprKind::BinaryEq { left, right } => {
            extract_literal(right).or_else(|| extract_literal(left))
        }
        TypedExprKind::LogicalAnd { left, right } => extract_first_literal_from_eq(Some(left))
            .or_else(|| extract_first_literal_from_eq(Some(right))),
        _ => None,
    }
}

/// Extract up to `count` literal values from equality predicates in a
/// `LogicalAnd` chain.
fn extract_literals_from_eq_chain(
    filter: Option<&TypedExpr>,
    count: usize,
) -> Vec<aiondb_core::Value> {
    let mut result = Vec::with_capacity(count);
    collect_eq_literals(filter, &mut result, count);
    result
}

fn collect_eq_literals(expr: Option<&TypedExpr>, out: &mut Vec<aiondb_core::Value>, max: usize) {
    if out.len() >= max {
        return;
    }
    let Some(expr) = expr else { return };
    match &expr.kind {
        TypedExprKind::BinaryEq { left, right } => {
            if let Some(val) = extract_literal(right).or_else(|| extract_literal(left)) {
                out.push(val);
            }
        }
        TypedExprKind::LogicalAnd { left, right } => {
            collect_eq_literals(Some(left), out, max);
            collect_eq_literals(Some(right), out, max);
        }
        _ => {}
    }
}

fn extract_literal(expr: &TypedExpr) -> Option<aiondb_core::Value> {
    match &expr.kind {
        TypedExprKind::Literal(val) => Some(val.clone()),
        _ => None,
    }
}
