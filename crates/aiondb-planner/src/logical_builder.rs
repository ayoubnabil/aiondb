use aiondb_catalog::CatalogReader;
use aiondb_core::{DataType, DbError, DbResult, TxnId, Value};
use aiondb_plan::{
    logical::RowLockPlan, InsertOnConflict, JoinType, LogicalPlan, OnConflictActionPlan,
    ProjectionExpr, ResultField, ScalarFunction, TypedExpr, TypedExprKind,
};

use aiondb_parser::{CopyDirection, Expr, Literal, ObjectName};

use crate::type_check::{
    TypedAlterRole, TypedAlterTable, TypedAnalyze, TypedCopy, TypedCreateEdgeLabel,
    TypedCreateIndex, TypedCreateNodeLabel, TypedCreateRole, TypedCreateSchema,
    TypedCreateSequence, TypedCreateTable, TypedCreateTableAs, TypedCreateView, TypedDelete,
    TypedDropEdgeLabel, TypedDropIndex, TypedDropNodeLabel, TypedDropRole, TypedDropSchema,
    TypedDropSequence, TypedDropTable, TypedDropView, TypedGrant, TypedInsert, TypedOnConflict,
    TypedOnConflictAction, TypedRevoke, TypedSelect, TypedSetBranch, TypedSetOperation,
    TypedTruncateTable, TypedUpdate, TypedVacuum,
};

fn expr_contains_aggregate(expr: &TypedExpr) -> bool {
    let mut stack = vec![expr];
    while let Some(expr) = stack.pop() {
        if matches!(
            expr.kind,
            TypedExprKind::AggCount { .. }
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
        ) {
            return true;
        }
        push_typed_expr_children(expr, &mut stack);
    }
    false
}

fn push_typed_expr_children<'a>(expr: &'a TypedExpr, stack: &mut Vec<&'a TypedExpr>) {
    match &expr.kind {
        TypedExprKind::BinaryEq { left, right }
        | TypedExprKind::BinaryNe { left, right }
        | TypedExprKind::BinaryGt { left, right }
        | TypedExprKind::BinaryGe { left, right }
        | TypedExprKind::BinaryLt { left, right }
        | TypedExprKind::BinaryLe { left, right }
        | TypedExprKind::LogicalAnd { left, right }
        | TypedExprKind::LogicalOr { left, right }
        | TypedExprKind::ArithAdd { left, right }
        | TypedExprKind::ArithSub { left, right }
        | TypedExprKind::ArithMul { left, right }
        | TypedExprKind::ArithDiv { left, right }
        | TypedExprKind::ArithMod { left, right }
        | TypedExprKind::Concat { left, right }
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
        | TypedExprKind::ArrayOverlap { left, right }
        | TypedExprKind::Nullif { left, right }
        | TypedExprKind::IsDistinctFrom { left, right, .. } => {
            stack.push(right);
            stack.push(left);
        }
        TypedExprKind::LogicalNot { expr }
        | TypedExprKind::Negate { expr }
        | TypedExprKind::IsNull { expr, .. }
        | TypedExprKind::Cast { expr, .. }
        | TypedExprKind::InSubquery { expr, .. } => stack.push(expr),
        TypedExprKind::Like { expr, pattern, .. } => {
            stack.push(pattern);
            stack.push(expr);
        }
        TypedExprKind::InList { expr, list, .. } => {
            stack.extend(list);
            stack.push(expr);
        }
        TypedExprKind::Between {
            expr, low, high, ..
        } => {
            stack.push(high);
            stack.push(low);
            stack.push(expr);
        }
        TypedExprKind::CaseWhen {
            conditions,
            results,
            else_result,
        } => {
            if let Some(expr) = else_result {
                stack.push(expr);
            }
            stack.extend(results);
            stack.extend(conditions);
        }
        TypedExprKind::Coalesce { args }
        | TypedExprKind::ScalarFunction { args, .. }
        | TypedExprKind::ArrayConstruct { elements: args }
        | TypedExprKind::UserFunction { args, .. } => stack.extend(args),
        TypedExprKind::WindowFunction {
            args,
            partition_by,
            order_by,
            ..
        } => {
            for sort in order_by {
                stack.push(&sort.expr);
            }
            stack.extend(partition_by);
            stack.extend(args);
        }
        TypedExprKind::AggCount { expr, filter, .. } => {
            if let Some(expr) = expr {
                stack.push(expr);
            }
            if let Some(filter) = filter {
                stack.push(filter);
            }
        }
        TypedExprKind::AggSum { expr, filter, .. }
        | TypedExprKind::AggAvg { expr, filter, .. }
        | TypedExprKind::AggAnyValue { expr, filter }
        | TypedExprKind::AggMin { expr, filter }
        | TypedExprKind::AggMax { expr, filter }
        | TypedExprKind::AggArrayAgg { expr, filter, .. }
        | TypedExprKind::AggBoolAnd { expr, filter }
        | TypedExprKind::AggBoolOr { expr, filter }
        | TypedExprKind::AggStddevPop { expr, filter }
        | TypedExprKind::AggStddevSamp { expr, filter }
        | TypedExprKind::AggVarPop { expr, filter }
        | TypedExprKind::AggVarSamp { expr, filter } => {
            stack.push(expr);
            if let Some(filter) = filter {
                stack.push(filter);
            }
        }
        TypedExprKind::AggStringAgg {
            expr,
            delimiter,
            filter,
            ..
        } => {
            stack.push(delimiter);
            stack.push(expr);
            if let Some(filter) = filter {
                stack.push(filter);
            }
        }
        TypedExprKind::Literal(_)
        | TypedExprKind::ColumnRef { .. }
        | TypedExprKind::OuterColumnRef { .. }
        | TypedExprKind::NextValue { .. }
        | TypedExprKind::ScalarSubquery { .. }
        | TypedExprKind::ArraySubquery { .. }
        | TypedExprKind::ExistsSubquery { .. } => {}
    }
}

fn normalize_distinct_on_to_projection_outputs(
    outputs: &[ProjectionExpr],
    distinct_on: Vec<TypedExpr>,
) -> Vec<TypedExpr> {
    distinct_on
        .into_iter()
        .map(|expr| rebind_distinct_on_to_projection_output(outputs, &expr).unwrap_or(expr))
        .collect()
}

fn rebind_distinct_on_to_projection_output(
    outputs: &[ProjectionExpr],
    expr: &TypedExpr,
) -> Option<TypedExpr> {
    if let Some((ordinal, output)) = outputs
        .iter()
        .enumerate()
        .find(|(_, output)| output.expr == *expr)
    {
        return Some(TypedExpr::column_ref(
            &output.field.name,
            ordinal,
            output.field.data_type.clone(),
            output.field.nullable,
        ));
    }

    let (TypedExprKind::ColumnRef {
        name: column_name, ..
    }
    | TypedExprKind::OuterColumnRef {
        name: column_name, ..
    }) = &expr.kind
    else {
        return None;
    };

    let mut matches = outputs
        .iter()
        .enumerate()
        .filter(|(_, output)| projection_matches_distinct_on_name(output, column_name));
    let (ordinal, output) = matches.next()?;
    if matches.next().is_some() {
        return None;
    }

    Some(TypedExpr::column_ref(
        &output.field.name,
        ordinal,
        output.field.data_type.clone(),
        output.field.nullable,
    ))
}

fn projection_matches_distinct_on_name(output: &ProjectionExpr, column_name: &str) -> bool {
    output.field.name.eq_ignore_ascii_case(column_name)
        || matches!(
            &output.expr.kind,
            TypedExprKind::ColumnRef { name, .. } | TypedExprKind::OuterColumnRef { name, .. }
                if name.eq_ignore_ascii_case(column_name)
        )
}

fn expr_references_row(expr: &TypedExpr) -> bool {
    let mut stack = vec![expr];
    while let Some(expr) = stack.pop() {
        match &expr.kind {
            TypedExprKind::ColumnRef { .. }
            | TypedExprKind::OuterColumnRef { .. }
            | TypedExprKind::NextValue { .. } => return true,
            TypedExprKind::AggCount { .. }
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
            | TypedExprKind::ExistsSubquery { .. } => {}
            _ => push_typed_expr_children(expr, &mut stack),
        }
    }
    false
}

struct ExistsSemiJoinCandidate {
    right_table_id: aiondb_core::RelationId,
    right_filter: Option<TypedExpr>,
    join_type: JoinType,
    condition: TypedExpr,
    remaining_filter: Option<TypedExpr>,
}

fn try_extract_correlated_exists_semi_join(
    filter: &TypedExpr,
    left_width: usize,
) -> Option<ExistsSemiJoinCandidate> {
    if left_width == 0 {
        return None;
    }

    let conjuncts = split_logical_and(filter);
    for (idx, conjunct) in conjuncts.iter().enumerate() {
        let TypedExprKind::ExistsSubquery { plan, negated } = &conjunct.kind else {
            continue;
        };
        let Some((right_table_id, inner_filter)) = simple_exists_subquery_source(plan) else {
            continue;
        };
        let inner_filter = inner_filter?;
        if !expr_contains_outer_ref(inner_filter) {
            continue;
        }
        let inner_conjuncts = split_logical_and(inner_filter);
        let correlated_filter = rebuild_conjunction(
            inner_conjuncts
                .iter()
                .filter(|expr| expr_contains_outer_ref(expr))
                .map(|expr| (*expr).clone())
                .collect(),
        )?;
        let right_filter = rebuild_conjunction(
            inner_conjuncts
                .iter()
                .filter(|expr| !expr_contains_outer_ref(expr))
                .map(|expr| (*expr).clone())
                .collect(),
        );
        let condition = rewrite_subquery_expr_for_join(&correlated_filter, left_width)?;
        let remaining_filter = rebuild_conjunction(
            conjuncts
                .iter()
                .enumerate()
                .filter(|(other_idx, _)| *other_idx != idx)
                .map(|(_, expr)| (*expr).clone())
                .collect(),
        );
        return Some(ExistsSemiJoinCandidate {
            right_table_id,
            right_filter,
            join_type: if *negated {
                JoinType::Anti
            } else {
                JoinType::Semi
            },
            condition,
            remaining_filter,
        });
    }

    None
}

fn simple_exists_subquery_source(
    plan: &LogicalPlan,
) -> Option<(aiondb_core::RelationId, Option<&TypedExpr>)> {
    match plan {
        LogicalPlan::ProjectTable {
            table_id,
            filter,
            order_by,
            limit,
            offset,
            distinct,
            distinct_on,
            ..
        } if order_by.is_empty()
            && limit.is_none()
            && offset.is_none()
            && !*distinct
            && distinct_on.is_empty() =>
        {
            Some((*table_id, filter.as_ref()))
        }
        _ => None,
    }
}

fn split_logical_and(expr: &TypedExpr) -> Vec<&TypedExpr> {
    match &expr.kind {
        TypedExprKind::LogicalAnd { left, right } => {
            let mut out = split_logical_and(left);
            out.extend(split_logical_and(right));
            out
        }
        _ => vec![expr],
    }
}

fn rebuild_conjunction(mut conjuncts: Vec<TypedExpr>) -> Option<TypedExpr> {
    let first = conjuncts.pop()?;
    Some(
        conjuncts
            .into_iter()
            .rev()
            .fold(first, |acc, expr| TypedExpr::logical_and(expr, acc)),
    )
}

fn expr_contains_outer_ref(expr: &TypedExpr) -> bool {
    let mut stack = vec![expr];
    while let Some(expr) = stack.pop() {
        match &expr.kind {
            TypedExprKind::OuterColumnRef { .. } => return true,
            TypedExprKind::BinaryEq { left, right }
            | TypedExprKind::IsDistinctFrom { left, right, .. }
            | TypedExprKind::LogicalAnd { left, right } => {
                stack.push(right);
                stack.push(left);
            }
            TypedExprKind::IsNull { expr, .. } | TypedExprKind::Cast { expr, .. } => {
                stack.push(expr);
            }
            TypedExprKind::ColumnRef { .. } | TypedExprKind::Literal(_) => {}
            _ => return true,
        }
    }
    false
}

fn rewrite_subquery_expr_for_join(expr: &TypedExpr, left_width: usize) -> Option<TypedExpr> {
    let rewritten = match &expr.kind {
        TypedExprKind::OuterColumnRef { name, ordinal } => TypedExpr::column_ref(
            name.clone(),
            normalize_join_input_ordinal(*ordinal, left_width),
            expr.data_type.clone(),
            expr.nullable,
        ),
        TypedExprKind::ColumnRef { name, ordinal } => TypedExpr::column_ref(
            name.clone(),
            normalize_join_input_ordinal(*ordinal, left_width).checked_add(left_width)?,
            expr.data_type.clone(),
            expr.nullable,
        ),
        TypedExprKind::Literal(value) => {
            TypedExpr::literal(value.clone(), expr.data_type.clone(), expr.nullable)
        }
        TypedExprKind::BinaryEq { left, right } => TypedExpr::binary_eq(
            rewrite_subquery_expr_for_join(left, left_width)?,
            rewrite_subquery_expr_for_join(right, left_width)?,
        ),
        TypedExprKind::LogicalAnd { left, right } => TypedExpr::logical_and(
            rewrite_subquery_expr_for_join(left, left_width)?,
            rewrite_subquery_expr_for_join(right, left_width)?,
        ),
        TypedExprKind::IsDistinctFrom {
            left,
            right,
            negated,
        } if *negated => TypedExpr::binary_eq(
            rewrite_subquery_expr_for_join(left, left_width)?,
            rewrite_subquery_expr_for_join(right, left_width)?,
        ),
        TypedExprKind::IsDistinctFrom { .. } => return None,
        TypedExprKind::IsNull {
            expr: inner,
            negated,
        } => TypedExpr {
            kind: TypedExprKind::IsNull {
                expr: Box::new(rewrite_subquery_expr_for_join(inner, left_width)?),
                negated: *negated,
            },
            data_type: expr.data_type.clone(),
            nullable: expr.nullable,
        },
        TypedExprKind::Cast {
            expr: inner,
            target_type,
        } => TypedExpr::cast(
            rewrite_subquery_expr_for_join(inner, left_width)?,
            target_type.clone(),
        ),
        _ => return None,
    };
    Some(rewritten)
}

fn normalize_join_input_ordinal(ordinal: usize, input_width: usize) -> usize {
    if input_width == 0 {
        ordinal
    } else {
        ordinal % input_width
    }
}

pub struct LogicalBuilder;

impl LogicalBuilder {
    pub fn build_select(&self, select: TypedSelect) -> LogicalPlan {
        // If the select has joins, build a left-deep join tree.
        // Start with the base table as a SeqScan, then fold each join on top:
        //   SeqScan(a) -> Join(SeqScan(a), SeqScan(b)) -> Join(Join(...), SeqScan(c))
        // The logical node stays `NestedLoopJoin` so typed-expression ordinals
        // remain stable even if execution later chooses a faster join path.
        // The final (outermost) join carries the outputs, filter, order_by,
        // limit, offset, and distinct from the SELECT. Inner joins are
        // "passthrough" nodes with no projection of their own.
        let has_aggregates = !select.group_by.is_empty()
            || select.having.is_some()
            || select
                .outputs
                .iter()
                .any(|o| expr_contains_aggregate(&o.expr))
            || select
                .order_by
                .iter()
                .any(|item| expr_contains_aggregate(&item.expr));

        let TypedSelect {
            row_lock,
            mut outputs,
            table_id,
            input_width,
            source,
            joins,
            mut filter,
            group_by,
            grouping_sets,
            having,
            mut order_by,
            mut limit,
            mut offset,
            distinct,
            mut distinct_on,
            param_types: _,
        } = select;

        if let Some(plan) = try_build_hybrid_function_scan(
            &outputs,
            table_id,
            source.as_deref(),
            &joins,
            filter.as_ref(),
            &group_by,
            &grouping_sets,
            having.as_ref(),
            &order_by,
            limit.as_ref(),
            offset.as_ref(),
            distinct,
            &distinct_on,
        ) {
            return plan;
        }

        distinct_on = normalize_distinct_on_to_projection_outputs(&outputs, distinct_on);

        let has_degenerate_having = having.is_some()
            && joins.is_empty()
            && group_by.is_empty()
            && outputs
                .iter()
                .all(|output| !expr_contains_aggregate(&output.expr))
            && outputs
                .iter()
                .all(|output| !expr_references_row(&output.expr))
            && having
                .as_ref()
                .is_some_and(|expr| !expr_contains_aggregate(expr) && !expr_references_row(expr))
            && order_by.iter().all(|sort| {
                !expr_contains_aggregate(&sort.expr) && !expr_references_row(&sort.expr)
            });

        if has_degenerate_having {
            return LogicalPlan::ProjectOnce {
                outputs,
                filter: having,
                order_by,
                limit,
                offset,
                distinct,
                distinct_on,
            };
        }

        if !has_aggregates && joins.is_empty() && source.is_none() && row_lock.is_none() {
            if let (Some(left_table_id), Some(filter_ref)) = (table_id, filter.as_ref()) {
                if let Some(exists_join) =
                    try_extract_correlated_exists_semi_join(filter_ref, input_width)
                {
                    return LogicalPlan::NestedLoopJoin {
                        left: Box::new(LogicalPlan::SeqScan {
                            table_id: left_table_id,
                        }),
                        right: Box::new(LogicalPlan::ProjectTable {
                            table_id: exists_join.right_table_id,
                            outputs: Vec::new(),
                            filter: exists_join.right_filter,
                            order_by: Vec::new(),
                            limit: None,
                            offset: None,
                            distinct: false,
                            distinct_on: Vec::new(),
                        }),
                        join_type: exists_join.join_type,
                        condition: Some(exists_join.condition),
                        outputs,
                        filter: exists_join.remaining_filter,
                        order_by,
                        limit,
                        offset,
                        distinct,
                        distinct_on,
                    };
                }
            }
        }

        if !joins.is_empty() {
            let mut current = if let Some(source) = source {
                self.build_set_branch(*source)
            } else if let Some(left_table_id) = table_id {
                LogicalPlan::SeqScan {
                    table_id: left_table_id,
                }
            } else {
                LogicalPlan::ProjectOnce {
                    outputs: Vec::new(),
                    filter: None,
                    order_by: Vec::new(),
                    limit: None,
                    offset: None,
                    distinct: false,
                    distinct_on: Vec::new(),
                }
            };

            let last_idx = joins.len() - 1;
            let mut join_outputs = Some(if has_aggregates {
                Vec::new()
            } else {
                std::mem::take(&mut outputs)
            });
            // Query-level WHERE clauses must be applied before aggregation.
            // Keep the filter on the outermost join even for aggregate queries
            // so the source rowset is reduced before AggregateSource consumes it.
            let mut join_filter = Some(filter.take());
            let mut join_order_by = Some(if has_aggregates {
                Vec::new()
            } else {
                std::mem::take(&mut order_by)
            });
            let mut join_limit = Some(if has_aggregates { None } else { limit.take() });
            let mut join_offset = Some(if has_aggregates { None } else { offset.take() });
            let mut join_distinct_on = Some(if has_aggregates {
                Vec::new()
            } else {
                std::mem::take(&mut distinct_on)
            });

            for (i, join) in joins.into_iter().enumerate() {
                let join_has_derived_source = join.source.is_some();
                let right = if let Some(source) = join.source {
                    self.build_set_branch(*source)
                } else if let Some(table_id) = join.table_id {
                    LogicalPlan::SeqScan { table_id }
                } else {
                    LogicalPlan::ProjectOnce {
                        outputs: Vec::new(),
                        filter: None,
                        order_by: Vec::new(),
                        limit: None,
                        offset: None,
                        distinct: false,
                        distinct_on: Vec::new(),
                    }
                };
                let is_last = i == last_idx;
                let hoist_derived_inner_join_condition = join_has_derived_source
                    && join.join_type == JoinType::Inner
                    && join.condition.is_some();
                let mut join_condition = join.condition;
                let mut local_filter = if is_last {
                    join_filter.take().unwrap_or(None)
                } else {
                    None
                };
                if hoist_derived_inner_join_condition {
                    if let Some(condition) = join_condition.take() {
                        local_filter = Some(match local_filter {
                            Some(existing) => TypedExpr::logical_and(condition, existing),
                            None => condition,
                        });
                    }
                }
                current = LogicalPlan::NestedLoopJoin {
                    left: Box::new(current),
                    right: Box::new(right),
                    join_type: join.join_type,
                    condition: join_condition,
                    // Only the outermost join carries the query-level properties.
                    outputs: if is_last {
                        join_outputs.take().unwrap_or_default()
                    } else {
                        Vec::new()
                    },
                    filter: local_filter,
                    order_by: if is_last {
                        join_order_by.take().unwrap_or_default()
                    } else {
                        Vec::new()
                    },
                    limit: if is_last {
                        join_limit.take().unwrap_or(None)
                    } else {
                        None
                    },
                    offset: if is_last {
                        join_offset.take().unwrap_or(None)
                    } else {
                        None
                    },
                    distinct: is_last && !has_aggregates && distinct,
                    distinct_on: if is_last {
                        join_distinct_on.take().unwrap_or_default()
                    } else {
                        Vec::new()
                    },
                };
            }
            if has_aggregates {
                return LogicalPlan::AggregateSource {
                    source: Box::new(current),
                    group_by,
                    grouping_sets,
                    aggregates: outputs,
                    having,
                    filter,
                    order_by,
                    limit,
                    offset,
                    distinct,
                    distinct_on,
                };
            }
            return current;
        }

        if has_aggregates {
            if let Some(source) = source {
                return LogicalPlan::AggregateSource {
                    source: Box::new(self.build_set_branch(*source)),
                    group_by,
                    grouping_sets,
                    aggregates: outputs,
                    having,
                    filter,
                    order_by,
                    limit,
                    offset,
                    distinct,
                    distinct_on,
                };
            }
            if let Some(table_id) = table_id {
                return LogicalPlan::Aggregate {
                    table_id,
                    group_by,
                    grouping_sets,
                    aggregates: outputs,
                    having,
                    filter,
                    order_by,
                    limit,
                    offset,
                    distinct,
                    distinct_on,
                };
            }
        }

        match (table_id, source) {
            (_, Some(source)) => LogicalPlan::ProjectSource {
                source: Box::new(self.build_set_branch(*source)),
                outputs,
                filter,
                order_by,
                limit,
                offset,
                distinct,
                distinct_on,
            },
            (Some(table_id), None) => {
                if let Some(row_lock) = row_lock {
                    LogicalPlan::LockingProjectTable {
                        table_id,
                        outputs,
                        filter,
                        order_by,
                        limit,
                        offset,
                        distinct,
                        distinct_on,
                        row_lock: RowLockPlan {
                            skip_locked: row_lock.skip_locked,
                        },
                    }
                } else {
                    LogicalPlan::ProjectTable {
                        table_id,
                        outputs,
                        filter,
                        order_by,
                        limit,
                        offset,
                        distinct,
                        distinct_on,
                    }
                }
            }
            (None, None) => LogicalPlan::ProjectOnce {
                outputs,
                filter,
                order_by,
                limit,
                offset,
                distinct,
                distinct_on,
            },
        }
    }

    pub fn build_copy(&self, copy: TypedCopy) -> LogicalPlan {
        match copy.direction {
            CopyDirection::From => LogicalPlan::CopyFrom {
                table_id: copy.table_id,
                columns: copy.columns,
            },
            CopyDirection::To => LogicalPlan::CopyTo {
                table_id: copy.table_id,
                columns: copy.columns,
            },
        }
    }

    pub fn build_create_table(&self, create_table: TypedCreateTable) -> LogicalPlan {
        LogicalPlan::CreateTable {
            relation_name: create_table.relation_name,
            columns: create_table.columns,
            defaults: create_table.defaults,
            identities: create_table.identities,
            typed_table_of: create_table.typed_table_of,
            primary_key_columns: create_table.primary_key_columns,
            unique_constraints: create_table
                .unique_constraints
                .into_iter()
                .map(|constraint| aiondb_plan::UniqueConstraintPlan {
                    columns: constraint.columns,
                    name: constraint.name,
                })
                .collect(),
            foreign_keys: create_table
                .foreign_keys
                .into_iter()
                .map(|fk| aiondb_plan::ForeignKeyPlan {
                    columns: fk.columns,
                    referenced_table: fk.referenced_table,
                    referenced_columns: fk.referenced_columns,
                    on_delete: fk.on_delete,
                    on_update: fk.on_update,
                    on_delete_set_columns: fk.on_delete_set_columns,
                    on_update_set_columns: fk.on_update_set_columns,
                    match_type: fk.match_type,
                    name: fk.name,
                })
                .collect(),
            check_constraints: create_table.check_constraints,
            shard_key_columns: create_table.shard_key_columns,
            shard_count: create_table.shard_count,
        }
    }

    pub fn build_create_sequence(&self, create_sequence: TypedCreateSequence) -> LogicalPlan {
        LogicalPlan::CreateSequence {
            sequence_name: create_sequence.sequence_name,
        }
    }

    pub fn build_create_index(&self, create_index: TypedCreateIndex) -> LogicalPlan {
        LogicalPlan::CreateIndex {
            index_name: create_index.index_name,
            table_id: create_index.table_id,
            key_columns: create_index.key_columns,
            key_expressions: create_index.key_expressions,
            hnsw_params: create_index.hnsw_params,
            gin: create_index.gin,
            unique: create_index.unique,
            nulls_not_distinct: create_index.nulls_not_distinct,
            concurrently: create_index.concurrently,
        }
    }

    pub fn build_truncate_table(&self, truncate_table: TypedTruncateTable) -> LogicalPlan {
        LogicalPlan::TruncateTable {
            table_id: truncate_table.table_id,
        }
    }

    pub fn build_drop_table(&self, drop_table: TypedDropTable) -> LogicalPlan {
        LogicalPlan::DropTable {
            table_id: drop_table.table_id,
            cascade: drop_table.cascade,
        }
    }

    pub fn build_drop_index(&self, drop_index: TypedDropIndex) -> LogicalPlan {
        LogicalPlan::DropIndex {
            index_ids: drop_index.index_ids,
        }
    }

    pub fn build_drop_sequence(&self, drop_sequence: TypedDropSequence) -> LogicalPlan {
        LogicalPlan::DropSequence {
            sequence_id: drop_sequence.sequence_id,
        }
    }

    pub fn build_alter_table(&self, alter_table: TypedAlterTable) -> LogicalPlan {
        match alter_table {
            TypedAlterTable::AddColumn(add_column) => LogicalPlan::AlterTableAddColumn {
                table_id: add_column.table_id,
                column: add_column.column,
                default: add_column.default,
            },
            TypedAlterTable::DropColumn(drop_column) => LogicalPlan::AlterTableDropColumn {
                table_id: drop_column.table_id,
                column_id: drop_column.column_id,
            },
            TypedAlterTable::RenameTable(rename) => LogicalPlan::AlterTableRename {
                table_id: rename.table_id,
                new_name: rename.new_name,
            },
            TypedAlterTable::RenameColumn(rename_col) => LogicalPlan::AlterTableRenameColumn {
                table_id: rename_col.table_id,
                old_column_id: rename_col.old_column_id,
                new_column_name: rename_col.new_column_name,
            },
            TypedAlterTable::SetDefault(set_default) => LogicalPlan::AlterTableSetDefault {
                table_id: set_default.table_id,
                column_id: set_default.column_id,
                default_expr: set_default.default_expr,
            },
            TypedAlterTable::DropDefault(drop_default) => LogicalPlan::AlterTableDropDefault {
                table_id: drop_default.table_id,
                column_id: drop_default.column_id,
            },
            TypedAlterTable::SetNotNull(set_not_null) => LogicalPlan::AlterTableSetNotNull {
                table_id: set_not_null.table_id,
                column_id: set_not_null.column_id,
            },
            TypedAlterTable::DropNotNull(drop_not_null) => LogicalPlan::AlterTableDropNotNull {
                table_id: drop_not_null.table_id,
                column_id: drop_not_null.column_id,
            },
            TypedAlterTable::AddConstraint(add_constraint) => {
                LogicalPlan::AlterTableAddConstraint {
                    table_id: add_constraint.table_id,
                    constraint_type: add_constraint.constraint_type,
                    constraint_name: add_constraint.constraint_name,
                    columns: add_constraint.columns,
                    check_expr: add_constraint.check_expr,
                    ref_table: add_constraint.ref_table,
                    ref_columns: add_constraint.ref_columns,
                    on_delete: add_constraint.on_delete,
                    on_update: add_constraint.on_update,
                    on_delete_set_columns: add_constraint.on_delete_set_columns,
                    on_update_set_columns: add_constraint.on_update_set_columns,
                    match_type: add_constraint.match_type,
                }
            }
            TypedAlterTable::DropConstraint(drop_constraint) => {
                LogicalPlan::AlterTableDropConstraint {
                    table_id: drop_constraint.table_id,
                    constraint_name: drop_constraint.constraint_name,
                }
            }
            TypedAlterTable::AlterColumnType(alter_col_type) => {
                LogicalPlan::AlterTableAlterColumnType {
                    table_id: alter_col_type.table_id,
                    column_id: alter_col_type.column_id,
                    new_type: alter_col_type.new_type,
                    raw_type_name: alter_col_type.raw_type_name,
                    text_type_modifier: alter_col_type.text_type_modifier,
                }
            }
            TypedAlterTable::NoOp => LogicalPlan::InternalNoOp {
                tag: "ALTER TABLE".to_owned(),
                notice: None,
            },
        }
    }

    pub fn build_insert(&self, insert: TypedInsert) -> LogicalPlan {
        let TypedInsert {
            table_id,
            columns,
            rows,
            query,
            query_assignments,
            on_conflict,
            returning,
            param_types: _,
        } = insert;
        let on_conflict = on_conflict.map(convert_on_conflict);
        match query {
            Some(query) => LogicalPlan::InsertSelect {
                table_id,
                columns,
                assignments: query_assignments.unwrap_or_default(),
                source: Box::new(self.build_select(query)),
                on_conflict,
                returning,
            },
            None => LogicalPlan::InsertValues {
                table_id,
                columns,
                rows,
                on_conflict,
                returning,
            },
        }
    }

    pub fn build_delete(&self, delete: TypedDelete) -> LogicalPlan {
        LogicalPlan::DeleteFromTable {
            table_id: delete.table_id,
            filter: delete.filter,
            returning: delete.returning,
            using_table_ids: delete.using_table_ids,
        }
    }

    pub fn build_update(&self, update: TypedUpdate) -> LogicalPlan {
        LogicalPlan::UpdateTable {
            table_id: update.table_id,
            assignments: update.assignments,
            filter: update.filter,
            returning: update.returning,
            from_table_ids: update.from_table_ids,
        }
    }

    pub fn build_set_operation(&self, set_op: TypedSetOperation) -> LogicalPlan {
        LogicalPlan::SetOperation {
            op: set_op.op,
            all: set_op.all,
            left: Box::new(self.build_set_branch(*set_op.left)),
            right: Box::new(self.build_set_branch(*set_op.right)),
            output_fields: set_op.output_fields,
            order_by: set_op.order_by,
            limit: set_op.limit,
            offset: set_op.offset,
        }
    }

    fn build_set_branch(&self, branch: TypedSetBranch) -> LogicalPlan {
        match branch {
            TypedSetBranch::Select(select) => self.build_select(select),
            TypedSetBranch::SetOperation(set_op) => self.build_set_operation(set_op),
            TypedSetBranch::Insert(insert) => self.build_insert(insert),
            TypedSetBranch::Update(update) => self.build_update(update),
            TypedSetBranch::Delete(delete) => self.build_delete(delete),
        }
    }

    pub fn build_create_table_as(&self, ctas: TypedCreateTableAs) -> LogicalPlan {
        LogicalPlan::CreateTableAs {
            relation_name: ctas.relation_name,
            columns: ctas.columns,
            with_no_data: ctas.with_no_data,
            source: Box::new(self.build_select(ctas.query)),
        }
    }

    pub fn build_create_view(&self, create_view: TypedCreateView) -> LogicalPlan {
        LogicalPlan::CreateView {
            view_name: create_view.view_name,
            query_sql: create_view.query_sql,
            creation_search_path_schemas: create_view.creation_search_path_schemas,
            or_replace: create_view.or_replace,
            columns: create_view.columns,
            check_option: create_view.check_option,
        }
    }

    pub fn build_drop_view(&self, drop_view: TypedDropView) -> LogicalPlan {
        LogicalPlan::DropView {
            view_id: drop_view.view_id,
        }
    }

    pub fn build_create_node_label(&self, typed: TypedCreateNodeLabel) -> LogicalPlan {
        LogicalPlan::CreateNodeLabel {
            label: typed.label,
            table_id: typed.table_id,
        }
    }

    pub fn build_create_edge_label(&self, typed: TypedCreateEdgeLabel) -> LogicalPlan {
        LogicalPlan::CreateEdgeLabel {
            label: typed.label,
            table_id: typed.table_id,
            source_label: typed.source_label,
            target_label: typed.target_label,
            endpoints: typed.endpoints.map(|(source_id_column, target_id_column)| {
                aiondb_catalog::EdgeEndpoints {
                    source_id_column,
                    target_id_column,
                }
            }),
        }
    }

    pub fn build_drop_node_label(&self, typed: TypedDropNodeLabel) -> LogicalPlan {
        LogicalPlan::DropNodeLabel { label: typed.label }
    }

    pub fn build_drop_edge_label(&self, typed: TypedDropEdgeLabel) -> LogicalPlan {
        LogicalPlan::DropEdgeLabel { label: typed.label }
    }

    pub fn build_create_role(&self, typed: TypedCreateRole) -> LogicalPlan {
        LogicalPlan::CreateRole {
            name: typed.name,
            login: typed.login,
            superuser: typed.superuser,
            password: typed.password,
            inherit: typed.inherit,
            createdb: typed.createdb,
            createrole: typed.createrole,
            replication: typed.replication,
            bypassrls: typed.bypassrls,
            connection_limit: typed.connection_limit,
            valid_until: typed.valid_until,
        }
    }

    pub fn build_drop_role(&self, typed: TypedDropRole) -> LogicalPlan {
        LogicalPlan::DropRole { name: typed.name }
    }

    pub fn build_alter_role(&self, typed: TypedAlterRole) -> LogicalPlan {
        LogicalPlan::AlterRole {
            name: typed.name,
            login: typed.login,
            superuser: typed.superuser,
            current_password_hash: typed.current_password_hash,
            new_password: typed.new_password,
            inherit: typed.inherit,
            createdb: typed.createdb,
            createrole: typed.createrole,
            replication: typed.replication,
            bypassrls: typed.bypassrls,
            connection_limit: typed.connection_limit,
            valid_until: typed.valid_until,
        }
    }

    pub fn build_grant(&self, typed: TypedGrant) -> LogicalPlan {
        LogicalPlan::Grant {
            privileges: typed.privileges,
            target: typed.target,
            role_name: typed.role_name,
        }
    }

    pub fn build_analyze(&self, typed: TypedAnalyze) -> LogicalPlan {
        LogicalPlan::Analyze {
            table_id: typed.table_id,
        }
    }

    pub fn build_vacuum(&self, typed: TypedVacuum) -> LogicalPlan {
        LogicalPlan::Vacuum {
            table_id: typed.table_id,
        }
    }

    pub fn build_revoke(&self, typed: TypedRevoke) -> LogicalPlan {
        LogicalPlan::Revoke {
            privileges: typed.privileges,
            target: typed.target,
            role_name: typed.role_name,
        }
    }

    pub fn build_create_schema(&self, typed: TypedCreateSchema) -> LogicalPlan {
        LogicalPlan::CreateSchema { name: typed.name }
    }

    pub fn build_drop_schema(&self, typed: TypedDropSchema) -> LogicalPlan {
        LogicalPlan::DropSchema {
            schema_id: typed.schema_id,
            name: typed.name,
            cascade: typed.cascade,
        }
    }

    // -------------------------------------------------------------------
    // Cypher query plan builder
    // -------------------------------------------------------------------

    fn cypher_expr_to_typed_with_subqueries(
        &self,
        expr: &Expr,
        catalog: &dyn CatalogReader,
        txn_id: TxnId,
    ) -> DbResult<TypedExpr> {
        let lowered = self.lower_cypher_exists_expr(expr, catalog, txn_id)?;
        cypher_expr_to_typed(&lowered)
    }

    fn cypher_projection_expr_with_subqueries(
        &self,
        item: &aiondb_parser::CypherReturnItem,
        catalog: &dyn CatalogReader,
        txn_id: TxnId,
    ) -> DbResult<ProjectionExpr> {
        let mut lowered = item.clone();
        lowered.expr = self.lower_cypher_exists_expr(&item.expr, catalog, txn_id)?;
        cypher_projection_expr(&lowered)
    }

    fn cypher_sort_expr_with_subqueries(
        &self,
        item: &aiondb_parser::OrderByItem,
        catalog: &dyn CatalogReader,
        txn_id: TxnId,
    ) -> DbResult<aiondb_plan::SortExpr> {
        let mut lowered = item.clone();
        lowered.expr = self.lower_cypher_exists_expr(&item.expr, catalog, txn_id)?;
        cypher_sort_expr(&lowered)
    }

    fn lower_cypher_exists_expr(
        &self,
        expr: &Expr,
        catalog: &dyn CatalogReader,
        txn_id: TxnId,
    ) -> DbResult<Expr> {
        match expr {
            Expr::CypherExists {
                query,
                negated,
                span,
            } => {
                let plan = self.build_cypher_query_plan(query, catalog, txn_id)?;
                let payload = serde_json::to_string(&plan).map_err(|error| {
                    DbError::internal(format!(
                        "failed to encode Cypher EXISTS subquery plan: {error}"
                    ))
                })?;
                Ok(Expr::FunctionCall {
                    name: ObjectName {
                        parts: vec!["__cypher_exists_subquery".to_owned()],
                        span: *span,
                    },
                    args: vec![
                        Expr::Literal(Literal::String(payload), *span),
                        Expr::Literal(Literal::Boolean(*negated), *span),
                    ],
                    distinct: false,
                    filter: None,
                    span: *span,
                })
            }
            Expr::CypherPatternComprehension {
                pattern,
                where_clause,
                map_expr,
                span,
            } => {
                let match_clause = aiondb_plan::graph::CypherMatchClause {
                    optional: false,
                    patterns: vec![self.build_cypher_pattern(pattern, catalog, txn_id)?],
                    filter: where_clause
                        .as_deref()
                        .map(|expr| {
                            self.cypher_expr_to_typed_with_subqueries(expr, catalog, txn_id)
                        })
                        .transpose()?,
                };
                let projection = self.cypher_projection_expr_with_subqueries(
                    &aiondb_parser::CypherReturnItem {
                        expr: (**map_expr).clone(),
                        alias: Some("__value".to_owned()),
                        span: *span,
                    },
                    catalog,
                    txn_id,
                )?;
                let plan = aiondb_plan::graph::CypherQueryPlan {
                    pipeline: vec![aiondb_plan::graph::CypherPipelineOp::Match(match_clause)],
                    matches: Vec::new(),
                    creates: Vec::new(),
                    merges: Vec::new(),
                    sets: Vec::new(),
                    deletes: Vec::new(),
                    returns: vec![projection],
                    order_by: Vec::new(),
                    skip: None,
                    limit: None,
                    distinct: false,
                    union: None,
                };
                let payload = serde_json::to_string(&plan).map_err(|error| {
                    DbError::internal(format!(
                        "failed to encode Cypher pattern comprehension plan: {error}"
                    ))
                })?;
                Ok(Expr::FunctionCall {
                    name: ObjectName {
                        parts: vec!["__cypher_pattern_comprehension".to_owned()],
                        span: *span,
                    },
                    args: vec![Expr::Literal(Literal::String(payload), *span)],
                    distinct: false,
                    filter: None,
                    span: *span,
                })
            }
            Expr::FunctionCall {
                name,
                args,
                distinct,
                filter,
                span,
            } => Ok(Expr::FunctionCall {
                name: name.clone(),
                args: args
                    .iter()
                    .map(|arg| self.lower_cypher_exists_expr(arg, catalog, txn_id))
                    .collect::<DbResult<Vec<_>>>()?,
                distinct: *distinct,
                filter: filter
                    .as_deref()
                    .map(|expr| self.lower_cypher_exists_expr(expr, catalog, txn_id))
                    .transpose()?
                    .map(Box::new),
                span: *span,
            }),
            Expr::UnaryOp { op, expr, span } => Ok(Expr::UnaryOp {
                op: *op,
                expr: Box::new(self.lower_cypher_exists_expr(expr, catalog, txn_id)?),
                span: *span,
            }),
            Expr::BinaryOp {
                left,
                op,
                right,
                span,
            } => Ok(Expr::BinaryOp {
                left: Box::new(self.lower_cypher_exists_expr(left, catalog, txn_id)?),
                op: *op,
                right: Box::new(self.lower_cypher_exists_expr(right, catalog, txn_id)?),
                span: *span,
            }),
            Expr::IsNull {
                expr,
                negated,
                span,
            } => Ok(Expr::IsNull {
                expr: Box::new(self.lower_cypher_exists_expr(expr, catalog, txn_id)?),
                negated: *negated,
                span: *span,
            }),
            Expr::IsDistinctFrom {
                left,
                right,
                negated,
                span,
            } => Ok(Expr::IsDistinctFrom {
                left: Box::new(self.lower_cypher_exists_expr(left, catalog, txn_id)?),
                right: Box::new(self.lower_cypher_exists_expr(right, catalog, txn_id)?),
                negated: *negated,
                span: *span,
            }),
            Expr::Like {
                expr,
                pattern,
                negated,
                case_insensitive,
                span,
            } => Ok(Expr::Like {
                expr: Box::new(self.lower_cypher_exists_expr(expr, catalog, txn_id)?),
                pattern: Box::new(self.lower_cypher_exists_expr(pattern, catalog, txn_id)?),
                negated: *negated,
                case_insensitive: *case_insensitive,
                span: *span,
            }),
            Expr::InList {
                expr,
                list,
                negated,
                span,
            } => Ok(Expr::InList {
                expr: Box::new(self.lower_cypher_exists_expr(expr, catalog, txn_id)?),
                list: list
                    .iter()
                    .map(|item| self.lower_cypher_exists_expr(item, catalog, txn_id))
                    .collect::<DbResult<Vec<_>>>()?,
                negated: *negated,
                span: *span,
            }),
            Expr::Between {
                expr,
                low,
                high,
                negated,
                span,
            } => Ok(Expr::Between {
                expr: Box::new(self.lower_cypher_exists_expr(expr, catalog, txn_id)?),
                low: Box::new(self.lower_cypher_exists_expr(low, catalog, txn_id)?),
                high: Box::new(self.lower_cypher_exists_expr(high, catalog, txn_id)?),
                negated: *negated,
                span: *span,
            }),
            Expr::Cast {
                expr,
                data_type,
                span,
            } => Ok(Expr::Cast {
                expr: Box::new(self.lower_cypher_exists_expr(expr, catalog, txn_id)?),
                data_type: data_type.clone(),
                span: *span,
            }),
            Expr::CaseWhen {
                operand,
                conditions,
                results,
                else_result,
                span,
            } => Ok(Expr::CaseWhen {
                operand: operand
                    .as_deref()
                    .map(|expr| self.lower_cypher_exists_expr(expr, catalog, txn_id))
                    .transpose()?
                    .map(Box::new),
                conditions: conditions
                    .iter()
                    .map(|expr| self.lower_cypher_exists_expr(expr, catalog, txn_id))
                    .collect::<DbResult<Vec<_>>>()?,
                results: results
                    .iter()
                    .map(|expr| self.lower_cypher_exists_expr(expr, catalog, txn_id))
                    .collect::<DbResult<Vec<_>>>()?,
                else_result: else_result
                    .as_deref()
                    .map(|expr| self.lower_cypher_exists_expr(expr, catalog, txn_id))
                    .transpose()?
                    .map(Box::new),
                span: *span,
            }),
            Expr::Array { elements, span } => Ok(Expr::Array {
                elements: elements
                    .iter()
                    .map(|expr| self.lower_cypher_exists_expr(expr, catalog, txn_id))
                    .collect::<DbResult<Vec<_>>>()?,
                span: *span,
            }),
            Expr::WindowFunction {
                function,
                partition_by,
                order_by,
                window_name,
                span,
            } => Ok(Expr::WindowFunction {
                function: Box::new(self.lower_cypher_exists_expr(function, catalog, txn_id)?),
                partition_by: partition_by
                    .iter()
                    .map(|expr| self.lower_cypher_exists_expr(expr, catalog, txn_id))
                    .collect::<DbResult<Vec<_>>>()?,
                order_by: order_by.clone(),
                window_name: window_name.clone(),
                span: *span,
            }),
            _ => Ok(expr.clone()),
        }
    }

    /// Build a [`aiondb_plan::graph::CypherQueryPlan`] directly from the
    /// parser-level [`aiondb_parser::CypherStatement`] AST.
    ///
    /// This intentionally bypasses the full SQL binder / type-checker:
    /// expressions inside patterns and RETURN items are inferred via a
    /// lightweight literal-only converter; the Cypher executor handles
    /// further resolution at runtime.
    pub fn build_cypher_query_plan(
        &self,
        stmt: &aiondb_parser::CypherStatement,
        catalog: &dyn CatalogReader,
        txn_id: TxnId,
    ) -> DbResult<aiondb_plan::graph::CypherQueryPlan> {
        use aiondb_parser::CypherClause;
        use aiondb_plan::graph;

        let mut pipeline = Vec::new();
        let matches = Vec::new();
        let mut creates = Vec::new();
        let mut merges = Vec::new();
        let mut sets = Vec::new();
        let mut deletes = Vec::new();
        let mut returns = Vec::new();
        let mut order_by = Vec::new();
        let mut skip = None;
        let mut limit = None;
        let mut distinct = false;

        for clause in &stmt.clauses {
            match clause {
                CypherClause::Match(m) => {
                    let plan_match = self.build_cypher_match(m, catalog, txn_id)?;
                    // Preserve read-clause order exactly in the pipeline so
                    // MATCH/WITH/UNWIND semantics stay consistent.
                    pipeline.push(graph::CypherPipelineOp::Match(plan_match));
                }
                CypherClause::Create(c) => {
                    creates.push(self.build_cypher_create(c, catalog, txn_id)?);
                }
                CypherClause::Merge(m) => {
                    merges.push(self.build_cypher_merge(m, catalog, txn_id)?);
                }
                CypherClause::Set(s) => {
                    for item in &s.items {
                        self.collect_cypher_set_items(item, &mut sets)?;
                    }
                }
                CypherClause::Delete(d) => {
                    deletes.push(graph::CypherDeleteClause {
                        detach: d.detach,
                        variables: d
                            .variables
                            .iter()
                            .map(|v| graph::CypherDeleteTarget {
                                variable: v.clone(),
                                connected_edge_table_ids: Vec::new(),
                            })
                            .collect(),
                    });
                }
                CypherClause::Return(r) => {
                    distinct = r.distinct;
                    returns = r
                        .items
                        .iter()
                        .map(|item| {
                            self.cypher_projection_expr_with_subqueries(item, catalog, txn_id)
                        })
                        .collect::<DbResult<Vec<_>>>()?;
                    order_by = r
                        .order_by
                        .iter()
                        .map(|item| self.cypher_sort_expr_with_subqueries(item, catalog, txn_id))
                        .collect::<DbResult<Vec<_>>>()?;
                    skip = r
                        .skip
                        .as_ref()
                        .map(|expr| {
                            self.cypher_expr_to_typed_with_subqueries(expr, catalog, txn_id)
                        })
                        .transpose()?;
                    limit = r
                        .limit
                        .as_ref()
                        .map(|expr| {
                            self.cypher_expr_to_typed_with_subqueries(expr, catalog, txn_id)
                        })
                        .transpose()?;
                }
                CypherClause::Unwind(u) => {
                    pipeline.push(graph::CypherPipelineOp::Unwind(graph::CypherUnwindClause {
                        expr: self
                            .cypher_expr_to_typed_with_subqueries(&u.expr, catalog, txn_id)?,
                        variable: u.variable.clone(),
                    }));
                }
                CypherClause::With(w) => {
                    let items = w
                        .items
                        .iter()
                        .map(|item| {
                            self.cypher_projection_expr_with_subqueries(item, catalog, txn_id)
                        })
                        .collect::<DbResult<Vec<_>>>()?;
                    let preserve_binding_sources = w
                        .items
                        .iter()
                        .map(|item| cypher_binding_source(&item.expr))
                        .collect::<Vec<_>>();
                    let with_order_by = w
                        .order_by
                        .iter()
                        .map(|item| self.cypher_sort_expr_with_subqueries(item, catalog, txn_id))
                        .collect::<DbResult<Vec<_>>>()?;
                    pipeline.push(graph::CypherPipelineOp::With(Box::new(
                        graph::CypherWithClause {
                            distinct: w.distinct,
                            items,
                            preserve_binding_sources,
                            filter: w
                                .where_clause
                                .as_ref()
                                .map(|expr| {
                                    self.cypher_expr_to_typed_with_subqueries(expr, catalog, txn_id)
                                })
                                .transpose()?,
                            order_by: with_order_by,
                            skip: w
                                .skip
                                .as_ref()
                                .map(|expr| {
                                    self.cypher_expr_to_typed_with_subqueries(expr, catalog, txn_id)
                                })
                                .transpose()?,
                            limit: w
                                .limit
                                .as_ref()
                                .map(|expr| {
                                    self.cypher_expr_to_typed_with_subqueries(expr, catalog, txn_id)
                                })
                                .transpose()?,
                        },
                    )));
                }
                CypherClause::Remove(r) => {
                    // REMOVE n.prop is equivalent to SET n.prop = NULL
                    for item in &r.items {
                        match item {
                            aiondb_parser::CypherRemoveItem::Property {
                                variable,
                                property,
                                ..
                            } => {
                                sets.push(graph::CypherSetItem {
                                    variable: variable.clone(),
                                    property: Some(property.clone()),
                                    expr: TypedExpr::literal(Value::Null, DataType::Text, true),
                                    table_id: None,
                                });
                            }
                            aiondb_parser::CypherRemoveItem::Label { .. } => {
                                return Err(unsupported_native_cypher_feature(
                                    "REMOVE <variable>:<label>",
                                ));
                            }
                        }
                    }
                }
                CypherClause::Call(call) => {
                    if let Some(subquery) = call.subquery.as_deref() {
                        let subplan = self.build_cypher_query_plan(subquery, catalog, txn_id)?;
                        pipeline.push(graph::CypherPipelineOp::CallSubquery(Box::new(subplan)));
                    } else {
                        let Some(procedure) =
                            crate::cypher_procedure::resolve_graph_procedure_call(
                                &call.procedure,
                                &call.yields,
                                call.args.len(),
                            )?
                        else {
                            return Err(unsupported_native_cypher_feature(format!(
                                "CALL {}",
                                call.procedure
                            )));
                        };
                        let args = call
                            .args
                            .iter()
                            .map(|expr| {
                                self.cypher_expr_to_typed_with_subqueries(expr, catalog, txn_id)
                            })
                            .collect::<DbResult<Vec<_>>>()?;
                        pipeline.push(graph::CypherPipelineOp::ProcedureCall(
                            graph::CypherProcedureCall {
                                procedure: procedure.name,
                                args,
                                yields: procedure.yields,
                            },
                        ));
                    }
                }
                CypherClause::Foreach(fc) => {
                    let foreach = self.build_cypher_foreach(fc, catalog, txn_id)?;
                    pipeline.push(graph::CypherPipelineOp::Foreach(Box::new(foreach)));
                }
            }
        }

        // Handle UNION if present.
        let union = if let Some(ref cypher_union) = stmt.union {
            let right = self.build_cypher_query_plan(&cypher_union.right, catalog, txn_id)?;
            Some(Box::new(graph::CypherUnionPlan {
                all: cypher_union.all,
                right,
            }))
        } else {
            None
        };

        Ok(graph::CypherQueryPlan {
            pipeline,
            matches,
            creates,
            merges,
            sets,
            deletes,
            returns,
            order_by,
            skip,
            limit,
            distinct,
            union,
        })
    }

    fn build_cypher_foreach(
        &self,
        fc: &aiondb_parser::CypherForeachClause,
        catalog: &dyn CatalogReader,
        txn_id: TxnId,
    ) -> DbResult<aiondb_plan::graph::CypherForeachPlan> {
        use aiondb_parser::CypherClause;
        use aiondb_plan::graph;

        let expr = self.cypher_expr_to_typed_with_subqueries(&fc.expr, catalog, txn_id)?;
        let mut body = Vec::new();

        for clause in &fc.clauses {
            match clause {
                CypherClause::Set(s) => {
                    let mut set_items = Vec::new();
                    for item in &s.items {
                        self.collect_cypher_set_items(item, &mut set_items)?;
                    }
                    for set_item in set_items {
                        body.push(graph::CypherForeachOp::Set(set_item));
                    }
                }
                CypherClause::Remove(r) => {
                    for item in &r.items {
                        match item {
                            aiondb_parser::CypherRemoveItem::Property {
                                variable,
                                property,
                                ..
                            } => {
                                body.push(graph::CypherForeachOp::Set(graph::CypherSetItem {
                                    variable: variable.clone(),
                                    property: Some(property.clone()),
                                    expr: TypedExpr::literal(Value::Null, DataType::Text, true),
                                    table_id: None,
                                }));
                            }
                            aiondb_parser::CypherRemoveItem::Label { .. } => {
                                return Err(unsupported_native_cypher_feature(
                                    "REMOVE <variable>:<label> inside FOREACH",
                                ));
                            }
                        }
                    }
                }
                CypherClause::Create(c) => {
                    body.push(graph::CypherForeachOp::Create(
                        self.build_cypher_create(c, catalog, txn_id)?,
                    ));
                }
                CypherClause::Merge(m) => {
                    body.push(graph::CypherForeachOp::Merge(Box::new(
                        self.build_cypher_merge(m, catalog, txn_id)?,
                    )));
                }
                CypherClause::Delete(d) => {
                    body.push(graph::CypherForeachOp::Delete(graph::CypherDeleteClause {
                        detach: d.detach,
                        variables: d
                            .variables
                            .iter()
                            .map(|v| graph::CypherDeleteTarget {
                                variable: v.clone(),
                                connected_edge_table_ids: Vec::new(),
                            })
                            .collect(),
                    }));
                }
                CypherClause::Foreach(nested) => {
                    body.push(graph::CypherForeachOp::Foreach(Box::new(
                        self.build_cypher_foreach(nested, catalog, txn_id)?,
                    )));
                }
                _ => {
                    return Err(unsupported_native_cypher_feature(
                        "only SET, REMOVE, CREATE, MERGE, DELETE and nested FOREACH are allowed inside FOREACH",
                    ));
                }
            }
        }

        Ok(graph::CypherForeachPlan {
            variable: fc.variable.clone(),
            expr,
            body,
        })
    }

    fn build_cypher_match(
        &self,
        m: &aiondb_parser::CypherMatchClause,
        catalog: &dyn CatalogReader,
        txn_id: TxnId,
    ) -> DbResult<aiondb_plan::graph::CypherMatchClause> {
        let patterns = m
            .patterns
            .iter()
            .map(|p| self.build_cypher_pattern(p, catalog, txn_id))
            .collect::<DbResult<Vec<_>>>()?;

        Ok(aiondb_plan::graph::CypherMatchClause {
            optional: m.optional,
            patterns,
            filter: m
                .where_clause
                .as_ref()
                .map(|expr| self.cypher_expr_to_typed_with_subqueries(expr, catalog, txn_id))
                .transpose()?,
        })
    }

    fn build_cypher_create(
        &self,
        c: &aiondb_parser::CypherCreateClause,
        catalog: &dyn CatalogReader,
        txn_id: TxnId,
    ) -> DbResult<aiondb_plan::graph::CypherCreateClause> {
        let patterns = c
            .patterns
            .iter()
            .map(|p| self.build_cypher_pattern(p, catalog, txn_id))
            .collect::<DbResult<Vec<_>>>()?;

        Ok(aiondb_plan::graph::CypherCreateClause { patterns })
    }

    fn build_cypher_merge(
        &self,
        m: &aiondb_parser::CypherMergeClause,
        catalog: &dyn CatalogReader,
        txn_id: TxnId,
    ) -> DbResult<aiondb_plan::graph::CypherMergeClause> {
        let pattern = self.build_cypher_pattern(&m.pattern, catalog, txn_id)?;

        let mut on_create_set = Vec::new();
        let mut on_match_set = Vec::new();

        for action in &m.actions {
            let target = if action.on_create {
                &mut on_create_set
            } else {
                &mut on_match_set
            };
            for item in &action.items {
                self.collect_cypher_set_items(item, target)?;
            }
        }

        Ok(aiondb_plan::graph::CypherMergeClause {
            pattern,
            on_create_set,
            on_match_set,
        })
    }

    fn build_cypher_pattern(
        &self,
        p: &aiondb_parser::CypherPathPattern,
        catalog: &dyn CatalogReader,
        txn_id: TxnId,
    ) -> DbResult<aiondb_plan::graph::CypherPattern> {
        let nodes = p
            .nodes
            .iter()
            .map(|n| self.build_cypher_node_pattern(n, catalog, txn_id))
            .collect::<DbResult<Vec<_>>>()?;
        let relationships = p
            .rels
            .iter()
            .map(|r| self.build_cypher_rel_pattern(r, catalog, txn_id))
            .collect::<DbResult<Vec<_>>>()?;

        let path_function = p.path_function.map(|f| match f {
            aiondb_parser::CypherPathFunction::ShortestPath => {
                aiondb_plan::graph::CypherPathFunction::ShortestPath
            }
            aiondb_parser::CypherPathFunction::AllShortestPaths => {
                aiondb_plan::graph::CypherPathFunction::AllShortestPaths
            }
        });

        Ok(aiondb_plan::graph::CypherPattern {
            path_function,
            path_variable: p.path_variable.clone(),
            nodes,
            relationships,
        })
    }

    fn build_cypher_node_pattern(
        &self,
        n: &aiondb_parser::CypherNodePattern,
        catalog: &dyn CatalogReader,
        txn_id: TxnId,
    ) -> DbResult<aiondb_plan::graph::CypherNodePattern> {
        // Resolve the label to a table_id via the catalog.
        let (label, table_id) = if let Some(lbl) = n.labels.first() {
            let tid = catalog
                .get_node_label(txn_id, lbl)?
                .map(|desc| desc.table_id);
            (Some(lbl.clone()), tid)
        } else {
            (None, None)
        };

        let properties: Vec<aiondb_plan::graph::CypherPropertyExpr> = n
            .properties
            .iter()
            .map(|(key, expr)| {
                Ok(aiondb_plan::graph::CypherPropertyExpr {
                    key: key.clone(),
                    value: self.cypher_expr_to_typed_with_subqueries(expr, catalog, txn_id)?,
                })
            })
            .collect::<DbResult<Vec<_>>>()?;

        // Attempt to find a BTree index for the first property filter that
        // has a constant value.  This allows the executor to use an index
        // scan instead of a full table scan.
        let index_scan = if let Some(tid) = table_id {
            resolve_property_index_scan(catalog, txn_id, tid, &properties)?
        } else {
            None
        };

        Ok(aiondb_plan::graph::CypherNodePattern {
            variable: n.variable.clone(),
            label,
            table_id,
            properties,
            index_scan,
            range_pushdown: Vec::new(),
        })
    }

    fn build_cypher_rel_pattern(
        &self,
        r: &aiondb_parser::CypherRelPattern,
        catalog: &dyn CatalogReader,
        txn_id: TxnId,
    ) -> DbResult<aiondb_plan::graph::CypherRelPattern> {
        let table_id = if let Some(ref rel_type) = r.rel_type {
            catalog
                .get_edge_label(txn_id, rel_type)?
                .map(|desc| desc.table_id)
        } else {
            None
        };

        let direction = match r.direction {
            aiondb_parser::CypherDirection::Outgoing => {
                aiondb_plan::graph::CypherRelDirection::Outgoing
            }
            aiondb_parser::CypherDirection::Incoming => {
                aiondb_plan::graph::CypherRelDirection::Incoming
            }
            aiondb_parser::CypherDirection::Both => aiondb_plan::graph::CypherRelDirection::Both,
        };

        let properties: Vec<aiondb_plan::graph::CypherPropertyExpr> = r
            .properties
            .iter()
            .map(|(key, expr)| {
                Ok(aiondb_plan::graph::CypherPropertyExpr {
                    key: key.clone(),
                    value: self.cypher_expr_to_typed_with_subqueries(expr, catalog, txn_id)?,
                })
            })
            .collect::<DbResult<Vec<_>>>()?;

        // Attempt to find a BTree index for edge property filters.
        let index_scan = if let Some(tid) = table_id {
            resolve_property_index_scan(catalog, txn_id, tid, &properties)?
        } else {
            None
        };

        Ok(aiondb_plan::graph::CypherRelPattern {
            variable: r.variable.clone(),
            rel_type: r.rel_type.clone(),
            rel_type_alternatives: r.rel_types_alt.clone(),
            table_id,
            direction,
            properties,
            min_hops: r.min_hops,
            max_hops: r.max_hops,
            index_scan,
        })
    }

    fn collect_cypher_set_items(
        &self,
        item: &aiondb_parser::CypherSetItem,
        out: &mut Vec<aiondb_plan::graph::CypherSetItem>,
    ) -> DbResult<()> {
        match item {
            aiondb_parser::CypherSetItem::Property {
                variable,
                property,
                expr,
                ..
            } => {
                out.push(aiondb_plan::graph::CypherSetItem {
                    variable: variable.clone(),
                    property: Some(property.clone()),
                    expr: cypher_expr_to_typed(expr)?,
                    table_id: None,
                });
                Ok(())
            }
            aiondb_parser::CypherSetItem::Label {
                variable, label, ..
            } => Err(unsupported_native_cypher_feature(format!(
                "SET {variable}:{label} — label assignment is not yet supported in the direct pipeline",
            ))),
            aiondb_parser::CypherSetItem::ReplaceProperties {
                variable, entries, ..
            }
            | aiondb_parser::CypherSetItem::MergeProperties {
                variable, entries, ..
            } => {
                for (key, value_expr) in entries {
                    out.push(aiondb_plan::graph::CypherSetItem {
                        variable: variable.clone(),
                        property: Some(key.clone()),
                        expr: cypher_expr_to_typed(value_expr)?,
                        table_id: None,
                    });
                }
                Ok(())
            }
        }
    }
}

include!("logical_builder_cypher.rs");
