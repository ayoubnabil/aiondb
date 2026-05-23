//! Greedy join reordering for chains of INNER joins.
//!
//! When a query involves multiple INNER joins (e.g., `A JOIN B JOIN C`),
//! the planner always produces a left-deep tree in FROM-clause order.
//! This pass reorders the relations to minimize intermediate result
//! sizes using a greedy heuristic: at each step, pick the relation
//! that produces the smallest estimated join result with the current
//! partial result.
//!
//! This mirrors PostgreSQL's `standard_join_search()` in `joinrels.c`,
//! simplified to a greedy approach (PostgreSQL uses dynamic programming
//! for ≤12 relations and GEQO for more).
//!
//! ### Correctness notes
//!
//! A previous implementation was deleted due to three bugs:
//! 1. No ordinal remapping after reorder → wrong column references
//! 2. `flatten_join_tree` lost filter predicates from inner joins
//! 3. `get_column_count` excluded system columns → broken ordinal mapping
//!
//! This rewrite addresses all three by:
//! 1. Building an explicit ordinal mapping and applying it to ALL expressions
//! 2. Collecting both `condition` AND `filter` predicates during flattening
//! 3. Using `logical_plan_child_width()` which includes system columns

use std::collections::BTreeSet;
use std::sync::Arc;

use aiondb_catalog::CatalogReader;
use aiondb_core::{DbError, DbResult, TxnId};
use aiondb_plan::{
    JoinType, LogicalPlan, ProjectionExpr, SetOperationType, SortExpr, TypedExpr, TypedExprKind,
};

use crate::{
    i64_to_f64,
    physical_builder::{estimate_filter_selectivity, estimate_hybrid_function_rows},
    predicate_pushdown, usize_to_f64,
};

/// Minimum number of relations required to attempt reordering.
/// With only 2 relations there is nothing to reorder.
const MIN_RELATIONS_FOR_REORDER: usize = 3;
/// Keep this pass off attacker-sized left-deep join chains. Rebuilding remains
/// iterative, but flattening and expression remapping still walk recursive plan
/// and expression structures.
const MAX_RELATIONS_FOR_REORDER: usize = 128;

/// Try to reorder a chain of INNER joins for better performance.
///
/// Returns the reordered plan, or the original plan unchanged if
/// reordering is not applicable (non-inner joins, too few relations,
/// width computation failures).
pub(crate) fn try_reorder_joins(
    plan: LogicalPlan,
    catalog_reader: &Arc<dyn CatalogReader>,
    txn_id: TxnId,
) -> DbResult<LogicalPlan> {
    let original_plan = plan.clone();
    let LogicalPlan::NestedLoopJoin {
        join_type: JoinType::Inner,
        outputs,
        ..
    } = &plan
    else {
        return Ok(plan);
    };
    if outputs.is_empty() {
        return Ok(plan);
    }

    if count_left_deep_inner_join_relations(&plan) > MAX_RELATIONS_FOR_REORDER {
        return Ok(plan);
    }
    if !join_reorder_enabled() {
        return Ok(plan);
    }

    // Flatten the join tree into individual relations and predicates.
    let mut relations: Vec<LogicalPlan> = Vec::new();
    let mut predicates: Vec<TypedExpr> = Vec::new();
    let mut outer_outputs: Vec<ProjectionExpr> = Vec::new();
    let mut outer_filter: Option<TypedExpr> = None;
    let mut outer_order_by: Vec<SortExpr> = Vec::new();
    let mut outer_limit: Option<TypedExpr> = None;
    let mut outer_offset: Option<TypedExpr> = None;
    let mut outer_distinct = false;
    let mut outer_distinct_on: Vec<TypedExpr> = Vec::new();

    if !flatten_inner_join_chain(
        plan,
        &mut relations,
        &mut predicates,
        &mut outer_outputs,
        &mut outer_filter,
        &mut outer_order_by,
        &mut outer_limit,
        &mut outer_offset,
        &mut outer_distinct,
        &mut outer_distinct_on,
    ) {
        // Flattening failed (non-inner join encountered, etc.).
        return Ok(original_plan);
    }

    // Compute widths for each relation BEFORE the early return so that
    // rebuild_left_deep always has correct column counts for predicate
    // distribution.  Without this, SeqScan nodes report width=0 via
    // output_fields(), causing join conditions to be dropped.
    let mut widths: Vec<usize> = Vec::with_capacity(relations.len());
    for rel in &relations {
        match predicate_pushdown::logical_plan_child_width(rel, catalog_reader, txn_id)? {
            Some(w) => widths.push(w),
            None => {
                let fields = rel.output_fields();
                if fields.is_empty() {
                    // Can't determine width; keep the original plan rather
                    // than rebuilding an approximated flattened tree.
                    return Ok(original_plan);
                }
                widths.push(fields.len());
            }
        }
    }

    if relations.len() < MIN_RELATIONS_FOR_REORDER {
        return Ok(original_plan);
    }

    // Build ordinal ranges for original order.
    // relation i occupies ordinals [start_i, start_i + width_i).
    let original_starts: Vec<usize> = widths
        .iter()
        .scan(0usize, |acc, &w| {
            let start = *acc;
            *acc += w;
            Some(start)
        })
        .collect();

    // Greedy reordering: pick relations to minimize intermediate size.
    let predicate_infos: Vec<PredicateRelInfo> = predicates
        .iter()
        .map(|predicate| predicate_relation_info(predicate, &original_starts, &widths))
        .collect();
    let mut base_predicates_by_relation: Vec<Vec<TypedExpr>> = vec![Vec::new(); relations.len()];
    let mut join_predicates: Vec<TypedExpr> = Vec::new();
    let mut join_predicate_infos: Vec<PredicateRelInfo> = Vec::new();
    for (predicate, info) in predicates.into_iter().zip(predicate_infos.into_iter()) {
        if let [relation_idx] = info.relation_refs.as_slice() {
            let base_start = original_starts[*relation_idx];
            base_predicates_by_relation[*relation_idx]
                .push(predicate_pushdown::map_ordinals(predicate, |ordinal| {
                    ordinal.saturating_sub(base_start)
                }));
        } else {
            join_predicates.push(predicate);
            join_predicate_infos.push(info);
        }
    }
    for (relation, base_preds) in relations
        .iter_mut()
        .zip(base_predicates_by_relation.into_iter())
    {
        if let Some(predicate) = predicate_pushdown::combine_conjuncts(base_preds) {
            *relation = predicate_pushdown::push_into_child(relation.clone(), predicate);
        }
    }
    let join_graph_connected = join_graph_is_connected(relations.len(), &join_predicate_infos);
    if !join_graph_connected {
        return Ok(original_plan);
    }

    let new_order = greedy_order(&relations, &join_predicate_infos);

    // Check if the order actually changed.
    let is_identity = new_order.iter().enumerate().all(|(i, &j)| i == j);
    if is_identity {
        return Ok(original_plan);
    }

    // Build ordinal mapping: old_ordinal → new_ordinal.
    let new_starts: Vec<usize> = {
        let mut starts = vec![0usize; relations.len()];
        let mut offset = 0;
        for &orig_idx in &new_order {
            starts[orig_idx] = offset;
            offset += widths[orig_idx];
        }
        starts
    };

    let ordinal_map = |old_ord: usize| -> usize {
        // Find which original relation this ordinal belongs to.
        for (i, (&orig_start, &width)) in original_starts.iter().zip(widths.iter()).enumerate() {
            if old_ord >= orig_start && old_ord < orig_start + width {
                let local = old_ord - orig_start;
                return new_starts[i] + local;
            }
        }
        old_ord // fallback (shouldn't happen)
    };

    // Remap all predicates.
    let remapped_predicates: Vec<TypedExpr> = join_predicates
        .into_iter()
        .map(|p| predicate_pushdown::map_ordinals(p, ordinal_map))
        .collect();

    // Remap outer expressions.
    let remapped_outputs: Vec<ProjectionExpr> = outer_outputs
        .into_iter()
        .map(|p| remap_projection(p, ordinal_map))
        .collect();
    let remapped_filter = outer_filter.map(|f| predicate_pushdown::map_ordinals(f, ordinal_map));
    let remapped_order_by: Vec<SortExpr> = outer_order_by
        .into_iter()
        .map(|s| remap_sort(s, ordinal_map))
        .collect();
    let remapped_limit = outer_limit.map(|l| predicate_pushdown::map_ordinals(l, ordinal_map));
    let remapped_offset = outer_offset.map(|o| predicate_pushdown::map_ordinals(o, ordinal_map));
    let remapped_distinct_on: Vec<TypedExpr> = outer_distinct_on
        .into_iter()
        .map(|e| predicate_pushdown::map_ordinals(e, ordinal_map))
        .collect();

    // Reorder relations.
    let mut reordered: Vec<LogicalPlan> = Vec::with_capacity(relations.len());
    // We need to extract by index, so collect into a vec of Options.
    let mut relation_slots: Vec<Option<LogicalPlan>> = relations.into_iter().map(Some).collect();
    for &idx in &new_order {
        let Some(slot) = relation_slots.get_mut(idx) else {
            return Err(DbError::internal(
                "join reorder produced an out-of-range relation index",
            ));
        };
        let Some(relation) = slot.take() else {
            return Err(DbError::internal(
                "join reorder produced a duplicate relation index",
            ));
        };
        reordered.push(relation);
    }

    // Recompute widths in new order.
    let reordered_widths: Vec<usize> = new_order.iter().map(|&i| widths[i]).collect();
    if join_graph_connected {
        Ok(rebuild_balanced_join_tree(
            reordered,
            remapped_predicates,
            Some(&reordered_widths),
            remapped_outputs,
            remapped_filter,
            remapped_order_by,
            remapped_limit,
            remapped_offset,
            outer_distinct,
            remapped_distinct_on,
        ))
    } else {
        Ok(rebuild_left_deep(
            reordered,
            remapped_predicates,
            Some(&reordered_widths),
            remapped_outputs,
            remapped_filter,
            remapped_order_by,
            remapped_limit,
            remapped_offset,
            outer_distinct,
            remapped_distinct_on,
        ))
    }
}

fn join_reorder_enabled() -> bool {
    if env_flag_enabled("AIONDB_DISABLE_JOIN_REORDER") {
        return false;
    }
    std::env::var("AIONDB_ENABLE_JOIN_REORDER")
        .ok()
        .map(|value| env_value_enabled(&value))
        .unwrap_or(true)
}

fn env_flag_enabled(name: &str) -> bool {
    std::env::var(name)
        .ok()
        .is_some_and(|value| env_value_enabled(&value))
}

fn env_value_enabled(value: &str) -> bool {
    !(value == "0"
        || value.eq_ignore_ascii_case("false")
        || value.eq_ignore_ascii_case("no")
        || value.eq_ignore_ascii_case("off"))
}

// ------------------------------------------------------------------
// Flattening
// ------------------------------------------------------------------

fn count_left_deep_inner_join_relations(plan: &LogicalPlan) -> usize {
    let mut count = 1usize;
    let mut current = plan;
    while let LogicalPlan::NestedLoopJoin {
        left,
        join_type: JoinType::Inner,
        ..
    } = current
    {
        count = count.saturating_add(1);
        current = left.as_ref();
    }
    count
}

/// Recursively flatten a left-deep chain of INNER joins into a list
/// of leaf relations and predicates. Returns false if a non-inner join
/// is encountered (in which case reordering is unsafe).
fn flatten_inner_join_chain(
    plan: LogicalPlan,
    relations: &mut Vec<LogicalPlan>,
    predicates: &mut Vec<TypedExpr>,
    outer_outputs: &mut Vec<ProjectionExpr>,
    outer_filter: &mut Option<TypedExpr>,
    outer_order_by: &mut Vec<SortExpr>,
    outer_limit: &mut Option<TypedExpr>,
    outer_offset: &mut Option<TypedExpr>,
    outer_distinct: &mut bool,
    outer_distinct_on: &mut Vec<TypedExpr>,
) -> bool {
    match plan {
        LogicalPlan::NestedLoopJoin {
            left,
            right,
            join_type: JoinType::Inner,
            condition,
            outputs,
            filter,
            order_by,
            limit,
            offset,
            distinct,
            distinct_on,
        } => {
            // Refuse to flatten chains whose predicates contain a subquery
            // or outer-column reference: reordering can place them at a
            // level where the referenced outer relation isn't yet bound,
            // plan stand.
            if let Some(cond) = &condition {
                if predicate_pushdown::predicate_contains_subquery_or_outer_ref(cond) {
                    return false;
                }
            }
            if let Some(filt) = &filter {
                if predicate_pushdown::predicate_contains_subquery_or_outer_ref(filt) {
                    return false;
                }
            }
            // Collect predicates from this join node.
            if let Some(cond) = condition {
                let mut conjuncts = Vec::new();
                predicate_pushdown::collect_conjuncts(cond, &mut conjuncts);
                predicates.extend(conjuncts);
            }
            if let Some(filt) = filter {
                let mut conjuncts = Vec::new();
                predicate_pushdown::collect_conjuncts(filt, &mut conjuncts);
                predicates.extend(conjuncts);
            }

            // The outermost join's metadata (outputs, order_by, limit, etc.)
            // must be preserved. We detect "outermost" by checking if these
            // fields are non-empty (inner joins in the chain have empty
            // outputs/order_by/limit).
            if !outputs.is_empty() && outer_outputs.is_empty() {
                *outer_outputs = outputs;
            }
            if !order_by.is_empty() && outer_order_by.is_empty() {
                *outer_order_by = order_by;
            }
            if limit.is_some() && outer_limit.is_none() {
                *outer_limit = limit;
            }
            if offset.is_some() && outer_offset.is_none() {
                *outer_offset = offset;
            }
            if distinct {
                *outer_distinct = true;
            }
            if !distinct_on.is_empty() && outer_distinct_on.is_empty() {
                *outer_distinct_on = distinct_on;
            }

            // Recurse into left child (which may be another join).
            if !flatten_inner_join_chain(
                *left,
                relations,
                predicates,
                outer_outputs,
                outer_filter,
                outer_order_by,
                outer_limit,
                outer_offset,
                outer_distinct,
                outer_distinct_on,
            ) {
                return false;
            }
            // Right child is always a leaf in left-deep trees.
            relations.push(*right);
            true
        }
        // Non-join leaf: add directly.
        leaf => {
            relations.push(leaf);
            true
        }
    }
}

// ------------------------------------------------------------------
// Greedy ordering
// ------------------------------------------------------------------

/// Greedy join ordering heuristic: start with the smallest relation,
/// then at each step add the relation that produces the smallest
/// estimated join result.
#[derive(Debug, Clone)]
struct PredicateRelInfo {
    relation_refs: Vec<usize>,
    join_selectivity: f64,
    base_selectivity: f64,
}

fn greedy_order(relations: &[LogicalPlan], predicate_infos: &[PredicateRelInfo]) -> Vec<usize> {
    let n = relations.len();
    let estimates: Vec<f64> = relations.iter().map(|r| estimate_plan_rows(r)).collect();
    let adjusted_estimates: Vec<f64> = estimates
        .iter()
        .enumerate()
        .map(|(relation_idx, estimate)| {
            let mut adjusted = *estimate;
            for predicate in predicate_infos {
                if predicate.relation_refs.as_slice() == [relation_idx] {
                    adjusted *= predicate.base_selectivity;
                }
            }
            adjusted.max(1.0)
        })
        .collect();

    // Avoid reordering when estimates are all equal (no statistics).
    let all_equal = adjusted_estimates
        .windows(2)
        .all(|w| (w[0] - w[1]).abs() < f64::EPSILON);
    if all_equal {
        return (0..n).collect();
    }

    let mut used = vec![false; n];
    let mut order = Vec::with_capacity(n);

    // Start with the smallest relation.
    let first = adjusted_estimates
        .iter()
        .enumerate()
        .filter(|(i, _)| !used[*i])
        .min_by(|(_, a), (_, b)| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal))
        .map(|(i, _)| i)
        .unwrap_or(0);
    used[first] = true;
    order.push(first);
    let mut used_relations = BTreeSet::from([first]);
    let mut current_rows = adjusted_estimates[first];

    // Greedily add the relation that minimizes the intermediate result.
    for _ in 1..n {
        let mut best_idx = None;
        let mut best_cost = f64::MAX;

        for (i, est) in adjusted_estimates.iter().enumerate() {
            if used[i] {
                continue;
            }
            let mut join_selectivity: f64 = 1.0;
            for predicate in predicate_infos {
                if !predicate.relation_refs.contains(&i) {
                    continue;
                }
                if !predicate
                    .relation_refs
                    .iter()
                    .all(|relation| *relation == i || used_relations.contains(relation))
                {
                    continue;
                }
                if predicate
                    .relation_refs
                    .iter()
                    .any(|relation| used_relations.contains(relation))
                {
                    join_selectivity *= predicate.join_selectivity;
                }
            }
            let join_result = (current_rows * est * join_selectivity.max(1.0e-6)).max(1.0);
            if join_result < best_cost {
                best_cost = join_result;
                best_idx = Some(i);
            }
        }

        let next = best_idx.unwrap_or(0);
        used[next] = true;
        order.push(next);
        used_relations.insert(next);
        // Update the running estimate for the intermediate result.
        current_rows = best_cost.max(1.0);
    }

    order
}

fn predicate_relation_info(
    predicate: &TypedExpr,
    relation_starts: &[usize],
    relation_widths: &[usize],
) -> PredicateRelInfo {
    let mut relation_refs = BTreeSet::new();
    collect_predicate_relation_refs(
        predicate,
        relation_starts,
        relation_widths,
        &mut relation_refs,
    );
    let relation_refs: Vec<usize> = relation_refs.into_iter().collect();
    PredicateRelInfo {
        join_selectivity: join_selectivity_for_predicate(predicate),
        base_selectivity: base_selectivity_for_predicate(predicate),
        relation_refs,
    }
}

fn base_selectivity_for_predicate(predicate: &TypedExpr) -> f64 {
    estimate_filter_selectivity(predicate)
}

fn join_selectivity_for_predicate(predicate: &TypedExpr) -> f64 {
    if !is_column_column_equality(predicate) {
        return estimate_filter_selectivity(predicate);
    }

    let Some((left_name, right_name)) = column_column_equality_names(predicate) else {
        return 0.1;
    };
    if equality_is_key_like(left_name, right_name) {
        0.005
    } else {
        // Broad equalities such as tenant_id = tenant_id are useful filters,
        // but they are poor join-order anchors. Treating them as selective
        // makes the greedy planner pull large dimension tables too early and
        // creates avoidable cross-products in hybrid SQL/graph/vector plans.
        1.0
    }
}

fn join_graph_is_connected(relation_count: usize, predicate_infos: &[PredicateRelInfo]) -> bool {
    if relation_count <= 1 {
        return true;
    }

    let mut adjacency = vec![Vec::<usize>::new(); relation_count];
    for predicate in predicate_infos {
        if predicate.relation_refs.len() < 2 {
            continue;
        }
        for (idx, &left) in predicate.relation_refs.iter().enumerate() {
            for &right in &predicate.relation_refs[idx + 1..] {
                adjacency[left].push(right);
                adjacency[right].push(left);
            }
        }
    }

    let Some(start) = adjacency.iter().position(|neighbors| !neighbors.is_empty()) else {
        return false;
    };
    let mut stack = vec![start];
    let mut seen = vec![false; relation_count];
    seen[start] = true;
    while let Some(node) = stack.pop() {
        for &next in &adjacency[node] {
            if !seen[next] {
                seen[next] = true;
                stack.push(next);
            }
        }
    }

    seen.into_iter().all(|visited| visited)
}

fn collect_predicate_relation_refs(
    expr: &TypedExpr,
    relation_starts: &[usize],
    relation_widths: &[usize],
    out: &mut BTreeSet<usize>,
) {
    let mut stack = vec![expr];
    while let Some(expr) = stack.pop() {
        if let TypedExprKind::ColumnRef { ordinal, .. } = &expr.kind {
            if let Some(relation_idx) =
                ordinal_to_relation_idx(*ordinal, relation_starts, relation_widths)
            {
                out.insert(relation_idx);
            }
        }
        predicate_pushdown::for_each_child_expr(expr, &mut |child| stack.push(child));
    }
}

fn ordinal_to_relation_idx(
    ordinal: usize,
    relation_starts: &[usize],
    relation_widths: &[usize],
) -> Option<usize> {
    relation_starts
        .iter()
        .zip(relation_widths.iter())
        .enumerate()
        .find_map(|(idx, (&start, &width))| {
            (ordinal >= start && ordinal < start + width).then_some(idx)
        })
}

fn is_column_column_equality(expr: &TypedExpr) -> bool {
    matches!(
        &expr.kind,
        TypedExprKind::BinaryEq { left, right }
            if column_ref_name(left).is_some() && column_ref_name(right).is_some()
    )
}

fn column_column_equality_names(expr: &TypedExpr) -> Option<(&str, &str)> {
    match &expr.kind {
        TypedExprKind::BinaryEq { left, right } => {
            Some((column_ref_name(left)?, column_ref_name(right)?))
        }
        _ => None,
    }
}

fn column_ref_name(expr: &TypedExpr) -> Option<&str> {
    match &expr.kind {
        TypedExprKind::ColumnRef { name, .. } => Some(name.as_str()),
        TypedExprKind::Cast { expr, .. } => column_ref_name(expr),
        _ => None,
    }
}

fn equality_is_key_like(left_name: &str, right_name: &str) -> bool {
    let left = normalized_column_name(left_name);
    let right = normalized_column_name(right_name);
    if left == "tenant_id" || right == "tenant_id" {
        return false;
    }
    is_key_like_column(left) && is_key_like_column(right)
}

fn normalized_column_name(name: &str) -> &str {
    name.rsplit(['.', '\0']).next().unwrap_or(name)
}

fn is_key_like_column(name: &str) -> bool {
    name.eq_ignore_ascii_case("id")
        || name.ends_with("_id")
        || name.eq_ignore_ascii_case("source")
        || name.eq_ignore_ascii_case("target")
        || name.eq_ignore_ascii_case("source_id")
        || name.eq_ignore_ascii_case("target_id")
}

// ------------------------------------------------------------------
// Rebuild
// ------------------------------------------------------------------

/// Rebuild a left-deep join tree from reordered relations and
/// remapped predicates.
///
/// Predicates are distributed to the earliest (innermost) join level
/// where all their referenced columns are available, rather than
/// being piled on the outermost join. This avoids cross-join
/// intermediates even if predicate pushdown is later skipped.
fn rebuild_left_deep(
    relations: Vec<LogicalPlan>,
    predicates: Vec<TypedExpr>,
    known_widths: Option<&[usize]>,
    outputs: Vec<ProjectionExpr>,
    filter: Option<TypedExpr>,
    order_by: Vec<SortExpr>,
    limit: Option<TypedExpr>,
    offset: Option<TypedExpr>,
    distinct: bool,
    distinct_on: Vec<TypedExpr>,
) -> LogicalPlan {
    if relations.is_empty() {
        return LogicalPlan::ProjectOnce {
            outputs,
            filter,
            order_by,
            limit,
            offset,
            distinct,
            distinct_on,
        };
    }
    if relations.len() == 1 {
        let mut rels = relations;
        if let Some(plan) = rels.pop() {
            return plan;
        }
        return LogicalPlan::ProjectOnce {
            outputs,
            filter,
            order_by,
            limit,
            offset,
            distinct,
            distinct_on,
        };
    }

    // Compute cumulative column widths. After joining relations
    // [0..=k], the available columns span ordinals [0..cum_widths[k]).
    // Use pre-computed widths from catalog when available (SeqScan
    // nodes return empty output_fields, which would break predicate
    // distribution).
    let fallback_widths: Vec<usize>;
    let rel_widths: &[usize] = match known_widths {
        Some(w) if w.len() == relations.len() => w,
        _ => {
            fallback_widths = relations
                .iter()
                .map(|r| r.output_fields().len().max(1))
                .collect();
            &fallback_widths
        }
    };
    let cum_widths: Vec<usize> = rel_widths
        .iter()
        .scan(0usize, |acc, &w| {
            *acc += w;
            Some(*acc)
        })
        .collect();

    // For each predicate, find the max column ordinal it references.
    let pred_max_ords: Vec<usize> = predicates
        .iter()
        .map(|p| max_ordinal(p).unwrap_or(0))
        .collect();

    let n = relations.len();
    let mut rels = relations.into_iter();
    let Some(first) = rels.next() else {
        return LogicalPlan::ProjectOnce {
            outputs,
            filter,
            order_by,
            limit,
            offset,
            distinct,
            distinct_on,
        };
    };
    let Some(second) = rels.next() else {
        return first;
    };

    // Predicates eligible at join level k (joining relations 0..=k+1)
    // are those whose max ordinal < cum_widths[k+1].
    let mut used_pred = vec![false; predicates.len()];

    let level_available = cum_widths[1]; // after joining rel[0]+rel[1]
    let first_width = rel_widths[0];
    let mut initial_left_preds: Vec<TypedExpr> = Vec::new();
    let mut initial_right_preds: Vec<TypedExpr> = Vec::new();
    let mut level_preds: Vec<TypedExpr> = Vec::new();
    for (i, pred) in predicates.iter().enumerate() {
        if !used_pred[i] && pred_max_ords[i] < level_available {
            match predicate_pushdown::classify_predicate(pred, first_width) {
                predicate_pushdown::PredicateSide::LeftOnly => {
                    initial_left_preds.push(pred.clone());
                }
                predicate_pushdown::PredicateSide::RightOnly => {
                    initial_right_preds
                        .push(predicate_pushdown::map_ordinals(pred.clone(), |ordinal| {
                            ordinal.saturating_sub(first_width)
                        }));
                }
                predicate_pushdown::PredicateSide::Both
                | predicate_pushdown::PredicateSide::Unknown => {
                    level_preds.push(pred.clone());
                }
            }
            used_pred[i] = true;
        }
    }

    let is_last = n == 2;
    let level_condition = predicate_pushdown::combine_conjuncts(level_preds);
    let first = match predicate_pushdown::combine_conjuncts(initial_left_preds) {
        Some(pred) => predicate_pushdown::push_into_child(first, pred),
        None => first,
    };
    let second = match predicate_pushdown::combine_conjuncts(initial_right_preds) {
        Some(pred) => predicate_pushdown::push_into_child(second, pred),
        None => second,
    };

    let mut current = LogicalPlan::NestedLoopJoin {
        left: Box::new(first),
        right: Box::new(second),
        join_type: JoinType::Inner,
        condition: level_condition,
        outputs: if is_last { outputs.clone() } else { Vec::new() },
        filter: if is_last { filter.clone() } else { None },
        order_by: if is_last {
            order_by.clone()
        } else {
            Vec::new()
        },
        limit: if is_last { limit.clone() } else { None },
        offset: if is_last { offset.clone() } else { None },
        distinct: if is_last { distinct } else { false },
        distinct_on: if is_last {
            distinct_on.clone()
        } else {
            Vec::new()
        },
    };

    if is_last {
        return current;
    }

    let remaining: Vec<LogicalPlan> = rels.collect();
    let last_idx = remaining.len() - 1;
    for (i, rel) in remaining.into_iter().enumerate() {
        let level = i + 2; // joining relation index
        let is_outermost = i == last_idx;

        let level_available = cum_widths[level];
        let left_width = cum_widths[level - 1];
        let mut left_only_preds: Vec<TypedExpr> = Vec::new();
        let mut right_only_preds: Vec<TypedExpr> = Vec::new();
        let mut level_preds: Vec<TypedExpr> = Vec::new();
        for (j, pred) in predicates.iter().enumerate() {
            if !used_pred[j] && pred_max_ords[j] < level_available {
                match predicate_pushdown::classify_predicate(pred, left_width) {
                    predicate_pushdown::PredicateSide::LeftOnly => {
                        left_only_preds.push(pred.clone());
                    }
                    predicate_pushdown::PredicateSide::RightOnly => {
                        right_only_preds
                            .push(predicate_pushdown::map_ordinals(pred.clone(), |ordinal| {
                                ordinal.saturating_sub(left_width)
                            }));
                    }
                    predicate_pushdown::PredicateSide::Both
                    | predicate_pushdown::PredicateSide::Unknown => {
                        level_preds.push(pred.clone());
                    }
                }
                used_pred[j] = true;
            }
        }
        // Any remaining predicates go on the outermost join.
        if is_outermost {
            for (j, pred) in predicates.iter().enumerate() {
                if !used_pred[j] {
                    level_preds.push(pred.clone());
                    used_pred[j] = true;
                }
            }
        }
        let level_condition = predicate_pushdown::combine_conjuncts(level_preds);
        current = match predicate_pushdown::combine_conjuncts(left_only_preds) {
            Some(pred) => predicate_pushdown::push_into_child(current, pred),
            None => current,
        };
        let rel = match predicate_pushdown::combine_conjuncts(right_only_preds) {
            Some(pred) => predicate_pushdown::push_into_child(rel, pred),
            None => rel,
        };

        current = LogicalPlan::NestedLoopJoin {
            left: Box::new(current),
            right: Box::new(rel),
            join_type: JoinType::Inner,
            condition: level_condition,
            outputs: if is_outermost {
                outputs.clone()
            } else {
                Vec::new()
            },
            filter: if is_outermost { filter.clone() } else { None },
            order_by: if is_outermost {
                order_by.clone()
            } else {
                Vec::new()
            },
            limit: if is_outermost { limit.clone() } else { None },
            offset: if is_outermost { offset.clone() } else { None },
            distinct: if is_outermost { distinct } else { false },
            distinct_on: if is_outermost {
                distinct_on.clone()
            } else {
                Vec::new()
            },
        };
    }

    current
}

fn rebuild_balanced_join_tree(
    relations: Vec<LogicalPlan>,
    predicates: Vec<TypedExpr>,
    known_widths: Option<&[usize]>,
    outputs: Vec<ProjectionExpr>,
    filter: Option<TypedExpr>,
    order_by: Vec<SortExpr>,
    limit: Option<TypedExpr>,
    offset: Option<TypedExpr>,
    distinct: bool,
    distinct_on: Vec<TypedExpr>,
) -> LogicalPlan {
    if relations.len() <= 2 {
        return rebuild_left_deep(
            relations,
            predicates,
            known_widths,
            outputs,
            filter,
            order_by,
            limit,
            offset,
            distinct,
            distinct_on,
        );
    }

    let fallback_widths: Vec<usize>;
    let rel_widths: &[usize] = match known_widths {
        Some(w) if w.len() == relations.len() => w,
        _ => {
            fallback_widths = relations
                .iter()
                .map(|r| r.output_fields().len().max(1))
                .collect();
            &fallback_widths
        }
    };
    fn build_subtree(
        relations: &[LogicalPlan],
        rel_widths: &[usize],
        predicates: Vec<TypedExpr>,
    ) -> LogicalPlan {
        if relations.len() == 1 {
            return match predicate_pushdown::combine_conjuncts(predicates) {
                Some(pred) => predicate_pushdown::push_into_child(relations[0].clone(), pred),
                None => relations[0].clone(),
            };
        }

        let mid = relations.len() / 2;
        let left_total_width: usize = rel_widths[..mid].iter().sum();
        let total_width: usize = rel_widths.iter().sum();

        let mut left_preds = Vec::new();
        let mut right_preds = Vec::new();
        let mut current_preds = Vec::new();

        for predicate in predicates {
            let min_ord = min_ordinal(&predicate).unwrap_or(0);
            let max_ord = max_ordinal(&predicate).unwrap_or(0);
            if max_ord < left_total_width {
                left_preds.push(predicate);
            } else if min_ord >= left_total_width && max_ord < total_width {
                right_preds.push(predicate_pushdown::map_ordinals(predicate, |ordinal| {
                    ordinal.saturating_sub(left_total_width)
                }));
            } else {
                current_preds.push(predicate);
            }
        }

        if (mid > 1 && !predicates_connect_relation_subset(&left_preds, &rel_widths[..mid]))
            || (relations.len() - mid > 1
                && !predicates_connect_relation_subset(&right_preds, &rel_widths[mid..]))
        {
            return rebuild_left_deep(
                relations.to_vec(),
                current_preds
                    .into_iter()
                    .chain(left_preds)
                    .chain(right_preds.into_iter().map(|predicate| {
                        predicate_pushdown::map_ordinals(predicate, |ordinal| {
                            ordinal.saturating_add(left_total_width)
                        })
                    }))
                    .collect(),
                Some(rel_widths),
                Vec::new(),
                None,
                Vec::new(),
                None,
                None,
                false,
                Vec::new(),
            );
        }

        let left = build_subtree(&relations[..mid], &rel_widths[..mid], left_preds);
        let right = build_subtree(&relations[mid..], &rel_widths[mid..], right_preds);

        LogicalPlan::NestedLoopJoin {
            left: Box::new(left),
            right: Box::new(right),
            join_type: JoinType::Inner,
            condition: predicate_pushdown::combine_conjuncts(current_preds),
            outputs: Vec::new(),
            filter: None,
            order_by: Vec::new(),
            limit: None,
            offset: None,
            distinct: false,
            distinct_on: Vec::new(),
        }
    }

    let mut root = build_subtree(&relations, rel_widths, predicates);

    if let LogicalPlan::NestedLoopJoin {
        outputs: root_outputs,
        filter: root_filter,
        order_by: root_order_by,
        limit: root_limit,
        offset: root_offset,
        distinct: root_distinct,
        distinct_on: root_distinct_on,
        ..
    } = &mut root
    {
        *root_outputs = outputs;
        *root_filter = filter;
        *root_order_by = order_by;
        *root_limit = limit;
        *root_offset = offset;
        *root_distinct = distinct;
        *root_distinct_on = distinct_on;
    }

    root
}

fn predicates_connect_relation_subset(predicates: &[TypedExpr], rel_widths: &[usize]) -> bool {
    if rel_widths.len() <= 1 {
        return true;
    }
    let relation_starts: Vec<usize> = rel_widths
        .iter()
        .scan(0usize, |acc, &width| {
            let start = *acc;
            *acc = acc.saturating_add(width);
            Some(start)
        })
        .collect();
    let predicate_infos: Vec<PredicateRelInfo> = predicates
        .iter()
        .map(|predicate| predicate_relation_info(predicate, &relation_starts, rel_widths))
        .collect();
    join_graph_is_connected(rel_widths.len(), &predicate_infos)
}

/// Find the maximum `ColumnRef` ordinal in an expression.
fn max_ordinal(expr: &TypedExpr) -> Option<usize> {
    let mut max = None;
    let mut stack = vec![expr];
    while let Some(expr) = stack.pop() {
        match &expr.kind {
            TypedExprKind::ColumnRef { ordinal, .. } => {
                max = Some(max.map_or(*ordinal, |current: usize| current.max(*ordinal)));
            }
            _ => predicate_pushdown::for_each_child_expr(expr, &mut |child| {
                stack.push(child);
            }),
        }
    }
    max
}

fn min_ordinal(expr: &TypedExpr) -> Option<usize> {
    let mut min = None;
    let mut stack = vec![expr];
    while let Some(expr) = stack.pop() {
        match &expr.kind {
            TypedExprKind::ColumnRef { ordinal, .. } => {
                min = Some(min.map_or(*ordinal, |current: usize| current.min(*ordinal)));
            }
            _ => predicate_pushdown::for_each_child_expr(expr, &mut |child| {
                stack.push(child);
            }),
        }
    }
    min
}

// ------------------------------------------------------------------
// Expression remapping helpers
// ------------------------------------------------------------------

fn remap_projection(proj: ProjectionExpr, f: impl Fn(usize) -> usize + Copy) -> ProjectionExpr {
    ProjectionExpr {
        field: proj.field,
        expr: predicate_pushdown::map_ordinals(proj.expr, f),
    }
}

fn remap_sort(sort: SortExpr, f: impl Fn(usize) -> usize + Copy) -> SortExpr {
    SortExpr {
        expr: predicate_pushdown::map_ordinals(sort.expr, f),
        descending: sort.descending,
        nulls_first: sort.nulls_first,
    }
}

/// Build a `PhysicalBuilder`-compatible row estimate for a logical plan.
fn estimate_plan_rows(plan: &LogicalPlan) -> f64 {
    // Build a temporary physical plan to use the existing estimator.
    // For simple cases (ProjectTable, SeqScan), we can estimate directly.
    match plan {
        LogicalPlan::ProjectOnce {
            filter,
            distinct,
            distinct_on,
            limit,
            offset,
            ..
        } => {
            let filtered = apply_logical_filter_estimate(1.0, filter.as_ref());
            let deduped = apply_logical_distinct_reduction(filtered, *distinct, distinct_on);
            let offset_rows = apply_logical_offset(deduped, offset.as_ref());
            apply_logical_limit(offset_rows, limit.as_ref())
        }
        LogicalPlan::ProjectTable {
            filter,
            distinct,
            distinct_on,
            limit,
            offset,
            ..
        }
        | LogicalPlan::LockingProjectTable {
            filter,
            distinct,
            distinct_on,
            limit,
            offset,
            ..
        } => {
            let base = 1000.0;
            let filtered = apply_logical_filter_estimate(base, filter.as_ref());
            let deduped = apply_logical_distinct_reduction(filtered, *distinct, distinct_on);
            let offset_rows = apply_logical_offset(deduped, offset.as_ref());
            apply_logical_limit(offset_rows, limit.as_ref())
        }
        LogicalPlan::SeqScan { .. } => 1000.0,
        LogicalPlan::ProjectSource {
            source,
            filter,
            distinct,
            distinct_on,
            limit,
            offset,
            ..
        } => {
            let base = estimate_plan_rows(source);
            let filtered = apply_logical_filter_estimate(base, filter.as_ref());
            let deduped = apply_logical_distinct_reduction(filtered, *distinct, distinct_on);
            let offset_rows = apply_logical_offset(deduped, offset.as_ref());
            apply_logical_limit(offset_rows, limit.as_ref())
        }
        LogicalPlan::Aggregate {
            group_by,
            having,
            filter,
            distinct,
            distinct_on,
            limit,
            offset,
            ..
        } => {
            let filtered_source = apply_logical_filter_estimate(1000.0, filter.as_ref());
            let grouped = apply_logical_group_reduction(filtered_source, group_by);
            let having_filtered = apply_logical_filter_estimate(grouped, having.as_ref());
            let deduped = apply_logical_distinct_reduction(having_filtered, *distinct, distinct_on);
            let offset_rows = apply_logical_offset(deduped, offset.as_ref());
            apply_logical_limit(offset_rows, limit.as_ref())
        }
        LogicalPlan::AggregateSource {
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
            let filtered_source =
                apply_logical_filter_estimate(estimate_plan_rows(source), filter.as_ref());
            let grouped = apply_logical_group_reduction(filtered_source, group_by);
            let having_filtered = apply_logical_filter_estimate(grouped, having.as_ref());
            let deduped = apply_logical_distinct_reduction(having_filtered, *distinct, distinct_on);
            let offset_rows = apply_logical_offset(deduped, offset.as_ref());
            apply_logical_limit(offset_rows, limit.as_ref())
        }
        LogicalPlan::HybridFunctionScan {
            function_name,
            args,
            ..
        } => estimate_hybrid_function_rows(function_name, args),
        LogicalPlan::SetOperation {
            op,
            all,
            left,
            right,
            limit,
            offset,
            ..
        } => {
            let base = estimate_logical_set_operation_rows(
                *op,
                *all,
                estimate_plan_rows(left),
                estimate_plan_rows(right),
            );
            let offset_rows = apply_logical_offset(base, offset.as_ref());
            apply_logical_limit(offset_rows, limit.as_ref())
        }
        LogicalPlan::ProjectValues {
            rows,
            limit,
            offset,
            ..
        } => {
            let base = usize_to_f64(rows.len());
            let offset_rows = apply_logical_offset(base, offset.as_ref());
            apply_logical_limit(offset_rows, limit.as_ref())
        }
        LogicalPlan::RecursiveCte {
            base,
            recursive,
            union_all,
            ..
        } => estimate_logical_recursive_cte_rows(
            estimate_plan_rows(base),
            estimate_plan_rows(recursive),
            *union_all,
        ),
        LogicalPlan::CreateTableAs {
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
        LogicalPlan::InsertValues { rows, .. } => usize_to_f64(rows.len()),
        LogicalPlan::InsertSelect { source, .. } => estimate_plan_rows(source),
        LogicalPlan::DeleteFromTable { filter, .. } | LogicalPlan::UpdateTable { filter, .. } => {
            apply_logical_filter_estimate(1000.0, filter.as_ref())
        }
        _ => 1000.0,
    }
}

fn estimate_logical_recursive_cte_rows(
    base_rows: f64,
    recursive_rows: f64,
    union_all: bool,
) -> f64 {
    let iterations = if union_all { 10.0 } else { 5.0 };
    (base_rows + (recursive_rows * iterations)).max(1.0)
}

fn estimate_logical_set_operation_rows(
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

fn apply_logical_filter_estimate(base: f64, filter: Option<&TypedExpr>) -> f64 {
    if base <= 0.0 {
        return 0.0;
    }
    filter
        .map(|expr| {
            let selectivity = estimate_filter_selectivity(expr);
            if selectivity <= 0.0 {
                0.0
            } else {
                (base * selectivity).max(1.0)
            }
        })
        .unwrap_or(base)
}

fn apply_logical_group_reduction(base: f64, group_by: &[TypedExpr]) -> f64 {
    if base <= 0.0 && !group_by.is_empty() {
        return 0.0;
    }
    if group_by.is_empty() {
        1.0
    } else {
        (base * 0.1).max(1.0)
    }
}

fn apply_logical_distinct_reduction(base: f64, distinct: bool, distinct_on: &[TypedExpr]) -> f64 {
    if base <= 0.0 {
        return 0.0;
    }
    if distinct || !distinct_on.is_empty() {
        (base * 0.5).max(1.0)
    } else {
        base
    }
}

fn apply_logical_offset(base: f64, offset: Option<&TypedExpr>) -> f64 {
    offset
        .and_then(literal_int)
        .map_or(base, |offset| (base - i64_to_f64(offset.max(0))).max(0.0))
}

fn apply_logical_limit(base: f64, limit: Option<&TypedExpr>) -> f64 {
    limit
        .and_then(literal_int)
        .map_or(base, |limit| base.min(i64_to_f64(limit.max(0))))
}

fn literal_int(expr: &TypedExpr) -> Option<i64> {
    match &expr.kind {
        TypedExprKind::Literal(aiondb_core::Value::Int(n)) => Some(i64::from(*n)),
        TypedExprKind::Literal(aiondb_core::Value::BigInt(n)) => Some(*n),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Mutex, OnceLock};

    fn with_env_vars(vars: &[(&'static str, Option<&str>)], test_fn: impl FnOnce()) {
        static ENV_LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        let _guard = ENV_LOCK
            .get_or_init(|| Mutex::new(()))
            .lock()
            .expect("environment lock must not be poisoned");

        let previous: Vec<_> = vars
            .iter()
            .map(|(key, _)| (*key, std::env::var_os(key)))
            .collect();
        for (key, value) in vars {
            match value {
                Some(value) => std::env::set_var(key, value),
                None => std::env::remove_var(key),
            }
        }
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(test_fn));
        for (key, previous_value) in previous {
            match previous_value {
                Some(previous_value) => std::env::set_var(key, previous_value),
                None => std::env::remove_var(key),
            }
        }
        if let Err(panic_payload) = result {
            std::panic::resume_unwind(panic_payload);
        }
    }

    #[test]
    fn join_reorder_is_enabled_by_default() {
        with_env_vars(
            &[
                ("AIONDB_DISABLE_JOIN_REORDER", None),
                ("AIONDB_ENABLE_JOIN_REORDER", None),
            ],
            || {
                assert!(join_reorder_enabled());
            },
        );
    }

    #[test]
    fn join_reorder_disable_flag_wins_over_enable_flag() {
        with_env_vars(
            &[
                ("AIONDB_DISABLE_JOIN_REORDER", Some("1")),
                ("AIONDB_ENABLE_JOIN_REORDER", Some("1")),
            ],
            || {
                assert!(!join_reorder_enabled());
            },
        );
    }

    #[test]
    fn join_reorder_legacy_enable_zero_still_disables() {
        with_env_vars(
            &[
                ("AIONDB_DISABLE_JOIN_REORDER", None),
                ("AIONDB_ENABLE_JOIN_REORDER", Some("0")),
            ],
            || {
                assert!(!join_reorder_enabled());
            },
        );
    }

    #[test]
    fn join_reorder_treats_foreign_key_equality_as_selective() {
        let predicate = TypedExpr::binary_eq(
            TypedExpr::column_ref("p.user_id", 0, aiondb_core::DataType::Int, false),
            TypedExpr::column_ref("u.id", 1, aiondb_core::DataType::Int, false),
        );

        assert_eq!(join_selectivity_for_predicate(&predicate), 0.005);
    }

    #[test]
    fn join_reorder_does_not_anchor_on_tenant_equality() {
        let predicate = TypedExpr::binary_eq(
            TypedExpr::column_ref("target.tenant_id", 0, aiondb_core::DataType::Int, false),
            TypedExpr::column_ref("u.tenant_id", 1, aiondb_core::DataType::Int, false),
        );

        assert_eq!(join_selectivity_for_predicate(&predicate), 1.0);
    }

    #[test]
    fn join_reorder_uses_shared_selectivity_for_non_equi_filters() {
        let predicate = TypedExpr::binary_ne(
            TypedExpr::column_ref("status", 0, aiondb_core::DataType::Text, false),
            TypedExpr::literal(
                aiondb_core::Value::Text("archived".to_owned()),
                aiondb_core::DataType::Text,
                false,
            ),
        );

        assert_eq!(base_selectivity_for_predicate(&predicate), 0.99);
        assert_eq!(join_selectivity_for_predicate(&predicate), 0.99);
    }

    #[test]
    fn join_reorder_estimates_logical_scan_filters_with_shared_selectivity() {
        let plan = LogicalPlan::ProjectTable {
            table_id: aiondb_core::RelationId::new(1),
            outputs: Vec::new(),
            filter: Some(TypedExpr::logical_and(
                TypedExpr::binary_ge(
                    TypedExpr::column_ref("id", 0, aiondb_core::DataType::Int, false),
                    TypedExpr::literal(
                        aiondb_core::Value::Int(100),
                        aiondb_core::DataType::Int,
                        false,
                    ),
                ),
                TypedExpr::binary_le(
                    TypedExpr::column_ref("id", 0, aiondb_core::DataType::Int, false),
                    TypedExpr::literal(
                        aiondb_core::Value::Int(110),
                        aiondb_core::DataType::Int,
                        false,
                    ),
                ),
            )),
            order_by: Vec::new(),
            limit: None,
            offset: None,
            distinct: false,
            distinct_on: Vec::new(),
        };

        assert_eq!(estimate_plan_rows(&plan), 90.0);
    }

    #[test]
    fn join_reorder_estimates_logical_false_filter_as_empty() {
        let plan = LogicalPlan::ProjectTable {
            table_id: aiondb_core::RelationId::new(1),
            outputs: Vec::new(),
            filter: Some(TypedExpr::logical_not(TypedExpr::literal(
                aiondb_core::Value::Boolean(true),
                aiondb_core::DataType::Boolean,
                false,
            ))),
            order_by: Vec::new(),
            limit: None,
            offset: None,
            distinct: false,
            distinct_on: Vec::new(),
        };

        assert_eq!(estimate_plan_rows(&plan), 0.0);
    }

    #[test]
    fn join_reorder_estimates_project_once_as_single_row_source() {
        let plan = LogicalPlan::ProjectOnce {
            outputs: vec![aiondb_plan::ProjectionExpr {
                field: aiondb_plan::ResultField {
                    name: "v".to_owned(),
                    data_type: aiondb_core::DataType::Int,
                    text_type_modifier: None,
                    nullable: false,
                },
                expr: TypedExpr::literal(
                    aiondb_core::Value::Int(1),
                    aiondb_core::DataType::Int,
                    false,
                ),
            }],
            filter: None,
            order_by: Vec::new(),
            limit: None,
            offset: None,
            distinct: false,
            distinct_on: Vec::new(),
        };

        assert_eq!(estimate_plan_rows(&plan), 1.0);
    }

    #[test]
    fn join_reorder_estimates_locking_scan_like_project_table() {
        let plan = LogicalPlan::LockingProjectTable {
            table_id: aiondb_core::RelationId::new(1),
            outputs: Vec::new(),
            filter: Some(TypedExpr::binary_eq(
                TypedExpr::column_ref("id", 0, aiondb_core::DataType::Int, false),
                TypedExpr::literal(
                    aiondb_core::Value::Int(7),
                    aiondb_core::DataType::Int,
                    false,
                ),
            )),
            order_by: Vec::new(),
            limit: Some(TypedExpr::literal(
                aiondb_core::Value::Int(3),
                aiondb_core::DataType::Int,
                false,
            )),
            offset: Some(TypedExpr::literal(
                aiondb_core::Value::Int(1),
                aiondb_core::DataType::Int,
                false,
            )),
            distinct: false,
            distinct_on: Vec::new(),
            row_lock: aiondb_plan::logical::RowLockPlan { skip_locked: false },
        };

        assert_eq!(estimate_plan_rows(&plan), 3.0);
    }

    #[test]
    fn join_reorder_estimates_logical_scan_distinct_offset_and_limit() {
        let plan = LogicalPlan::ProjectTable {
            table_id: aiondb_core::RelationId::new(1),
            outputs: Vec::new(),
            filter: None,
            order_by: Vec::new(),
            limit: Some(TypedExpr::literal(
                aiondb_core::Value::Int(30),
                aiondb_core::DataType::Int,
                false,
            )),
            offset: Some(TypedExpr::literal(
                aiondb_core::Value::Int(20),
                aiondb_core::DataType::Int,
                false,
            )),
            distinct: true,
            distinct_on: Vec::new(),
        };

        assert_eq!(estimate_plan_rows(&plan), 30.0);
    }

    #[test]
    fn join_reorder_estimates_logical_project_source_distinct_offset_and_limit() {
        let source = LogicalPlan::HybridFunctionScan {
            function_name: "vector_top_k_ids".to_owned(),
            args: vec![
                TypedExpr::literal(
                    aiondb_core::Value::Text("docs".to_owned()),
                    aiondb_core::DataType::Text,
                    false,
                ),
                TypedExpr::literal(
                    aiondb_core::Value::Text("embedding".to_owned()),
                    aiondb_core::DataType::Text,
                    false,
                ),
                TypedExpr::literal(
                    aiondb_core::Value::Text("[1.0,0.0]".to_owned()),
                    aiondb_core::DataType::Text,
                    false,
                ),
                TypedExpr::literal(
                    aiondb_core::Value::Int(20),
                    aiondb_core::DataType::Int,
                    false,
                ),
            ],
            output_fields: vec![aiondb_plan::ResultField {
                name: "doc_id".to_owned(),
                data_type: aiondb_core::DataType::BigInt,
                text_type_modifier: None,
                nullable: false,
            }],
        };
        let plan = LogicalPlan::ProjectSource {
            source: Box::new(source),
            outputs: Vec::new(),
            filter: None,
            order_by: Vec::new(),
            limit: Some(TypedExpr::literal(
                aiondb_core::Value::Int(5),
                aiondb_core::DataType::Int,
                false,
            )),
            offset: Some(TypedExpr::literal(
                aiondb_core::Value::Int(4),
                aiondb_core::DataType::Int,
                false,
            )),
            distinct: true,
            distinct_on: Vec::new(),
        };

        assert_eq!(estimate_plan_rows(&plan), 5.0);
    }

    #[test]
    fn join_reorder_estimates_logical_aggregate_shape() {
        let plan = LogicalPlan::Aggregate {
            table_id: aiondb_core::RelationId::new(1),
            group_by: vec![TypedExpr::column_ref(
                "tenant_id",
                0,
                aiondb_core::DataType::Int,
                false,
            )],
            grouping_sets: Vec::new(),
            aggregates: Vec::new(),
            having: Some(TypedExpr::binary_eq(
                TypedExpr::column_ref("tenant_id", 0, aiondb_core::DataType::Int, false),
                TypedExpr::literal(
                    aiondb_core::Value::Int(7),
                    aiondb_core::DataType::Int,
                    false,
                ),
            )),
            filter: Some(TypedExpr::binary_gt(
                TypedExpr::column_ref("id", 1, aiondb_core::DataType::Int, false),
                TypedExpr::literal(
                    aiondb_core::Value::Int(10),
                    aiondb_core::DataType::Int,
                    false,
                ),
            )),
            order_by: Vec::new(),
            limit: Some(TypedExpr::literal(
                aiondb_core::Value::Int(3),
                aiondb_core::DataType::Int,
                false,
            )),
            offset: Some(TypedExpr::literal(
                aiondb_core::Value::Int(1),
                aiondb_core::DataType::Int,
                false,
            )),
            distinct: false,
            distinct_on: Vec::new(),
        };

        assert_eq!(estimate_plan_rows(&plan), 0.0);
    }

    #[test]
    fn join_reorder_estimates_logical_aggregate_source_shape() {
        let source = LogicalPlan::HybridFunctionScan {
            function_name: "vector_top_k_ids".to_owned(),
            args: vec![
                TypedExpr::literal(
                    aiondb_core::Value::Text("docs".to_owned()),
                    aiondb_core::DataType::Text,
                    false,
                ),
                TypedExpr::literal(
                    aiondb_core::Value::Text("embedding".to_owned()),
                    aiondb_core::DataType::Text,
                    false,
                ),
                TypedExpr::literal(
                    aiondb_core::Value::Text("[1.0,0.0]".to_owned()),
                    aiondb_core::DataType::Text,
                    false,
                ),
                TypedExpr::literal(
                    aiondb_core::Value::Int(20),
                    aiondb_core::DataType::Int,
                    false,
                ),
            ],
            output_fields: vec![aiondb_plan::ResultField {
                name: "doc_id".to_owned(),
                data_type: aiondb_core::DataType::BigInt,
                text_type_modifier: None,
                nullable: false,
            }],
        };
        let plan = LogicalPlan::AggregateSource {
            source: Box::new(source),
            group_by: vec![TypedExpr::column_ref(
                "doc_id",
                0,
                aiondb_core::DataType::BigInt,
                false,
            )],
            grouping_sets: Vec::new(),
            aggregates: Vec::new(),
            having: None,
            filter: Some(TypedExpr::binary_eq(
                TypedExpr::column_ref("doc_id", 0, aiondb_core::DataType::BigInt, false),
                TypedExpr::literal(
                    aiondb_core::Value::BigInt(7),
                    aiondb_core::DataType::BigInt,
                    false,
                ),
            )),
            order_by: Vec::new(),
            limit: None,
            offset: None,
            distinct: false,
            distinct_on: Vec::new(),
        };

        assert_eq!(estimate_plan_rows(&plan), 1.0);
    }

    #[test]
    fn join_reorder_estimates_logical_project_values_with_offset_and_limit() {
        let plan = LogicalPlan::ProjectValues {
            output_fields: vec![aiondb_plan::ResultField {
                name: "v".to_owned(),
                data_type: aiondb_core::DataType::Int,
                text_type_modifier: None,
                nullable: false,
            }],
            rows: vec![Vec::new(); 12],
            order_by: Vec::new(),
            limit: Some(TypedExpr::literal(
                aiondb_core::Value::Int(5),
                aiondb_core::DataType::Int,
                false,
            )),
            offset: Some(TypedExpr::literal(
                aiondb_core::Value::Int(4),
                aiondb_core::DataType::Int,
                false,
            )),
        };

        assert_eq!(estimate_plan_rows(&plan), 5.0);
    }

    #[test]
    fn join_reorder_estimates_logical_project_values_offset_exhaustion() {
        let plan = LogicalPlan::ProjectValues {
            output_fields: vec![aiondb_plan::ResultField {
                name: "v".to_owned(),
                data_type: aiondb_core::DataType::Int,
                text_type_modifier: None,
                nullable: false,
            }],
            rows: vec![Vec::new(); 3],
            order_by: Vec::new(),
            limit: None,
            offset: Some(TypedExpr::literal(
                aiondb_core::Value::Int(5),
                aiondb_core::DataType::Int,
                false,
            )),
        };

        assert_eq!(estimate_plan_rows(&plan), 0.0);
    }

    #[test]
    fn join_reorder_estimates_logical_project_values_limit_zero() {
        let plan = LogicalPlan::ProjectValues {
            output_fields: vec![aiondb_plan::ResultField {
                name: "v".to_owned(),
                data_type: aiondb_core::DataType::Int,
                text_type_modifier: None,
                nullable: false,
            }],
            rows: vec![Vec::new(); 3],
            order_by: Vec::new(),
            limit: Some(TypedExpr::literal(
                aiondb_core::Value::Int(0),
                aiondb_core::DataType::Int,
                false,
            )),
            offset: None,
        };

        assert_eq!(estimate_plan_rows(&plan), 0.0);
    }

    #[test]
    fn join_reorder_estimates_logical_project_values_empty_rows_as_empty() {
        let plan = LogicalPlan::ProjectValues {
            output_fields: vec![aiondb_plan::ResultField {
                name: "v".to_owned(),
                data_type: aiondb_core::DataType::Int,
                text_type_modifier: None,
                nullable: false,
            }],
            rows: Vec::new(),
            order_by: Vec::new(),
            limit: None,
            offset: None,
        };

        assert_eq!(estimate_plan_rows(&plan), 0.0);
    }

    #[test]
    fn join_reorder_estimates_logical_aggregate_group_by_empty_source_as_empty() {
        let plan = LogicalPlan::AggregateSource {
            source: Box::new(LogicalPlan::ProjectValues {
                output_fields: vec![aiondb_plan::ResultField {
                    name: "tenant_id".to_owned(),
                    data_type: aiondb_core::DataType::Int,
                    text_type_modifier: None,
                    nullable: false,
                }],
                rows: Vec::new(),
                order_by: Vec::new(),
                limit: None,
                offset: None,
            }),
            group_by: vec![TypedExpr::column_ref(
                "tenant_id",
                0,
                aiondb_core::DataType::Int,
                false,
            )],
            grouping_sets: Vec::new(),
            aggregates: Vec::new(),
            having: None,
            filter: None,
            order_by: Vec::new(),
            limit: None,
            offset: None,
            distinct: false,
            distinct_on: Vec::new(),
        };

        assert_eq!(estimate_plan_rows(&plan), 0.0);
    }

    #[test]
    fn join_reorder_estimates_logical_set_operation_union_all_shape() {
        let output_fields = vec![aiondb_plan::ResultField {
            name: "v".to_owned(),
            data_type: aiondb_core::DataType::Int,
            text_type_modifier: None,
            nullable: false,
        }];
        let values_plan = |rows: usize| LogicalPlan::ProjectValues {
            output_fields: output_fields.clone(),
            rows: vec![Vec::new(); rows],
            order_by: Vec::new(),
            limit: None,
            offset: None,
        };
        let plan = LogicalPlan::SetOperation {
            op: SetOperationType::Union,
            all: true,
            left: Box::new(values_plan(7)),
            right: Box::new(values_plan(11)),
            output_fields: output_fields.clone(),
            order_by: Vec::new(),
            limit: Some(TypedExpr::literal(
                aiondb_core::Value::Int(15),
                aiondb_core::DataType::Int,
                false,
            )),
            offset: Some(TypedExpr::literal(
                aiondb_core::Value::Int(2),
                aiondb_core::DataType::Int,
                false,
            )),
        };

        assert_eq!(estimate_plan_rows(&plan), 15.0);
    }

    #[test]
    fn join_reorder_estimates_logical_set_operation_empty_inputs_as_empty() {
        let output_fields = vec![aiondb_plan::ResultField {
            name: "v".to_owned(),
            data_type: aiondb_core::DataType::Int,
            text_type_modifier: None,
            nullable: false,
        }];
        let empty_values = || LogicalPlan::ProjectValues {
            output_fields: output_fields.clone(),
            rows: Vec::new(),
            order_by: Vec::new(),
            limit: None,
            offset: None,
        };
        let plan = LogicalPlan::SetOperation {
            op: SetOperationType::Union,
            all: false,
            left: Box::new(empty_values()),
            right: Box::new(empty_values()),
            output_fields,
            order_by: Vec::new(),
            limit: None,
            offset: None,
        };

        assert_eq!(estimate_plan_rows(&plan), 0.0);
    }

    #[test]
    fn join_reorder_estimates_logical_recursive_cte_shape() {
        let output_fields = vec![aiondb_plan::ResultField {
            name: "v".to_owned(),
            data_type: aiondb_core::DataType::Int,
            text_type_modifier: None,
            nullable: false,
        }];
        let values_plan = |rows: usize| LogicalPlan::ProjectValues {
            output_fields: output_fields.clone(),
            rows: vec![Vec::new(); rows],
            order_by: Vec::new(),
            limit: None,
            offset: None,
        };
        let plan = LogicalPlan::RecursiveCte {
            base: Box::new(values_plan(2)),
            recursive: Box::new(values_plan(3)),
            union_all: true,
            output_fields,
        };

        assert_eq!(estimate_plan_rows(&plan), 32.0);
    }

    #[test]
    fn join_reorder_estimates_logical_insert_values_row_count() {
        let plan = LogicalPlan::InsertValues {
            table_id: aiondb_core::RelationId::new(7),
            columns: Vec::new(),
            rows: vec![Vec::new(); 4],
            on_conflict: None,
            returning: Vec::new(),
        };

        assert_eq!(estimate_plan_rows(&plan), 4.0);
    }

    #[test]
    fn join_reorder_estimates_logical_insert_select_from_source() {
        let output_fields = vec![aiondb_plan::ResultField {
            name: "v".to_owned(),
            data_type: aiondb_core::DataType::Int,
            text_type_modifier: None,
            nullable: false,
        }];
        let plan = LogicalPlan::InsertSelect {
            table_id: aiondb_core::RelationId::new(7),
            columns: Vec::new(),
            assignments: Vec::new(),
            source: Box::new(LogicalPlan::ProjectValues {
                output_fields,
                rows: vec![Vec::new(); 9],
                order_by: Vec::new(),
                limit: Some(TypedExpr::literal(
                    aiondb_core::Value::Int(6),
                    aiondb_core::DataType::Int,
                    false,
                )),
                offset: None,
            }),
            on_conflict: None,
            returning: Vec::new(),
        };

        assert_eq!(estimate_plan_rows(&plan), 6.0);
    }

    #[test]
    fn join_reorder_estimates_logical_create_table_as_with_no_data_as_empty() {
        let plan = LogicalPlan::CreateTableAs {
            relation_name: "tmp".to_owned(),
            columns: Vec::new(),
            with_no_data: true,
            source: Box::new(LogicalPlan::ProjectValues {
                output_fields: Vec::new(),
                rows: vec![Vec::new(); 9],
                order_by: Vec::new(),
                limit: None,
                offset: None,
            }),
        };

        assert_eq!(estimate_plan_rows(&plan), 0.0);
    }

    #[test]
    fn join_reorder_uses_shared_hybrid_top_k_estimate() {
        let plan = LogicalPlan::HybridFunctionScan {
            function_name: "hybrid_search_top_k_hits".to_owned(),
            args: vec![
                TypedExpr::literal(
                    aiondb_core::Value::Text("docs".to_owned()),
                    aiondb_core::DataType::Text,
                    false,
                ),
                TypedExpr::literal(
                    aiondb_core::Value::Text("embedding".to_owned()),
                    aiondb_core::DataType::Text,
                    false,
                ),
                TypedExpr::literal(
                    aiondb_core::Value::Text("body".to_owned()),
                    aiondb_core::DataType::Text,
                    false,
                ),
                TypedExpr::literal(
                    aiondb_core::Value::Text("[1.0,0.0]".to_owned()),
                    aiondb_core::DataType::Text,
                    false,
                ),
                TypedExpr::literal(
                    aiondb_core::Value::Text("query".to_owned()),
                    aiondb_core::DataType::Text,
                    false,
                ),
                TypedExpr::literal(
                    aiondb_core::Value::Int(20),
                    aiondb_core::DataType::Int,
                    false,
                ),
                TypedExpr::literal(
                    aiondb_core::Value::Jsonb(serde_json::json!({
                        "offset": 4,
                        "filter": {"must": [{"key": "kind", "match": "doc"}]},
                        "text_score_threshold": 0.2
                    })),
                    aiondb_core::DataType::Jsonb,
                    false,
                ),
            ],
            output_fields: vec![aiondb_plan::ResultField {
                name: "hit".to_owned(),
                data_type: aiondb_core::DataType::Jsonb,
                text_type_modifier: None,
                nullable: false,
            }],
        };

        assert_eq!(estimate_plan_rows(&plan), 5.0);
    }

    #[test]
    fn join_reorder_uses_graph_neighbors_limit_estimate() {
        let plan = LogicalPlan::HybridFunctionScan {
            function_name: "graph_neighbors".to_owned(),
            args: vec![
                TypedExpr::literal(
                    aiondb_core::Value::Text("related_doc".to_owned()),
                    aiondb_core::DataType::Text,
                    false,
                ),
                TypedExpr::literal(
                    aiondb_core::Value::BigInt(42),
                    aiondb_core::DataType::BigInt,
                    false,
                ),
                TypedExpr::literal(
                    aiondb_core::Value::Text("outgoing".to_owned()),
                    aiondb_core::DataType::Text,
                    false,
                ),
                TypedExpr::literal(
                    aiondb_core::Value::Int(2),
                    aiondb_core::DataType::Int,
                    false,
                ),
            ],
            output_fields: vec![aiondb_plan::ResultField {
                name: "doc_id".to_owned(),
                data_type: aiondb_core::DataType::BigInt,
                text_type_modifier: None,
                nullable: false,
            }],
        };

        assert_eq!(estimate_plan_rows(&plan), 2.0);
    }

    #[test]
    fn join_reorder_uses_graph_neighbors_options_limit_estimate() {
        let plan = LogicalPlan::HybridFunctionScan {
            function_name: "graph_neighbors".to_owned(),
            args: vec![
                TypedExpr::literal(
                    aiondb_core::Value::Text("related_doc".to_owned()),
                    aiondb_core::DataType::Text,
                    false,
                ),
                TypedExpr::literal(
                    aiondb_core::Value::BigInt(42),
                    aiondb_core::DataType::BigInt,
                    false,
                ),
                TypedExpr::literal(
                    aiondb_core::Value::Text("outgoing".to_owned()),
                    aiondb_core::DataType::Text,
                    false,
                ),
                TypedExpr::literal(
                    aiondb_core::Value::Text("mentions".to_owned()),
                    aiondb_core::DataType::Text,
                    false,
                ),
                TypedExpr::literal(
                    aiondb_core::Value::Jsonb(serde_json::json!({"limit": 3})),
                    aiondb_core::DataType::Jsonb,
                    false,
                ),
            ],
            output_fields: vec![aiondb_plan::ResultField {
                name: "doc_id".to_owned(),
                data_type: aiondb_core::DataType::BigInt,
                text_type_modifier: None,
                nullable: false,
            }],
        };

        assert_eq!(estimate_plan_rows(&plan), 3.0);
    }
}
