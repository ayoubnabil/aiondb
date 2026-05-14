//! Projection pruning pass.
//!
//! When a `ProjectSource` has explicit outputs that reference only a
//! subset of the child's columns, we can narrow the child's output to
//! only the needed columns.  This reduces tuple width flowing through
//! joins and sorts, saving memory and CPU.
//!
//! This mirrors PostgreSQL's projection management in
//! `create_plan_recurse()` where unused columns are dropped early.

use aiondb_plan::{LogicalPlan, ProjectionExpr, TypedExpr, TypedExprKind};

use crate::predicate_pushdown;

/// Attempt to push a narrower projection into the child of a
/// `ProjectSource` when the parent only uses a subset of columns.
///
/// Returns the (possibly modified) child plan.
pub(crate) fn try_prune_child_projection(
    source: LogicalPlan,
    parent_outputs: &[ProjectionExpr],
    parent_filter: &Option<TypedExpr>,
    parent_order_by: &[aiondb_plan::SortExpr],
    parent_distinct_on: &[TypedExpr],
) -> LogicalPlan {
    // Only prune when the parent has explicit output projections and
    // the parent references a contiguous prefix of child columns.
    // Pruning from the middle would require ordinal remapping in the
    // parent, which is complex. Instead, we only prune trailing
    // columns that the parent never references.
    if parent_outputs.is_empty() {
        return source;
    }

    // Collect all column ordinals referenced by parent expressions.
    let mut needed_ordinals = std::collections::HashSet::new();
    for proj in parent_outputs {
        collect_column_refs(&proj.expr, &mut needed_ordinals);
    }
    if let Some(ref f) = parent_filter {
        collect_column_refs(f, &mut needed_ordinals);
    }
    for sort in parent_order_by {
        collect_column_refs(&sort.expr, &mut needed_ordinals);
    }
    for expr in parent_distinct_on {
        collect_column_refs(expr, &mut needed_ordinals);
    }

    match source {
        LogicalPlan::ProjectTable {
            table_id,
            outputs,
            filter,
            order_by,
            limit,
            offset,
            distinct,
            distinct_on,
        } if !outputs.is_empty() && !distinct && distinct_on.is_empty() => {
            // The child has explicit outputs and is NOT performing
            // deduplication. (When DISTINCT is active, all output
            // columns participate in the dedup key - pruning any of
            // them would change which rows survive and alter results.)
            //
            // We can safely truncate trailing columns that are not
            // the middle because that would shift ordinals in parent
            // expressions.
            let child_width = outputs.len();

            // Also include ordinals referenced by the child's own
            // filter, order_by, and distinct_on.
            let mut all_needed = needed_ordinals;
            if let Some(ref f) = filter {
                collect_column_refs(f, &mut all_needed);
            }
            for sort in &order_by {
                collect_column_refs(&sort.expr, &mut all_needed);
            }
            for expr in &distinct_on {
                collect_column_refs(expr, &mut all_needed);
            }

            // Find the highest needed ordinal within the child's range.
            // When `all_needed` is empty (parent only references literals
            // / aggregates with no column input), we must keep zero
            // columns from the child rather than `unwrap_or(0)` then
            // keeping `keep_count == 1`, which would smuggle a stray
            // column into a projection that should logically be empty.
            let max_needed_opt = all_needed
                .iter()
                .filter(|&&ord| ord < child_width)
                .max()
                .copied();

            let keep_count = match max_needed_opt {
                Some(max_needed) => (max_needed + 1).min(child_width),
                // No needed column: keep one as the smallest valid
                // projection (zero-column projections aren't representable
                // downstream). Prefer column 0 as a deterministic anchor.
                None => 1.min(child_width),
            };
            if keep_count >= child_width {
                return LogicalPlan::ProjectTable {
                    table_id,
                    outputs,
                    filter,
                    order_by,
                    limit,
                    offset,
                    distinct,
                    distinct_on,
                };
            }

            // Truncate trailing unused columns.
            let pruned_outputs: Vec<ProjectionExpr> =
                outputs.into_iter().take(keep_count).collect();

            LogicalPlan::ProjectTable {
                table_id,
                outputs: pruned_outputs,
                filter,
                order_by,
                limit,
                offset,
                distinct,
                distinct_on,
            }
        }
        other => other,
    }
}

/// Collect all `ColumnRef` ordinals from an expression tree.
fn collect_column_refs(expr: &TypedExpr, ordinals: &mut std::collections::HashSet<usize>) {
    let mut stack = vec![expr];
    while let Some(expr) = stack.pop() {
        match &expr.kind {
            TypedExprKind::ColumnRef { ordinal, .. } => {
                ordinals.insert(*ordinal);
            }
            _ => predicate_pushdown::for_each_child_expr(expr, &mut |child| {
                stack.push(child);
            }),
        }
    }
}
