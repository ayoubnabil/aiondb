//! Outer-join simplification pass.
//!
//! When a `WHERE` filter contains a *strict* predicate on the nullable
//! (null-extended) side of an outer join, the join can be converted to a
//! narrower join type - ultimately to `INNER` - because the predicate
//! eliminates the null-padded rows the outer join would produce.
//!
//! This mirrors PostgreSQL's `reduce_outer_joins()` in `prepjointree.c`.

use std::sync::Arc;

use aiondb_catalog::CatalogReader;
use aiondb_core::{DbResult, TxnId};
use aiondb_plan::{JoinType, LogicalPlan, TypedExpr, TypedExprKind};

use crate::predicate_pushdown;

/// Attempt to convert outer joins to inner joins when the WHERE filter
/// contains strict predicates on the nullable side.
///
/// Returns the (possibly simplified) plan.
pub(crate) fn simplify_outer_joins(
    plan: LogicalPlan,
    catalog_reader: &Arc<dyn CatalogReader>,
    txn_id: TxnId,
) -> DbResult<LogicalPlan> {
    let LogicalPlan::NestedLoopJoin {
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
    } = plan
    else {
        return Ok(plan);
    };

    // Already an inner join - nothing to simplify.
    if join_type == JoinType::Inner {
        return Ok(LogicalPlan::NestedLoopJoin {
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
        });
    }

    // No WHERE filter means we cannot eliminate null-extended rows.
    let Some(ref filter_expr) = filter else {
        return Ok(LogicalPlan::NestedLoopJoin {
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
        });
    };

    // Compute the column width of the left child.
    let left_width =
        match predicate_pushdown::logical_plan_child_width(&left, catalog_reader, txn_id)? {
            Some(w) => w,
            None => {
                return Ok(LogicalPlan::NestedLoopJoin {
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
                });
            }
        };

    // Decompose the filter into AND conjuncts.
    let mut conjuncts = Vec::new();
    predicate_pushdown::collect_conjuncts_ref(filter_expr, &mut conjuncts);

    // Check for strict predicates on each side.
    let has_strict_on_left = conjuncts
        .iter()
        .any(|c| references_left(c, left_width) && is_strict_predicate(c));
    let has_strict_on_right = conjuncts
        .iter()
        .any(|c| references_right(c, left_width) && is_strict_predicate(c));

    let new_join_type = match join_type {
        JoinType::Left => {
            if has_strict_on_right {
                JoinType::Inner
            } else {
                JoinType::Left
            }
        }
        JoinType::Right => {
            if has_strict_on_left {
                JoinType::Inner
            } else {
                JoinType::Right
            }
        }
        JoinType::Full => match (has_strict_on_left, has_strict_on_right) {
            (true, true) => JoinType::Inner,
            (false, true) => JoinType::Left,
            (true, false) => JoinType::Right,
            (false, false) => JoinType::Full,
        },
        JoinType::Inner | JoinType::Semi | JoinType::Anti => join_type,
    };

    Ok(LogicalPlan::NestedLoopJoin {
        left,
        right,
        join_type: new_join_type,
        condition,
        outputs,
        filter,
        order_by,
        limit,
        offset,
        distinct,
        distinct_on,
    })
}

// ------------------------------------------------------------------
// Strict predicate detection
// ------------------------------------------------------------------

/// A predicate is "strict" if it returns FALSE or NULL whenever any of
/// its column-reference inputs evaluates to NULL.  Strict predicates
/// reject null-extended rows, making the outer join equivalent to an
/// inner join on that side.
fn is_strict_predicate(expr: &TypedExpr) -> bool {
    match &expr.kind {
        // Comparisons: strict only when at least one operand is
        // NULL-propagating (has a bare column-ref path that would
        // produce NULL on null-extended rows). If both operands are
        // wrapped in NULL-masking functions like COALESCE, the
        // comparison can still succeed on NULL rows and is NOT strict.
        TypedExprKind::BinaryEq { left, right }
        | TypedExprKind::BinaryNe { left, right }
        | TypedExprKind::BinaryGt { left, right }
        | TypedExprKind::BinaryGe { left, right }
        | TypedExprKind::BinaryLt { left, right }
        | TypedExprKind::BinaryLe { left, right } => {
            is_null_propagating(left) || is_null_propagating(right)
        }

        // IS NOT NULL is strict by definition.
        TypedExprKind::IsNull {
            negated: true,
            expr,
        } => is_null_propagating(expr),

        // IS NULL is NOT strict - it returns TRUE when input is NULL.
        TypedExprKind::IsNull { negated: false, .. } => false,

        // LIKE / ILIKE: strict only when the tested expr propagates NULL.
        TypedExprKind::Like { expr, .. } => is_null_propagating(expr),

        // BETWEEN: strict only when the tested expr propagates NULL.
        TypedExprKind::Between { expr, .. } => is_null_propagating(expr),

        // IN (list): strict only when the tested expr propagates NULL.
        TypedExprKind::InList { expr, .. } => is_null_propagating(expr),

        // IS DISTINCT FROM never yields NULL (always TRUE/FALSE),
        // but IS NOT DISTINCT FROM (negated=true) returns TRUE when
        // both sides are NULL, so it is NOT strict.
        TypedExprKind::IsDistinctFrom { negated, .. } => !negated,

        // AND: strict if either child is strict (short-circuit to
        // FALSE/NULL when one side rejects NULL rows).
        TypedExprKind::LogicalAnd { left, right } => {
            is_strict_predicate(left) || is_strict_predicate(right)
        }

        // OR: strict only if BOTH children are strict (all branches
        // must reject NULL rows).
        TypedExprKind::LogicalOr { left, right } => {
            is_strict_predicate(left) && is_strict_predicate(right)
        }

        // NOT: strict if inner is strict, OR if inner is `IS NULL` (since
        // `NOT (col IS NULL)` ≡ `col IS NOT NULL`, which is itself strict
        // and rejects NULL rows for outer-join simplification).
        TypedExprKind::LogicalNot { expr } => {
            if matches!(expr.kind, TypedExprKind::IsNull { negated: false, .. }) {
                if let TypedExprKind::IsNull { expr: inner, .. } = &expr.kind {
                    return is_null_propagating(inner);
                }
            }
            is_strict_predicate(expr)
        }

        // CAST: strict if inner is strict.
        TypedExprKind::Cast { expr, .. } => is_strict_predicate(expr),

        // Scalar functions: conservatively not strict. Some functions
        // (concat, concat_ws, format) handle NULLs gracefully rather
        // than propagating them. Without a per-function strictness
        // catalog we cannot safely assume strictness.
        TypedExprKind::ScalarFunction { .. } => false,

        // COALESCE handles NULLs - not strict.
        TypedExprKind::Coalesce { .. } => false,

        // CASE WHEN may have NULL-safe branches - conservatively not strict.
        TypedExprKind::CaseWhen { .. } => false,

        // NULLIF can return NULL intentionally - not strict.
        TypedExprKind::Nullif { .. } => false,

        // Literals have no column dependency - not strict.
        TypedExprKind::Literal(_) => false,

        // Column references alone are not predicates, not strict.
        TypedExprKind::ColumnRef { .. } => false,

        // Subqueries: conservatively not strict.
        TypedExprKind::ExistsSubquery { .. }
        | TypedExprKind::InSubquery { .. }
        | TypedExprKind::ScalarSubquery { .. }
        | TypedExprKind::ArraySubquery { .. } => false,

        // Default: conservatively not strict.
        _ => false,
    }
}

/// An expression is "NULL-propagating" if it will produce NULL when
/// any of its column-reference inputs is NULL.  This is used to verify
/// that a comparison operand actually exposes the null-ness of the
/// join's nullable side rather than masking it (e.g. `COALESCE`).
fn is_null_propagating(expr: &TypedExpr) -> bool {
    match &expr.kind {
        // A bare column reference propagates NULL directly.
        TypedExprKind::ColumnRef { .. } => true,

        // CAST preserves NULL.
        TypedExprKind::Cast { expr, .. } => is_null_propagating(expr),

        // Arithmetic propagates NULL through both operands.
        TypedExprKind::ArithAdd { left, right }
        | TypedExprKind::ArithSub { left, right }
        | TypedExprKind::ArithMul { left, right }
        | TypedExprKind::ArithDiv { left, right }
        | TypedExprKind::ArithMod { left, right }
        | TypedExprKind::Concat { left, right } => {
            is_null_propagating(left) || is_null_propagating(right)
        }

        // Negation preserves NULL.
        TypedExprKind::Negate { expr } => is_null_propagating(expr),

        // Scalar functions generally propagate NULL (NULL in → NULL out).
        TypedExprKind::ScalarFunction { args, .. } => args.iter().any(is_null_propagating),

        // COALESCE masks NULLs - NOT null-propagating.
        TypedExprKind::Coalesce { .. } => false,

        // CASE WHEN can mask NULLs - NOT null-propagating.
        TypedExprKind::CaseWhen { .. } => false,

        // NULLIF can return NULL for reasons other than input NULL.
        TypedExprKind::Nullif { .. } => false,

        // Literals never propagate column NULLs.
        TypedExprKind::Literal(_) => false,

        // Default: conservatively not null-propagating.
        _ => false,
    }
}

// ------------------------------------------------------------------
// Side-reference detection
// ------------------------------------------------------------------

/// Check if the expression references any column on the left side
/// (ordinal < left_width).
fn references_left(expr: &TypedExpr, left_width: usize) -> bool {
    let mut has_left = false;
    let mut has_right = false;
    collect_sides(expr, left_width, &mut has_left, &mut has_right);
    has_left
}

/// Check if the expression references any column on the right side
/// (ordinal >= left_width).
fn references_right(expr: &TypedExpr, left_width: usize) -> bool {
    let mut has_left = false;
    let mut has_right = false;
    collect_sides(expr, left_width, &mut has_left, &mut has_right);
    has_right
}

use predicate_pushdown::collect_sides;
