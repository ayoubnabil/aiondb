//! Transitive predicate generation.
//!
//! When a join condition contains equi-pairs (`a.x = b.y`) and the
//! WHERE filter contains single-table predicates involving one side of
//! the pair (`b.y = 5`), we can infer an equivalent predicate for the
//! other side (`a.x = 5`).  The inferred predicates are added to the
//! join's filter so that predicate pushdown can push them into the
//! corresponding child, potentially enabling index lookups.
//!
//! This mirrors PostgreSQL's equivalence class machinery in
//! `equivclass.c` / `generate_implied_equalities()`.

use std::sync::Arc;

use aiondb_catalog::CatalogReader;
use aiondb_core::{DbResult, TxnId};
use aiondb_plan::{JoinType, LogicalPlan, TypedExpr, TypedExprKind};

use crate::predicate_pushdown;

/// Enrich a `NestedLoopJoin` plan with transitively derived predicates.
///
/// For each equi-pair `(left_ord, right_ord)` from the ON condition and
/// each `col_ord op literal` predicate in the WHERE filter, if `col_ord`
/// matches one side of the pair, a new `partner_ord op literal`
/// predicate is generated for the other side and added to the filter.
///
/// Returns the (possibly enriched) plan.
pub(crate) fn enrich_with_transitive_predicates(
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

    // Transitive inference is only safe for INNER joins.  For outer
    // joins, the null-extension semantics mean a predicate on one side
    // does not imply the same on the other.
    if join_type != JoinType::Inner {
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

    // Need both a condition (with equi-pairs) and a filter (with
    // single-table predicates) for transitivity to apply.
    let (Some(ref cond_expr), Some(ref filter_expr)) = (&condition, &filter) else {
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

    // Extract equi-pairs from the condition.
    let equi_pairs = extract_equi_pairs(cond_expr, left_width);
    if equi_pairs.is_empty() {
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

    // Collect filter conjuncts by reference; cloning the whole filter expr
    // up-front is wasted work when no transitive predicate is generated.
    let mut filter_conjunct_refs: Vec<&TypedExpr> = Vec::new();
    predicate_pushdown::collect_conjuncts_ref(filter_expr, &mut filter_conjunct_refs);

    // For each conjunct that is a `col op literal` pattern, try to
    // generate a transitive predicate for the partner column.
    let mut new_predicates: Vec<TypedExpr> = Vec::new();
    for conjunct in &filter_conjunct_refs {
        if let Some((col_ord, comparison)) = extract_column_comparison(conjunct) {
            for &(left_ord, right_ord) in &equi_pairs {
                if col_ord == left_ord {
                    // Generate predicate for the right partner.
                    let inferred = substitute_ordinal(&comparison, col_ord, right_ord);
                    if !filter_conjunct_refs.iter().any(|c| *c == &inferred)
                        && !new_predicates.contains(&inferred)
                    {
                        new_predicates.push(inferred);
                    }
                } else if col_ord == right_ord {
                    // Generate predicate for the left partner.
                    let inferred = substitute_ordinal(&comparison, col_ord, left_ord);
                    if !filter_conjunct_refs.iter().any(|c| *c == &inferred)
                        && !new_predicates.contains(&inferred)
                    {
                        new_predicates.push(inferred);
                    }
                }
            }
        }
    }

    if new_predicates.is_empty() {
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

    // AND the new predicates into the existing filter.
    let mut all_conjuncts: Vec<TypedExpr> = filter_conjunct_refs.into_iter().cloned().collect();
    all_conjuncts.extend(new_predicates);
    let enriched_filter = predicate_pushdown::combine_conjuncts(all_conjuncts);

    Ok(LogicalPlan::NestedLoopJoin {
        left,
        right,
        join_type,
        condition,
        outputs,
        filter: enriched_filter,
        order_by,
        limit,
        offset,
        distinct,
        distinct_on,
    })
}

// ------------------------------------------------------------------
// Equi-pair extraction
// ------------------------------------------------------------------

/// Extract `(left_ordinal, right_ordinal)` pairs from an equi-join
/// condition.  Only considers `BinaryEq` between column references
/// on opposite sides.
fn extract_equi_pairs(condition: &TypedExpr, left_width: usize) -> Vec<(usize, usize)> {
    let mut pairs = Vec::new();
    let mut conjuncts = Vec::new();
    predicate_pushdown::collect_conjuncts_ref(condition, &mut conjuncts);

    for conjunct in conjuncts {
        if let TypedExprKind::BinaryEq { left, right } = &conjunct.kind {
            if let (Some(lo), Some(ro)) = (column_ordinal(left), column_ordinal(right)) {
                if lo < left_width && ro >= left_width {
                    pairs.push((lo, ro));
                } else if ro < left_width && lo >= left_width {
                    pairs.push((ro, lo));
                }
            }
        }
    }
    pairs
}

/// Extract a bare column ordinal.  Casts are NOT stripped: a casted
/// join key `CAST(a.x AS int) = b.y` does not establish a direct
/// column equivalence because the cast may be lossy or type-changing.
/// Transitive inference across such pairs would generate predicates
/// like `a.x = 5` which may not be semantically equivalent to the
/// original casted condition.
fn column_ordinal(expr: &TypedExpr) -> Option<usize> {
    match &expr.kind {
        TypedExprKind::ColumnRef { ordinal, .. } => Some(*ordinal),
        _ => None,
    }
}

// ------------------------------------------------------------------
// Column-comparison extraction
// ------------------------------------------------------------------

/// Extract `(column_ordinal, full_comparison_expr)` from predicates of
/// the form `col op literal` or `literal op col` (for commutative ops).
///
/// Returns the ordinal of the column and the original expression (which
/// will be used as a template for substitution).
fn extract_column_comparison(expr: &TypedExpr) -> Option<(usize, TypedExpr)> {
    match &expr.kind {
        TypedExprKind::BinaryEq { left, right }
        | TypedExprKind::BinaryNe { left, right }
        | TypedExprKind::BinaryGt { left, right }
        | TypedExprKind::BinaryGe { left, right }
        | TypedExprKind::BinaryLt { left, right }
        | TypedExprKind::BinaryLe { left, right } => {
            let lo = column_ordinal(left);
            let ro = column_ordinal(right);
            match (lo, ro) {
                (Some(ord), None) if is_literal_like(right) => Some((ord, expr.clone())),
                (None, Some(ord)) if is_literal_like(left) => Some((ord, expr.clone())),
                _ => None,
            }
        }
        TypedExprKind::IsNull { expr: inner, .. } => {
            column_ordinal(inner).map(|ord| (ord, expr.clone()))
        }
        _ => None,
    }
}

/// Check if an expression is a literal or a cast of a literal.
fn is_literal_like(expr: &TypedExpr) -> bool {
    match &expr.kind {
        TypedExprKind::Literal(_) => true,
        TypedExprKind::Cast { expr, .. } => is_literal_like(expr),
        TypedExprKind::Negate { expr } => is_literal_like(expr),
        _ => false,
    }
}

// ------------------------------------------------------------------
// Ordinal substitution
// ------------------------------------------------------------------

/// Replace all `ColumnRef` with ordinal `from_ord` to `to_ord` in the
/// expression.
fn substitute_ordinal(expr: &TypedExpr, from_ord: usize, to_ord: usize) -> TypedExpr {
    predicate_pushdown::map_ordinals(
        expr.clone(),
        |ord| {
            if ord == from_ord {
                to_ord
            } else {
                ord
            }
        },
    )
}
