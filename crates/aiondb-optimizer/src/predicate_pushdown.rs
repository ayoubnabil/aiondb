//! Predicate pushdown for `NestedLoopJoin` nodes.
//!
//! When a `NestedLoopJoin` has a `filter` (WHERE clause), this pass
//! decomposes it into AND-connected conjuncts and pushes single-table
//! predicates down into the appropriate child node.  This enables the
//! downstream access-path selection to use indexes for pushed predicates.
//!
//! A predicate is pushed only when we are **certain** it references only
//! one side of the join (based on `ColumnRef` ordinals vs `left_width`).
//! For hybrid joins, predicates are never pushed into a child subtree that
//! contains `HybridFunctionScan`; only pure relational branches remain eligible.

use std::{cell::Cell, sync::Arc};

use aiondb_catalog::CatalogReader;
use aiondb_core::{DbError, DbResult, RelationId, TxnId};
use aiondb_plan::{JoinType, LogicalPlan, TypedExpr, TypedExprKind};

const MAX_OPTIMIZER_EXPR_REWRITE_DEPTH: usize = 1024;
const MAX_OPTIMIZER_PLAN_WIDTH_DEPTH: usize = 512;

thread_local! {
    static OPTIMIZER_EXPR_REWRITE_DEPTH: Cell<usize> = const { Cell::new(0) };
}

struct OptimizerExprRewriteGuard;

impl OptimizerExprRewriteGuard {
    fn enter() -> Option<Self> {
        OPTIMIZER_EXPR_REWRITE_DEPTH.with(|depth| {
            let current = depth.get();
            if current >= MAX_OPTIMIZER_EXPR_REWRITE_DEPTH {
                None
            } else {
                depth.set(current + 1);
                Some(Self)
            }
        })
    }
}

impl Drop for OptimizerExprRewriteGuard {
    fn drop(&mut self) {
        OPTIMIZER_EXPR_REWRITE_DEPTH.with(|depth| depth.set(depth.get().saturating_sub(1)));
    }
}

// ------------------------------------------------------------------
// Public entry point
// ------------------------------------------------------------------

/// Push single-table filter predicates from a `NestedLoopJoin` down
/// into the child nodes that they reference.
///
/// Returns the (possibly modified) plan.  If no predicates can be
/// pushed, the plan is returned unchanged.
pub(crate) fn push_predicates_into_join(
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

    // Check if there is anything to push (WHERE filter or, for INNER
    // joins, single-table predicates in the ON condition).
    //
    // Semi-join ON clauses must stay at the join node.  Decomposing a
    // decorrelated EXISTS condition can incorrectly push correlated
    // predicates into one child before the semi-match is evaluated.
    //
    // children: pushing a right-side ON predicate into the right child
    // filters rows before the anti-match check, which can incorrectly
    // emit left rows that should have been excluded.  Anti-joins
    // treat the ON clause as LEFT does - only WHERE on the left side
    // can be pushed safely.
    let is_inner = matches!(join_type, JoinType::Inner);

    // If there is no WHERE filter and the join is not INNER (so we
    // cannot touch the ON clause), bail out early.
    if filter.is_none() && !is_inner {
        return Ok(LogicalPlan::NestedLoopJoin {
            left,
            right,
            join_type,
            condition,
            outputs,
            filter: None,
            order_by,
            limit,
            offset,
            distinct,
            distinct_on,
        });
    }

    // If there is no WHERE filter and no ON condition, nothing to push.
    if filter.is_none() && condition.is_none() {
        return Ok(LogicalPlan::NestedLoopJoin {
            left,
            right,
            join_type,
            condition,
            outputs,
            filter: None,
            order_by,
            limit,
            offset,
            distinct,
            distinct_on,
        });
    }

    // Compute the column width of the left child so we can determine
    // which side each predicate references.
    let left_width = match logical_plan_child_width(&left, catalog_reader, txn_id)? {
        Some(w) => w,
        // Cannot determine width; bail out and keep filter/condition as-is.
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

    // Classify predicates: left-only, right-only, or both/unknown.
    let mut left_preds: Vec<TypedExpr> = Vec::new();
    let mut right_preds: Vec<TypedExpr> = Vec::new();
    let mut remaining_filter: Vec<TypedExpr> = Vec::new();

    // Determine which sides are safe to push WHERE predicates into.
    //
    // WHERE predicates pushed into it because the WHERE clause is
    // evaluated AFTER the join produces null-padded rows.  Pushing
    // such predicates into the child would change the result set.
    //
    //   LEFT  JOIN: right side is nullable -> do NOT push right-only
    //   RIGHT JOIN: left  side is nullable -> do NOT push left-only
    //   FULL  JOIN: both sides nullable    -> do NOT push either
    //   INNER/SEMI: neither side nullable  -> push both
    //   ANTI  JOIN: only outputs left rows; right-side WHERE filtering
    //               changes match semantics -> push left only
    let can_push_left = (is_inner
        || matches!(join_type, JoinType::Left | JoinType::Anti | JoinType::Semi))
        && !logical_plan_contains_hybrid_sources(&left);
    let can_push_right = (is_inner || matches!(join_type, JoinType::Right))
        && !logical_plan_contains_hybrid_sources(&right);

    // --- Phase 1: Decompose WHERE filter predicates ---
    if let Some(filter_expr) = filter {
        let filter_conjunct_count = count_conjuncts(&filter_expr);
        left_preds.reserve(filter_conjunct_count);
        right_preds.reserve(filter_conjunct_count);
        remaining_filter.reserve(filter_conjunct_count);
        let mut conjuncts = Vec::with_capacity(filter_conjunct_count);
        collect_conjuncts(filter_expr, &mut conjuncts);

        for conjunct in conjuncts {
            // SECURITY: a predicate that wraps a subquery (scalar/exists/
            // array/IN) or a correlated `OuterColumnRef` can carry hidden
            // column references that `classify_predicate` cannot see,
            // because `for_each_child_expr` deliberately does not descend
            // into subquery bodies (they wrap a `LogicalPlan`, not an
            // expression). Pushing such a conjunct into a single-child
            // scan would change the predicate's column ordinals and
            // strip the correlation it relies on. Keep these at the
            // join level. See SECURITY_FINDINGS_0DAY_A1.md F2.
            if predicate_contains_subquery_or_outer_ref(&conjunct) {
                remaining_filter.push(conjunct);
                continue;
            }
            match classify_predicate(&conjunct, left_width) {
                PredicateSide::LeftOnly if can_push_left => {
                    left_preds.push(conjunct);
                }
                PredicateSide::RightOnly if can_push_right => {
                    // Shift ordinals so they are relative to the right child.
                    right_preds.push(shift_ordinals(conjunct, left_width));
                }
                _ => {
                    remaining_filter.push(conjunct);
                }
            }
        }
    }

    // --- Phase 2: For INNER joins, decompose ON condition predicates ---
    // Pushing ON-clause predicates is only safe for INNER joins. For
    // LEFT/RIGHT/FULL joins the ON clause affects which rows are
    // null-extended, so single-table predicates must stay in the
    // condition.
    let new_condition = if is_inner {
        if let Some(cond_expr) = condition {
            let cond_conjunct_count = count_conjuncts(&cond_expr);
            left_preds.reserve(cond_conjunct_count);
            right_preds.reserve(cond_conjunct_count);
            let mut cond_conjuncts = Vec::with_capacity(cond_conjunct_count);
            collect_conjuncts(cond_expr, &mut cond_conjuncts);

            let mut remaining_cond: Vec<TypedExpr> = Vec::with_capacity(cond_conjunct_count);
            for conjunct in cond_conjuncts {
                // Same correlation guard as the WHERE branch above;
                // see SECURITY_FINDINGS_0DAY_A1.md F2.
                if predicate_contains_subquery_or_outer_ref(&conjunct) {
                    remaining_cond.push(conjunct);
                    continue;
                }
                match classify_predicate(&conjunct, left_width) {
                    PredicateSide::LeftOnly => left_preds.push(conjunct),
                    PredicateSide::RightOnly => {
                        right_preds.push(shift_ordinals(conjunct, left_width));
                    }
                    PredicateSide::Both | PredicateSide::Unknown => {
                        remaining_cond.push(conjunct);
                    }
                }
            }
            combine_conjuncts(remaining_cond)
        } else {
            None
        }
    } else {
        condition
    };

    // If nothing can be pushed, return unchanged.
    if left_preds.is_empty() && right_preds.is_empty() {
        let new_filter = combine_conjuncts(remaining_filter);
        return Ok(LogicalPlan::NestedLoopJoin {
            left,
            right,
            join_type,
            condition: new_condition,
            outputs,
            filter: new_filter,
            order_by,
            limit,
            offset,
            distinct,
            distinct_on,
        });
    }

    // Push predicates into children.  If the child does not accept
    // pushed predicates (e.g. ProjectValues from virtual tables), move
    // them back into the remaining join filter.
    if !left_preds.is_empty() && !can_push_into_child(&left) {
        remaining_filter.append(&mut left_preds);
    }
    if !right_preds.is_empty() && !can_push_into_child(&right) {
        // Right predicates were already shifted to local ordinals; shift
        // them back to combined-row ordinals before returning to the join
        // filter.  shift_ordinals subtracts, so we use a helper to add.
        remaining_filter.reserve(right_preds.len());
        for pred in right_preds.drain(..) {
            remaining_filter.push(add_ordinals(pred, left_width));
        }
    }

    let new_left = match combine_conjuncts(left_preds) {
        Some(pred) => push_into_child(*left, pred),
        None => *left,
    };
    // Recursively push down into nested joins
    let new_left = push_predicates_into_join(new_left, catalog_reader, txn_id)?;

    let new_right = match combine_conjuncts(right_preds) {
        Some(pred) => push_into_child(*right, pred),
        None => *right,
    };
    // Recursively push down into nested joins
    let new_right = push_predicates_into_join(new_right, catalog_reader, txn_id)?;

    let new_filter = combine_conjuncts(remaining_filter);

    Ok(LogicalPlan::NestedLoopJoin {
        left: Box::new(new_left),
        right: Box::new(new_right),
        join_type,
        condition: new_condition,
        outputs,
        filter: new_filter,
        order_by,
        limit,
        offset,
        distinct,
        distinct_on,
    })
}

// ------------------------------------------------------------------
// Predicate classification
// ------------------------------------------------------------------

#[derive(Debug, PartialEq)]
pub(crate) enum PredicateSide {
    /// All column references are on the left side (ordinal < left_width).
    LeftOnly,
    /// All column references are on the right side (ordinal >= left_width).
    RightOnly,
    /// References columns from both sides.
    Both,
    /// No column references found (constant expression) - keep at join level.
    Unknown,
}

/// Determine which side of the join a predicate references.
pub(crate) fn classify_predicate(expr: &TypedExpr, left_width: usize) -> PredicateSide {
    let mut has_left = false;
    let mut has_right = false;
    collect_sides(expr, left_width, &mut has_left, &mut has_right);

    match (has_left, has_right) {
        (true, false) => PredicateSide::LeftOnly,
        (false, true) => PredicateSide::RightOnly,
        (true, true) => PredicateSide::Both,
        (false, false) => PredicateSide::Unknown,
    }
}

/// Walk the expression tree and determine if it references left-side
/// and/or right-side columns.
pub(crate) fn collect_sides(
    expr: &TypedExpr,
    left_width: usize,
    has_left: &mut bool,
    has_right: &mut bool,
) {
    let mut stack = vec![expr];
    while let Some(expr) = stack.pop() {
        if *has_left && *has_right {
            return;
        }
        match &expr.kind {
            TypedExprKind::ColumnRef { ordinal, .. } => {
                if *ordinal < left_width {
                    *has_left = true;
                } else {
                    *has_right = true;
                }
            }
            _ => {
                for_each_child_expr(expr, &mut |child| stack.push(child));
            }
        }
    }
}

// ------------------------------------------------------------------
// Ordinal shifting
// ------------------------------------------------------------------

/// Add `offset` to every `ColumnRef` ordinal in the expression.
/// Used when moving a predicate back from right-child-local ordinals to
/// combined-row ordinals (the inverse of `shift_ordinals`).
fn add_ordinals(expr: TypedExpr, offset: usize) -> TypedExpr {
    map_ordinals(expr, |ord| ord.saturating_add(offset))
}

/// Subtract `offset` from every `ColumnRef` ordinal in the expression.
/// Used when pushing a predicate into the right child, where ordinals
/// are relative to the right child's column space.
fn shift_ordinals(expr: TypedExpr, offset: usize) -> TypedExpr {
    map_ordinals(expr, |ord| ord.saturating_sub(offset))
}

/// Apply `f` to every `ColumnRef` ordinal in `expr`, recursively.
pub(crate) fn map_ordinals(expr: TypedExpr, f: impl Fn(usize) -> usize + Copy) -> TypedExpr {
    remap_expr_ordinals(expr, f, true)
}

fn remap_expr_ordinals(
    expr: TypedExpr,
    f: impl Fn(usize) -> usize + Copy,
    remap_local_columns: bool,
) -> TypedExpr {
    let Some(_guard) = OptimizerExprRewriteGuard::enter() else {
        return expr;
    };
    let TypedExpr {
        kind,
        data_type,
        nullable,
    } = expr;
    let new_kind = match kind {
        TypedExprKind::ColumnRef { name, ordinal } => TypedExprKind::ColumnRef {
            name,
            ordinal: if remap_local_columns {
                f(ordinal)
            } else {
                ordinal
            },
        },
        TypedExprKind::OuterColumnRef { name, ordinal } => TypedExprKind::OuterColumnRef {
            name,
            ordinal: f(ordinal),
        },
        // Binary / two-child variants
        TypedExprKind::BinaryEq { left, right } => TypedExprKind::BinaryEq {
            left: Box::new(remap_expr_ordinals(*left, f, remap_local_columns)),
            right: Box::new(remap_expr_ordinals(*right, f, remap_local_columns)),
        },
        TypedExprKind::BinaryNe { left, right } => TypedExprKind::BinaryNe {
            left: Box::new(remap_expr_ordinals(*left, f, remap_local_columns)),
            right: Box::new(remap_expr_ordinals(*right, f, remap_local_columns)),
        },
        TypedExprKind::BinaryGe { left, right } => TypedExprKind::BinaryGe {
            left: Box::new(remap_expr_ordinals(*left, f, remap_local_columns)),
            right: Box::new(remap_expr_ordinals(*right, f, remap_local_columns)),
        },
        TypedExprKind::BinaryGt { left, right } => TypedExprKind::BinaryGt {
            left: Box::new(remap_expr_ordinals(*left, f, remap_local_columns)),
            right: Box::new(remap_expr_ordinals(*right, f, remap_local_columns)),
        },
        TypedExprKind::BinaryLe { left, right } => TypedExprKind::BinaryLe {
            left: Box::new(remap_expr_ordinals(*left, f, remap_local_columns)),
            right: Box::new(remap_expr_ordinals(*right, f, remap_local_columns)),
        },
        TypedExprKind::BinaryLt { left, right } => TypedExprKind::BinaryLt {
            left: Box::new(remap_expr_ordinals(*left, f, remap_local_columns)),
            right: Box::new(remap_expr_ordinals(*right, f, remap_local_columns)),
        },
        TypedExprKind::LogicalAnd { left, right } => TypedExprKind::LogicalAnd {
            left: Box::new(remap_expr_ordinals(*left, f, remap_local_columns)),
            right: Box::new(remap_expr_ordinals(*right, f, remap_local_columns)),
        },
        TypedExprKind::LogicalOr { left, right } => TypedExprKind::LogicalOr {
            left: Box::new(remap_expr_ordinals(*left, f, remap_local_columns)),
            right: Box::new(remap_expr_ordinals(*right, f, remap_local_columns)),
        },
        TypedExprKind::ArithAdd { left, right } => TypedExprKind::ArithAdd {
            left: Box::new(remap_expr_ordinals(*left, f, remap_local_columns)),
            right: Box::new(remap_expr_ordinals(*right, f, remap_local_columns)),
        },
        TypedExprKind::ArithSub { left, right } => TypedExprKind::ArithSub {
            left: Box::new(remap_expr_ordinals(*left, f, remap_local_columns)),
            right: Box::new(remap_expr_ordinals(*right, f, remap_local_columns)),
        },
        TypedExprKind::ArithMul { left, right } => TypedExprKind::ArithMul {
            left: Box::new(remap_expr_ordinals(*left, f, remap_local_columns)),
            right: Box::new(remap_expr_ordinals(*right, f, remap_local_columns)),
        },
        TypedExprKind::ArithDiv { left, right } => TypedExprKind::ArithDiv {
            left: Box::new(remap_expr_ordinals(*left, f, remap_local_columns)),
            right: Box::new(remap_expr_ordinals(*right, f, remap_local_columns)),
        },
        TypedExprKind::ArithMod { left, right } => TypedExprKind::ArithMod {
            left: Box::new(remap_expr_ordinals(*left, f, remap_local_columns)),
            right: Box::new(remap_expr_ordinals(*right, f, remap_local_columns)),
        },
        TypedExprKind::Concat { left, right } => TypedExprKind::Concat {
            left: Box::new(remap_expr_ordinals(*left, f, remap_local_columns)),
            right: Box::new(remap_expr_ordinals(*right, f, remap_local_columns)),
        },
        TypedExprKind::ArrayConcat { left, right } => TypedExprKind::ArrayConcat {
            left: Box::new(remap_expr_ordinals(*left, f, remap_local_columns)),
            right: Box::new(remap_expr_ordinals(*right, f, remap_local_columns)),
        },
        TypedExprKind::Nullif { left, right } => TypedExprKind::Nullif {
            left: Box::new(remap_expr_ordinals(*left, f, remap_local_columns)),
            right: Box::new(remap_expr_ordinals(*right, f, remap_local_columns)),
        },
        TypedExprKind::IsDistinctFrom {
            left,
            right,
            negated,
        } => TypedExprKind::IsDistinctFrom {
            left: Box::new(remap_expr_ordinals(*left, f, remap_local_columns)),
            right: Box::new(remap_expr_ordinals(*right, f, remap_local_columns)),
            negated,
        },
        TypedExprKind::ArrayContains { left, right } => TypedExprKind::ArrayContains {
            left: Box::new(remap_expr_ordinals(*left, f, remap_local_columns)),
            right: Box::new(remap_expr_ordinals(*right, f, remap_local_columns)),
        },
        TypedExprKind::ArrayContainedBy { left, right } => TypedExprKind::ArrayContainedBy {
            left: Box::new(remap_expr_ordinals(*left, f, remap_local_columns)),
            right: Box::new(remap_expr_ordinals(*right, f, remap_local_columns)),
        },
        TypedExprKind::ArrayOverlap { left, right } => TypedExprKind::ArrayOverlap {
            left: Box::new(remap_expr_ordinals(*left, f, remap_local_columns)),
            right: Box::new(remap_expr_ordinals(*right, f, remap_local_columns)),
        },
        TypedExprKind::JsonGet { left, right } => TypedExprKind::JsonGet {
            left: Box::new(remap_expr_ordinals(*left, f, remap_local_columns)),
            right: Box::new(remap_expr_ordinals(*right, f, remap_local_columns)),
        },
        TypedExprKind::JsonGetText { left, right } => TypedExprKind::JsonGetText {
            left: Box::new(remap_expr_ordinals(*left, f, remap_local_columns)),
            right: Box::new(remap_expr_ordinals(*right, f, remap_local_columns)),
        },
        TypedExprKind::JsonPathGet { left, right } => TypedExprKind::JsonPathGet {
            left: Box::new(remap_expr_ordinals(*left, f, remap_local_columns)),
            right: Box::new(remap_expr_ordinals(*right, f, remap_local_columns)),
        },
        TypedExprKind::JsonPathGetText { left, right } => TypedExprKind::JsonPathGetText {
            left: Box::new(remap_expr_ordinals(*left, f, remap_local_columns)),
            right: Box::new(remap_expr_ordinals(*right, f, remap_local_columns)),
        },
        TypedExprKind::JsonContains { left, right } => TypedExprKind::JsonContains {
            left: Box::new(remap_expr_ordinals(*left, f, remap_local_columns)),
            right: Box::new(remap_expr_ordinals(*right, f, remap_local_columns)),
        },
        TypedExprKind::JsonContainedBy { left, right } => TypedExprKind::JsonContainedBy {
            left: Box::new(remap_expr_ordinals(*left, f, remap_local_columns)),
            right: Box::new(remap_expr_ordinals(*right, f, remap_local_columns)),
        },
        TypedExprKind::JsonKeyExists { left, right } => TypedExprKind::JsonKeyExists {
            left: Box::new(remap_expr_ordinals(*left, f, remap_local_columns)),
            right: Box::new(remap_expr_ordinals(*right, f, remap_local_columns)),
        },
        TypedExprKind::JsonAnyKeyExists { left, right } => TypedExprKind::JsonAnyKeyExists {
            left: Box::new(remap_expr_ordinals(*left, f, remap_local_columns)),
            right: Box::new(remap_expr_ordinals(*right, f, remap_local_columns)),
        },
        TypedExprKind::JsonAllKeysExist { left, right } => TypedExprKind::JsonAllKeysExist {
            left: Box::new(remap_expr_ordinals(*left, f, remap_local_columns)),
            right: Box::new(remap_expr_ordinals(*right, f, remap_local_columns)),
        },
        // Unary / single-child variants
        TypedExprKind::LogicalNot { expr: inner } => TypedExprKind::LogicalNot {
            expr: Box::new(remap_expr_ordinals(*inner, f, remap_local_columns)),
        },
        TypedExprKind::Negate { expr: inner } => TypedExprKind::Negate {
            expr: Box::new(remap_expr_ordinals(*inner, f, remap_local_columns)),
        },
        TypedExprKind::IsNull {
            expr: inner,
            negated,
        } => TypedExprKind::IsNull {
            expr: Box::new(remap_expr_ordinals(*inner, f, remap_local_columns)),
            negated,
        },
        TypedExprKind::Cast {
            expr: inner,
            target_type,
        } => TypedExprKind::Cast {
            expr: Box::new(remap_expr_ordinals(*inner, f, remap_local_columns)),
            target_type,
        },
        // Multi-child variants
        TypedExprKind::Like {
            expr: inner,
            pattern,
            negated,
            case_insensitive,
        } => TypedExprKind::Like {
            expr: Box::new(remap_expr_ordinals(*inner, f, remap_local_columns)),
            pattern: Box::new(remap_expr_ordinals(*pattern, f, remap_local_columns)),
            negated,
            case_insensitive,
        },
        TypedExprKind::InList {
            expr: inner,
            list,
            negated,
        } => TypedExprKind::InList {
            expr: Box::new(remap_expr_ordinals(*inner, f, remap_local_columns)),
            list: list
                .into_iter()
                .map(|e| remap_expr_ordinals(e, f, remap_local_columns))
                .collect(),
            negated,
        },
        TypedExprKind::Between {
            expr: inner,
            low,
            high,
            negated,
        } => TypedExprKind::Between {
            expr: Box::new(remap_expr_ordinals(*inner, f, remap_local_columns)),
            low: Box::new(remap_expr_ordinals(*low, f, remap_local_columns)),
            high: Box::new(remap_expr_ordinals(*high, f, remap_local_columns)),
            negated,
        },
        TypedExprKind::CaseWhen {
            conditions,
            results,
            else_result,
        } => TypedExprKind::CaseWhen {
            conditions: conditions
                .into_iter()
                .map(|e| remap_expr_ordinals(e, f, remap_local_columns))
                .collect(),
            results: results
                .into_iter()
                .map(|e| remap_expr_ordinals(e, f, remap_local_columns))
                .collect(),
            else_result: else_result
                .map(|e| Box::new(remap_expr_ordinals(*e, f, remap_local_columns))),
        },
        TypedExprKind::Coalesce { args } => TypedExprKind::Coalesce {
            args: args
                .into_iter()
                .map(|e| remap_expr_ordinals(e, f, remap_local_columns))
                .collect(),
        },
        TypedExprKind::ScalarFunction { func, args } => TypedExprKind::ScalarFunction {
            func,
            args: args
                .into_iter()
                .map(|e| remap_expr_ordinals(e, f, remap_local_columns))
                .collect(),
        },
        TypedExprKind::UserFunction {
            name,
            args,
            body,
            params,
            language,
        } => TypedExprKind::UserFunction {
            name,
            args: args
                .into_iter()
                .map(|e| remap_expr_ordinals(e, f, remap_local_columns))
                .collect(),
            body,
            params,
            language,
        },
        TypedExprKind::WindowFunction {
            func,
            args,
            partition_by,
            order_by,
        } => TypedExprKind::WindowFunction {
            func,
            args: args
                .into_iter()
                .map(|e| remap_expr_ordinals(e, f, remap_local_columns))
                .collect(),
            partition_by: partition_by
                .into_iter()
                .map(|e| remap_expr_ordinals(e, f, remap_local_columns))
                .collect(),
            order_by: order_by
                .into_iter()
                .map(|sort| aiondb_plan::SortExpr {
                    expr: remap_expr_ordinals(sort.expr, f, remap_local_columns),
                    descending: sort.descending,
                    nulls_first: sort.nulls_first,
                })
                .collect(),
        },
        TypedExprKind::ArrayConstruct { elements } => TypedExprKind::ArrayConstruct {
            elements: elements
                .into_iter()
                .map(|e| remap_expr_ordinals(e, f, remap_local_columns))
                .collect(),
        },
        TypedExprKind::ScalarSubquery { plan } => TypedExprKind::ScalarSubquery {
            plan: Box::new(remap_outer_ordinals_in_plan(*plan, f)),
        },
        TypedExprKind::ArraySubquery { plan } => TypedExprKind::ArraySubquery {
            plan: Box::new(remap_outer_ordinals_in_plan(*plan, f)),
        },
        TypedExprKind::InSubquery {
            expr: inner,
            plan,
            negated,
        } => TypedExprKind::InSubquery {
            expr: Box::new(remap_expr_ordinals(*inner, f, remap_local_columns)),
            plan: Box::new(remap_outer_ordinals_in_plan(*plan, f)),
            negated,
        },
        TypedExprKind::ExistsSubquery { plan, negated } => TypedExprKind::ExistsSubquery {
            plan: Box::new(remap_outer_ordinals_in_plan(*plan, f)),
            negated,
        },
        // Leaf nodes (literals, parameters, etc.): return as-is.
        other => {
            return TypedExpr {
                kind: other,
                data_type,
                nullable,
            };
        }
    };
    TypedExpr {
        kind: new_kind,
        data_type,
        nullable,
    }
}

fn remap_outer_ordinals_in_projection(
    projection: aiondb_plan::ProjectionExpr,
    f: impl Fn(usize) -> usize + Copy,
) -> aiondb_plan::ProjectionExpr {
    aiondb_plan::ProjectionExpr {
        field: projection.field,
        expr: remap_expr_ordinals(projection.expr, f, false),
    }
}

fn remap_outer_ordinals_in_sort(
    sort: aiondb_plan::SortExpr,
    f: impl Fn(usize) -> usize + Copy,
) -> aiondb_plan::SortExpr {
    aiondb_plan::SortExpr {
        expr: remap_expr_ordinals(sort.expr, f, false),
        descending: sort.descending,
        nulls_first: sort.nulls_first,
    }
}

fn remap_outer_ordinals_in_plan(
    plan: LogicalPlan,
    f: impl Fn(usize) -> usize + Copy,
) -> LogicalPlan {
    match plan {
        LogicalPlan::ProjectOnce {
            outputs,
            filter,
            order_by,
            limit,
            offset,
            distinct,
            distinct_on,
        } => LogicalPlan::ProjectOnce {
            outputs: outputs
                .into_iter()
                .map(|projection| remap_outer_ordinals_in_projection(projection, f))
                .collect(),
            filter: filter.map(|expr| remap_expr_ordinals(expr, f, false)),
            order_by: order_by
                .into_iter()
                .map(|sort| remap_outer_ordinals_in_sort(sort, f))
                .collect(),
            limit: limit.map(|expr| remap_expr_ordinals(expr, f, false)),
            offset: offset.map(|expr| remap_expr_ordinals(expr, f, false)),
            distinct,
            distinct_on: distinct_on
                .into_iter()
                .map(|expr| remap_expr_ordinals(expr, f, false))
                .collect(),
        },
        LogicalPlan::ProjectTable {
            table_id,
            outputs,
            filter,
            order_by,
            limit,
            offset,
            distinct,
            distinct_on,
        } => LogicalPlan::ProjectTable {
            table_id,
            outputs: outputs
                .into_iter()
                .map(|projection| remap_outer_ordinals_in_projection(projection, f))
                .collect(),
            filter: filter.map(|expr| remap_expr_ordinals(expr, f, false)),
            order_by: order_by
                .into_iter()
                .map(|sort| remap_outer_ordinals_in_sort(sort, f))
                .collect(),
            limit: limit.map(|expr| remap_expr_ordinals(expr, f, false)),
            offset: offset.map(|expr| remap_expr_ordinals(expr, f, false)),
            distinct,
            distinct_on: distinct_on
                .into_iter()
                .map(|expr| remap_expr_ordinals(expr, f, false))
                .collect(),
        },
        LogicalPlan::LockingProjectTable {
            table_id,
            outputs,
            filter,
            order_by,
            limit,
            offset,
            distinct,
            distinct_on,
            row_lock,
        } => LogicalPlan::LockingProjectTable {
            table_id,
            outputs: outputs
                .into_iter()
                .map(|projection| remap_outer_ordinals_in_projection(projection, f))
                .collect(),
            filter: filter.map(|expr| remap_expr_ordinals(expr, f, false)),
            order_by: order_by
                .into_iter()
                .map(|sort| remap_outer_ordinals_in_sort(sort, f))
                .collect(),
            limit: limit.map(|expr| remap_expr_ordinals(expr, f, false)),
            offset: offset.map(|expr| remap_expr_ordinals(expr, f, false)),
            distinct,
            distinct_on: distinct_on
                .into_iter()
                .map(|expr| remap_expr_ordinals(expr, f, false))
                .collect(),
            row_lock,
        },
        LogicalPlan::ProjectSource {
            source,
            outputs,
            filter,
            order_by,
            limit,
            offset,
            distinct,
            distinct_on,
        } => LogicalPlan::ProjectSource {
            source: Box::new(remap_outer_ordinals_in_plan(*source, f)),
            outputs: outputs
                .into_iter()
                .map(|projection| remap_outer_ordinals_in_projection(projection, f))
                .collect(),
            filter: filter.map(|expr| remap_expr_ordinals(expr, f, false)),
            order_by: order_by
                .into_iter()
                .map(|sort| remap_outer_ordinals_in_sort(sort, f))
                .collect(),
            limit: limit.map(|expr| remap_expr_ordinals(expr, f, false)),
            offset: offset.map(|expr| remap_expr_ordinals(expr, f, false)),
            distinct,
            distinct_on: distinct_on
                .into_iter()
                .map(|expr| remap_expr_ordinals(expr, f, false))
                .collect(),
        },
        LogicalPlan::NestedLoopJoin {
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
        } => LogicalPlan::NestedLoopJoin {
            left: Box::new(remap_outer_ordinals_in_plan(*left, f)),
            right: Box::new(remap_outer_ordinals_in_plan(*right, f)),
            join_type,
            condition: condition.map(|expr| remap_expr_ordinals(expr, f, false)),
            outputs: outputs
                .into_iter()
                .map(|projection| remap_outer_ordinals_in_projection(projection, f))
                .collect(),
            filter: filter.map(|expr| remap_expr_ordinals(expr, f, false)),
            order_by: order_by
                .into_iter()
                .map(|sort| remap_outer_ordinals_in_sort(sort, f))
                .collect(),
            limit: limit.map(|expr| remap_expr_ordinals(expr, f, false)),
            offset: offset.map(|expr| remap_expr_ordinals(expr, f, false)),
            distinct,
            distinct_on: distinct_on
                .into_iter()
                .map(|expr| remap_expr_ordinals(expr, f, false))
                .collect(),
        },
        LogicalPlan::HybridFunctionScan {
            function_name,
            args,
            output_fields,
        } => LogicalPlan::HybridFunctionScan {
            function_name,
            args: args
                .into_iter()
                .map(|expr| remap_expr_ordinals(expr, f, false))
                .collect(),
            output_fields,
        },
        LogicalPlan::Aggregate {
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
        } => LogicalPlan::Aggregate {
            table_id,
            group_by: group_by
                .into_iter()
                .map(|expr| remap_expr_ordinals(expr, f, false))
                .collect(),
            grouping_sets,
            aggregates: aggregates
                .into_iter()
                .map(|projection| remap_outer_ordinals_in_projection(projection, f))
                .collect(),
            having: having.map(|expr| remap_expr_ordinals(expr, f, false)),
            filter: filter.map(|expr| remap_expr_ordinals(expr, f, false)),
            order_by: order_by
                .into_iter()
                .map(|sort| remap_outer_ordinals_in_sort(sort, f))
                .collect(),
            limit: limit.map(|expr| remap_expr_ordinals(expr, f, false)),
            offset: offset.map(|expr| remap_expr_ordinals(expr, f, false)),
            distinct,
            distinct_on: distinct_on
                .into_iter()
                .map(|expr| remap_expr_ordinals(expr, f, false))
                .collect(),
        },
        LogicalPlan::AggregateSource {
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
        } => LogicalPlan::AggregateSource {
            source: Box::new(remap_outer_ordinals_in_plan(*source, f)),
            group_by: group_by
                .into_iter()
                .map(|expr| remap_expr_ordinals(expr, f, false))
                .collect(),
            grouping_sets,
            aggregates: aggregates
                .into_iter()
                .map(|projection| remap_outer_ordinals_in_projection(projection, f))
                .collect(),
            having: having.map(|expr| remap_expr_ordinals(expr, f, false)),
            filter: filter.map(|expr| remap_expr_ordinals(expr, f, false)),
            order_by: order_by
                .into_iter()
                .map(|sort| remap_outer_ordinals_in_sort(sort, f))
                .collect(),
            limit: limit.map(|expr| remap_expr_ordinals(expr, f, false)),
            offset: offset.map(|expr| remap_expr_ordinals(expr, f, false)),
            distinct,
            distinct_on: distinct_on
                .into_iter()
                .map(|expr| remap_expr_ordinals(expr, f, false))
                .collect(),
        },
        LogicalPlan::SetOperation {
            op,
            all,
            left,
            right,
            output_fields,
            order_by,
            limit,
            offset,
        } => LogicalPlan::SetOperation {
            op,
            all,
            left: Box::new(remap_outer_ordinals_in_plan(*left, f)),
            right: Box::new(remap_outer_ordinals_in_plan(*right, f)),
            output_fields,
            order_by: order_by
                .into_iter()
                .map(|sort| remap_outer_ordinals_in_sort(sort, f))
                .collect(),
            limit: limit.map(|expr| remap_expr_ordinals(expr, f, false)),
            offset: offset.map(|expr| remap_expr_ordinals(expr, f, false)),
        },
        LogicalPlan::ProjectValues {
            output_fields,
            rows,
            order_by,
            limit,
            offset,
        } => LogicalPlan::ProjectValues {
            output_fields,
            rows: rows
                .into_iter()
                .map(|row| {
                    row.into_iter()
                        .map(|expr| remap_expr_ordinals(expr, f, false))
                        .collect()
                })
                .collect(),
            order_by: order_by
                .into_iter()
                .map(|sort| remap_outer_ordinals_in_sort(sort, f))
                .collect(),
            limit: limit.map(|expr| remap_expr_ordinals(expr, f, false)),
            offset: offset.map(|expr| remap_expr_ordinals(expr, f, false)),
        },
        LogicalPlan::RecursiveCte {
            base,
            recursive,
            union_all,
            output_fields,
        } => LogicalPlan::RecursiveCte {
            base: Box::new(remap_outer_ordinals_in_plan(*base, f)),
            recursive: Box::new(remap_outer_ordinals_in_plan(*recursive, f)),
            union_all,
            output_fields,
        },
        other => other,
    }
}

fn rewrite_project_source_refs(
    expr: TypedExpr,
    outputs: &[aiondb_plan::ProjectionExpr],
) -> TypedExpr {
    let Some(_guard) = OptimizerExprRewriteGuard::enter() else {
        return expr;
    };
    let TypedExpr {
        kind,
        data_type,
        nullable,
    } = expr;
    let new_kind = match kind {
        TypedExprKind::ColumnRef { ordinal, .. } => {
            if let Some(output) = outputs.get(ordinal) {
                return output.expr.clone();
            }
            return TypedExpr {
                kind: TypedExprKind::ColumnRef {
                    name: String::new(),
                    ordinal,
                },
                data_type,
                nullable,
            };
        }
        TypedExprKind::BinaryEq { left, right } => TypedExprKind::BinaryEq {
            left: Box::new(rewrite_project_source_refs(*left, outputs)),
            right: Box::new(rewrite_project_source_refs(*right, outputs)),
        },
        TypedExprKind::BinaryNe { left, right } => TypedExprKind::BinaryNe {
            left: Box::new(rewrite_project_source_refs(*left, outputs)),
            right: Box::new(rewrite_project_source_refs(*right, outputs)),
        },
        TypedExprKind::BinaryGe { left, right } => TypedExprKind::BinaryGe {
            left: Box::new(rewrite_project_source_refs(*left, outputs)),
            right: Box::new(rewrite_project_source_refs(*right, outputs)),
        },
        TypedExprKind::BinaryGt { left, right } => TypedExprKind::BinaryGt {
            left: Box::new(rewrite_project_source_refs(*left, outputs)),
            right: Box::new(rewrite_project_source_refs(*right, outputs)),
        },
        TypedExprKind::BinaryLe { left, right } => TypedExprKind::BinaryLe {
            left: Box::new(rewrite_project_source_refs(*left, outputs)),
            right: Box::new(rewrite_project_source_refs(*right, outputs)),
        },
        TypedExprKind::BinaryLt { left, right } => TypedExprKind::BinaryLt {
            left: Box::new(rewrite_project_source_refs(*left, outputs)),
            right: Box::new(rewrite_project_source_refs(*right, outputs)),
        },
        TypedExprKind::LogicalAnd { left, right } => TypedExprKind::LogicalAnd {
            left: Box::new(rewrite_project_source_refs(*left, outputs)),
            right: Box::new(rewrite_project_source_refs(*right, outputs)),
        },
        TypedExprKind::LogicalOr { left, right } => TypedExprKind::LogicalOr {
            left: Box::new(rewrite_project_source_refs(*left, outputs)),
            right: Box::new(rewrite_project_source_refs(*right, outputs)),
        },
        TypedExprKind::ArithAdd { left, right } => TypedExprKind::ArithAdd {
            left: Box::new(rewrite_project_source_refs(*left, outputs)),
            right: Box::new(rewrite_project_source_refs(*right, outputs)),
        },
        TypedExprKind::ArithSub { left, right } => TypedExprKind::ArithSub {
            left: Box::new(rewrite_project_source_refs(*left, outputs)),
            right: Box::new(rewrite_project_source_refs(*right, outputs)),
        },
        TypedExprKind::ArithMul { left, right } => TypedExprKind::ArithMul {
            left: Box::new(rewrite_project_source_refs(*left, outputs)),
            right: Box::new(rewrite_project_source_refs(*right, outputs)),
        },
        TypedExprKind::ArithDiv { left, right } => TypedExprKind::ArithDiv {
            left: Box::new(rewrite_project_source_refs(*left, outputs)),
            right: Box::new(rewrite_project_source_refs(*right, outputs)),
        },
        TypedExprKind::ArithMod { left, right } => TypedExprKind::ArithMod {
            left: Box::new(rewrite_project_source_refs(*left, outputs)),
            right: Box::new(rewrite_project_source_refs(*right, outputs)),
        },
        TypedExprKind::Concat { left, right } => TypedExprKind::Concat {
            left: Box::new(rewrite_project_source_refs(*left, outputs)),
            right: Box::new(rewrite_project_source_refs(*right, outputs)),
        },
        TypedExprKind::ArrayConcat { left, right } => TypedExprKind::ArrayConcat {
            left: Box::new(rewrite_project_source_refs(*left, outputs)),
            right: Box::new(rewrite_project_source_refs(*right, outputs)),
        },
        TypedExprKind::Nullif { left, right } => TypedExprKind::Nullif {
            left: Box::new(rewrite_project_source_refs(*left, outputs)),
            right: Box::new(rewrite_project_source_refs(*right, outputs)),
        },
        TypedExprKind::IsDistinctFrom {
            left,
            right,
            negated,
        } => TypedExprKind::IsDistinctFrom {
            left: Box::new(rewrite_project_source_refs(*left, outputs)),
            right: Box::new(rewrite_project_source_refs(*right, outputs)),
            negated,
        },
        TypedExprKind::ArrayContains { left, right } => TypedExprKind::ArrayContains {
            left: Box::new(rewrite_project_source_refs(*left, outputs)),
            right: Box::new(rewrite_project_source_refs(*right, outputs)),
        },
        TypedExprKind::ArrayContainedBy { left, right } => TypedExprKind::ArrayContainedBy {
            left: Box::new(rewrite_project_source_refs(*left, outputs)),
            right: Box::new(rewrite_project_source_refs(*right, outputs)),
        },
        TypedExprKind::ArrayOverlap { left, right } => TypedExprKind::ArrayOverlap {
            left: Box::new(rewrite_project_source_refs(*left, outputs)),
            right: Box::new(rewrite_project_source_refs(*right, outputs)),
        },
        TypedExprKind::JsonGet { left, right } => TypedExprKind::JsonGet {
            left: Box::new(rewrite_project_source_refs(*left, outputs)),
            right: Box::new(rewrite_project_source_refs(*right, outputs)),
        },
        TypedExprKind::JsonGetText { left, right } => TypedExprKind::JsonGetText {
            left: Box::new(rewrite_project_source_refs(*left, outputs)),
            right: Box::new(rewrite_project_source_refs(*right, outputs)),
        },
        TypedExprKind::JsonPathGet { left, right } => TypedExprKind::JsonPathGet {
            left: Box::new(rewrite_project_source_refs(*left, outputs)),
            right: Box::new(rewrite_project_source_refs(*right, outputs)),
        },
        TypedExprKind::JsonPathGetText { left, right } => TypedExprKind::JsonPathGetText {
            left: Box::new(rewrite_project_source_refs(*left, outputs)),
            right: Box::new(rewrite_project_source_refs(*right, outputs)),
        },
        TypedExprKind::JsonContains { left, right } => TypedExprKind::JsonContains {
            left: Box::new(rewrite_project_source_refs(*left, outputs)),
            right: Box::new(rewrite_project_source_refs(*right, outputs)),
        },
        TypedExprKind::JsonContainedBy { left, right } => TypedExprKind::JsonContainedBy {
            left: Box::new(rewrite_project_source_refs(*left, outputs)),
            right: Box::new(rewrite_project_source_refs(*right, outputs)),
        },
        TypedExprKind::JsonKeyExists { left, right } => TypedExprKind::JsonKeyExists {
            left: Box::new(rewrite_project_source_refs(*left, outputs)),
            right: Box::new(rewrite_project_source_refs(*right, outputs)),
        },
        TypedExprKind::JsonAnyKeyExists { left, right } => TypedExprKind::JsonAnyKeyExists {
            left: Box::new(rewrite_project_source_refs(*left, outputs)),
            right: Box::new(rewrite_project_source_refs(*right, outputs)),
        },
        TypedExprKind::JsonAllKeysExist { left, right } => TypedExprKind::JsonAllKeysExist {
            left: Box::new(rewrite_project_source_refs(*left, outputs)),
            right: Box::new(rewrite_project_source_refs(*right, outputs)),
        },
        TypedExprKind::LogicalNot { expr: inner } => TypedExprKind::LogicalNot {
            expr: Box::new(rewrite_project_source_refs(*inner, outputs)),
        },
        TypedExprKind::Negate { expr: inner } => TypedExprKind::Negate {
            expr: Box::new(rewrite_project_source_refs(*inner, outputs)),
        },
        TypedExprKind::IsNull {
            expr: inner,
            negated,
        } => TypedExprKind::IsNull {
            expr: Box::new(rewrite_project_source_refs(*inner, outputs)),
            negated,
        },
        TypedExprKind::Cast {
            expr: inner,
            target_type,
        } => TypedExprKind::Cast {
            expr: Box::new(rewrite_project_source_refs(*inner, outputs)),
            target_type,
        },
        TypedExprKind::Like {
            expr: inner,
            pattern,
            negated,
            case_insensitive,
        } => TypedExprKind::Like {
            expr: Box::new(rewrite_project_source_refs(*inner, outputs)),
            pattern: Box::new(rewrite_project_source_refs(*pattern, outputs)),
            negated,
            case_insensitive,
        },
        TypedExprKind::InList {
            expr: inner,
            list,
            negated,
        } => TypedExprKind::InList {
            expr: Box::new(rewrite_project_source_refs(*inner, outputs)),
            list: list
                .into_iter()
                .map(|value| rewrite_project_source_refs(value, outputs))
                .collect(),
            negated,
        },
        TypedExprKind::Between {
            expr: inner,
            low,
            high,
            negated,
        } => TypedExprKind::Between {
            expr: Box::new(rewrite_project_source_refs(*inner, outputs)),
            low: Box::new(rewrite_project_source_refs(*low, outputs)),
            high: Box::new(rewrite_project_source_refs(*high, outputs)),
            negated,
        },
        TypedExprKind::CaseWhen {
            conditions,
            results,
            else_result,
        } => TypedExprKind::CaseWhen {
            conditions: conditions
                .into_iter()
                .map(|value| rewrite_project_source_refs(value, outputs))
                .collect(),
            results: results
                .into_iter()
                .map(|value| rewrite_project_source_refs(value, outputs))
                .collect(),
            else_result: else_result
                .map(|value| Box::new(rewrite_project_source_refs(*value, outputs))),
        },
        TypedExprKind::Coalesce { args } => TypedExprKind::Coalesce {
            args: args
                .into_iter()
                .map(|value| rewrite_project_source_refs(value, outputs))
                .collect(),
        },
        TypedExprKind::ScalarFunction { func, args } => TypedExprKind::ScalarFunction {
            func,
            args: args
                .into_iter()
                .map(|value| rewrite_project_source_refs(value, outputs))
                .collect(),
        },
        TypedExprKind::UserFunction {
            name,
            args,
            body,
            params,
            language,
        } => TypedExprKind::UserFunction {
            name,
            args: args
                .into_iter()
                .map(|value| rewrite_project_source_refs(value, outputs))
                .collect(),
            body,
            params,
            language,
        },
        TypedExprKind::WindowFunction {
            func,
            args,
            partition_by,
            order_by,
        } => TypedExprKind::WindowFunction {
            func,
            args: args
                .into_iter()
                .map(|value| rewrite_project_source_refs(value, outputs))
                .collect(),
            partition_by: partition_by
                .into_iter()
                .map(|value| rewrite_project_source_refs(value, outputs))
                .collect(),
            order_by: order_by
                .into_iter()
                .map(|sort| aiondb_plan::SortExpr {
                    expr: rewrite_project_source_refs(sort.expr, outputs),
                    descending: sort.descending,
                    nulls_first: sort.nulls_first,
                })
                .collect(),
        },
        TypedExprKind::ArrayConstruct { elements } => TypedExprKind::ArrayConstruct {
            elements: elements
                .into_iter()
                .map(|value| rewrite_project_source_refs(value, outputs))
                .collect(),
        },
        other => {
            return TypedExpr {
                kind: other,
                data_type,
                nullable,
            };
        }
    };
    TypedExpr {
        kind: new_kind,
        data_type,
        nullable,
    }
}

// ------------------------------------------------------------------
// Child manipulation
// ------------------------------------------------------------------

/// Push a predicate into a child plan node.
///
/// - `ProjectTable`: AND the predicate with the existing filter.
/// - `SeqScan`: convert to `ProjectTable` with the predicate as filter.
/// - `NestedLoopJoin`: AND the predicate with the join's filter.
/// - Other nodes: wrap in a `ProjectSource` (not implemented - bail out
///   should have happened earlier).
///
/// Returns true when the child plan can accept a pushed-down predicate.
fn can_push_into_child(child: &LogicalPlan) -> bool {
    match child {
        LogicalPlan::ProjectTable { .. } | LogicalPlan::SeqScan { .. } => true,
        LogicalPlan::NestedLoopJoin { outputs, .. } => outputs.is_empty(),
        _ => project_source_allows_predicate_passthrough(child),
    }
}

pub(crate) fn push_into_child(child: LogicalPlan, predicate: TypedExpr) -> LogicalPlan {
    match child {
        LogicalPlan::ProjectTable {
            table_id,
            outputs,
            filter,
            order_by,
            limit,
            offset,
            distinct,
            distinct_on,
        } => {
            let new_filter = match filter {
                Some(existing) => Some(TypedExpr::logical_and(existing, predicate)),
                None => Some(predicate),
            };
            LogicalPlan::ProjectTable {
                table_id,
                outputs,
                filter: new_filter,
                order_by,
                limit,
                offset,
                distinct,
                distinct_on,
            }
        }
        LogicalPlan::SeqScan { table_id } => LogicalPlan::ProjectTable {
            table_id,
            outputs: Vec::new(),
            filter: Some(predicate),
            order_by: Vec::new(),
            limit: None,
            offset: None,
            distinct: false,
            distinct_on: Vec::new(),
        },
        LogicalPlan::NestedLoopJoin {
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
        } => {
            let new_filter = match filter {
                Some(existing) => Some(TypedExpr::logical_and(existing, predicate)),
                None => Some(predicate),
            };
            LogicalPlan::NestedLoopJoin {
                left,
                right,
                join_type,
                condition,
                outputs,
                filter: new_filter,
                order_by,
                limit,
                offset,
                distinct,
                distinct_on,
            }
        }
        LogicalPlan::ProjectSource {
            source,
            outputs,
            filter,
            order_by,
            limit,
            offset,
            distinct,
            distinct_on,
        } => {
            if project_source_passthrough_ordinals(&outputs)
                .filter(|_| limit.is_none())
                .filter(|_| offset.is_none())
                .filter(|_| !distinct)
                .filter(|_| distinct_on.is_empty())
                .filter(|_| !logical_plan_contains_hybrid_sources(&source))
                .filter(|_| can_push_into_child(&source))
                .is_some()
            {
                let remapped_predicate = rewrite_project_source_refs(predicate, &outputs);
                LogicalPlan::ProjectSource {
                    source: Box::new(push_into_child(*source, remapped_predicate)),
                    outputs,
                    filter,
                    order_by,
                    limit,
                    offset,
                    distinct,
                    distinct_on,
                }
            } else {
                LogicalPlan::ProjectSource {
                    source,
                    outputs,
                    filter: Some(match filter {
                        Some(existing) => TypedExpr::logical_and(existing, predicate),
                        None => predicate,
                    }),
                    order_by,
                    limit,
                    offset,
                    distinct,
                    distinct_on,
                }
            }
        }
        // Unreachable when callers check can_push_into_child first.
        other => other,
    }
}

fn project_source_allows_predicate_passthrough(child: &LogicalPlan) -> bool {
    match child {
        LogicalPlan::ProjectSource {
            source,
            outputs,
            order_by: _,
            limit,
            offset,
            distinct,
            distinct_on,
            ..
        } => {
            project_source_passthrough_ordinals(outputs).is_some()
                && limit.is_none()
                && offset.is_none()
                && !*distinct
                && distinct_on.is_empty()
                && !logical_plan_contains_hybrid_sources(source)
                && can_push_into_child(source)
        }
        _ => false,
    }
}

fn project_source_passthrough_ordinals(
    outputs: &[aiondb_plan::ProjectionExpr],
) -> Option<Vec<usize>> {
    if outputs.is_empty() {
        return None;
    }
    outputs
        .iter()
        .map(|projection| {
            projection
                .expr
                .kind
                .as_column_ref()
                .map(|(_, ordinal)| ordinal)
        })
        .collect()
}

// ------------------------------------------------------------------
// Child width computation
// ------------------------------------------------------------------

/// Compute the number of output columns for a logical plan child.
///
/// Returns `None` when the width cannot be determined (which means
/// predicate pushdown should be skipped for safety).
pub(crate) fn logical_plan_child_width(
    plan: &LogicalPlan,
    catalog_reader: &Arc<dyn CatalogReader>,
    txn_id: TxnId,
) -> DbResult<Option<usize>> {
    logical_plan_child_width_at_depth(plan, catalog_reader, txn_id, 0)
}

fn logical_plan_child_width_at_depth(
    plan: &LogicalPlan,
    catalog_reader: &Arc<dyn CatalogReader>,
    txn_id: TxnId,
    depth: usize,
) -> DbResult<Option<usize>> {
    if depth >= MAX_OPTIMIZER_PLAN_WIDTH_DEPTH {
        return Err(DbError::internal(
            "optimizer plan width traversal exceeded maximum depth",
        ));
    }
    match plan {
        LogicalPlan::SeqScan { table_id } => table_column_count(catalog_reader, txn_id, *table_id),
        LogicalPlan::ProjectTable {
            table_id, outputs, ..
        } => {
            if outputs.is_empty() {
                // No explicit projection: width = number of table columns.
                table_column_count(catalog_reader, txn_id, *table_id)
            } else {
                Ok(Some(outputs.len()))
            }
        }
        LogicalPlan::NestedLoopJoin {
            left,
            right,
            outputs,
            ..
        } => {
            if !outputs.is_empty() {
                return Ok(Some(outputs.len()));
            }
            // No explicit outputs: width = left_width + right_width.
            let lw = logical_plan_child_width_at_depth(left, catalog_reader, txn_id, depth + 1)?;
            let rw = logical_plan_child_width_at_depth(right, catalog_reader, txn_id, depth + 1)?;
            match (lw, rw) {
                (Some(l), Some(r)) => Ok(Some(l + r)),
                _ => Ok(None),
            }
        }
        _ => {
            let fields = plan.output_fields();
            if fields.is_empty() {
                Ok(None)
            } else {
                Ok(Some(fields.len()))
            }
        }
    }
}

fn logical_plan_contains_hybrid_sources(plan: &LogicalPlan) -> bool {
    let mut stack = vec![plan];
    while let Some(plan) = stack.pop() {
        match plan {
            LogicalPlan::HybridFunctionScan { .. } => return true,
            LogicalPlan::ProjectSource { source, .. }
            | LogicalPlan::AggregateSource { source, .. }
            | LogicalPlan::InsertSelect { source, .. } => stack.push(source),
            LogicalPlan::NestedLoopJoin { left, right, .. }
            | LogicalPlan::SetOperation { left, right, .. } => {
                stack.push(right);
                stack.push(left);
            }
            LogicalPlan::RecursiveCte {
                base, recursive, ..
            } => {
                stack.push(recursive);
                stack.push(base);
            }
            LogicalPlan::CypherQuery(_) => {}
            _ => {}
        }
    }
    false
}

/// System column names appended by the type-checker
/// (`compat_relation_with_system_columns`) and the executor
/// (`compat_scan_row_for_table_id`).  Must stay in sync.
const SYSTEM_COLUMN_NAMES: [&str; 7] = ["ctid", "tableoid", "xmin", "xmax", "cmin", "cmax", "oid"];

/// Look up the number of columns for a table from the catalog,
/// **including** the system columns that the type-checker appends.
///
/// This must match the width produced by
/// `compat_relation_with_system_columns` so that `ColumnRef` ordinals
/// in typed expressions are consistent with the width used here for
/// predicate classification and ordinal shifting.
pub(crate) fn table_column_count(
    catalog_reader: &Arc<dyn CatalogReader>,
    txn_id: TxnId,
    table_id: RelationId,
) -> DbResult<Option<usize>> {
    match catalog_reader.get_table_by_id(txn_id, table_id)? {
        Some(desc) => {
            let base = desc.columns.len();
            // Count how many system columns are NOT already present
            // in the table (mirrors `compat_relation_with_system_columns`).
            let system_count = SYSTEM_COLUMN_NAMES
                .iter()
                .filter(|name| {
                    !desc
                        .columns
                        .iter()
                        .any(|c| c.name.eq_ignore_ascii_case(name))
                })
                .count();
            Ok(Some(base + system_count))
        }
        None => Ok(None),
    }
}

// ------------------------------------------------------------------
// Expression helpers
// ------------------------------------------------------------------

/// Decompose an `AND`-connected expression into individual conjuncts.
pub(crate) fn collect_conjuncts(expr: TypedExpr, out: &mut Vec<TypedExpr>) {
    let mut stack = vec![expr];
    while let Some(expr) = stack.pop() {
        match expr.kind {
            TypedExprKind::LogicalAnd { left, right } => {
                stack.push(*right);
                stack.push(*left);
            }
            _ => {
                out.push(expr);
            }
        }
    }
}

/// Collect AND-conjuncts by reference (non-consuming).
pub(crate) fn collect_conjuncts_ref<'a>(expr: &'a TypedExpr, out: &mut Vec<&'a TypedExpr>) {
    let mut stack = vec![expr];
    while let Some(expr) = stack.pop() {
        match &expr.kind {
            TypedExprKind::LogicalAnd { left, right } => {
                stack.push(right);
                stack.push(left);
            }
            _ => {
                out.push(expr);
            }
        }
    }
}

/// Count the number of `AND`-separated conjuncts in an expression.
#[inline]
fn count_conjuncts(expr: &TypedExpr) -> usize {
    let mut count = 0usize;
    let mut stack = vec![expr];
    while let Some(expr) = stack.pop() {
        match &expr.kind {
            TypedExprKind::LogicalAnd { left, right } => {
                stack.push(right);
                stack.push(left);
            }
            _ => count = count.saturating_add(1),
        }
    }
    count
}

/// Combine a list of expressions with `AND`.
pub(crate) fn combine_conjuncts(mut exprs: Vec<TypedExpr>) -> Option<TypedExpr> {
    let mut result = exprs.pop()?;
    while let Some(expr) = exprs.pop() {
        result = TypedExpr::logical_and(expr, result);
    }
    Some(result)
}

/// Iterate over immediate child expressions of a typed expression.
pub(crate) fn for_each_child_expr<'a>(expr: &'a TypedExpr, f: &mut impl FnMut(&'a TypedExpr)) {
    match &expr.kind {
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
        | TypedExprKind::ArrayConcat { left, right }
        | TypedExprKind::Nullif { left, right }
        | TypedExprKind::IsDistinctFrom { left, right, .. }
        | TypedExprKind::ArrayContains { left, right }
        | TypedExprKind::ArrayContainedBy { left, right }
        | TypedExprKind::ArrayOverlap { left, right }
        | TypedExprKind::JsonGet { left, right }
        | TypedExprKind::JsonGetText { left, right }
        | TypedExprKind::JsonPathGet { left, right }
        | TypedExprKind::JsonPathGetText { left, right }
        | TypedExprKind::JsonContains { left, right }
        | TypedExprKind::JsonContainedBy { left, right }
        | TypedExprKind::JsonKeyExists { left, right }
        | TypedExprKind::JsonAnyKeyExists { left, right }
        | TypedExprKind::JsonAllKeysExist { left, right } => {
            f(left);
            f(right);
        }
        TypedExprKind::LogicalNot { expr: inner }
        | TypedExprKind::Negate { expr: inner }
        | TypedExprKind::IsNull { expr: inner, .. }
        | TypedExprKind::Cast { expr: inner, .. } => {
            f(inner);
        }
        TypedExprKind::Like { expr, pattern, .. } => {
            f(expr);
            f(pattern);
        }
        TypedExprKind::InList { expr, list, .. } => {
            f(expr);
            for item in list {
                f(item);
            }
        }
        TypedExprKind::Between {
            expr, low, high, ..
        } => {
            f(expr);
            f(low);
            f(high);
        }
        TypedExprKind::CaseWhen {
            conditions,
            results,
            else_result,
        } => {
            for cond in conditions {
                f(cond);
            }
            for res in results {
                f(res);
            }
            if let Some(e) = else_result {
                f(e);
            }
        }
        TypedExprKind::Coalesce { args }
        | TypedExprKind::ScalarFunction { args, .. }
        | TypedExprKind::UserFunction { args, .. } => {
            for arg in args {
                f(arg);
            }
        }
        TypedExprKind::WindowFunction {
            args,
            partition_by,
            order_by,
            ..
        } => {
            for arg in args {
                f(arg);
            }
            for expr in partition_by {
                f(expr);
            }
            for sort in order_by {
                f(&sort.expr);
            }
        }
        TypedExprKind::ArrayConstruct { elements } => {
            for elem in elements {
                f(elem);
            }
        }
        // Subqueries with a scalar input (e.g. `expr IN (SELECT ...)`) must
        // expose that input so visitors that compute the relation set of a
        // predicate observe the column references it carries.
        TypedExprKind::InSubquery { expr: inner, .. } => {
            f(inner);
        }
        // Leaf nodes (incl. ScalarSubquery / ArraySubquery / ExistsSubquery,
        // which wrap a LogicalPlan rather than a child expr; callers that
        // need to know whether a predicate contains a subquery should use
        // `predicate_contains_subquery_or_outer_ref` instead of relying on
        // `for_each_child_expr` to descend into the plan).
        _ => {}
    }
}

/// Returns true when `expr` (or any of its descendants reachable through
/// `for_each_child_expr`) wraps a subquery or references an outer column.
/// Join reordering and predicate pushdown call this to refuse to relocate
/// predicates whose evaluation correlates with rows outside the chain.
pub(crate) fn predicate_contains_subquery_or_outer_ref(expr: &TypedExpr) -> bool {
    let mut stack = vec![expr];
    while let Some(expr) = stack.pop() {
        if matches!(
            expr.kind,
            TypedExprKind::ScalarSubquery { .. }
                | TypedExprKind::ArraySubquery { .. }
                | TypedExprKind::InSubquery { .. }
                | TypedExprKind::ExistsSubquery { .. }
                | TypedExprKind::OuterColumnRef { .. }
        ) {
            return true;
        }
        for_each_child_expr(expr, &mut |child| stack.push(child));
    }
    false
}
