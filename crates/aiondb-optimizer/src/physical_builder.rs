use crate::cost::PlanCost;
use crate::predicate_pushdown::map_ordinals;
use aiondb_core::{DataType, Value};
use aiondb_plan::{
    JoinType, LogicalPlan, PhysicalPlan, ProjectionExpr, ScalarFunction, ScanAccessPath,
    SetOperationType, SortExpr, TypedExpr, TypedExprKind,
};

#[path = "physical_builder_costing_and_keys.rs"]
mod physical_builder_costing_and_keys;

pub(crate) use self::physical_builder_costing_and_keys::estimate_filter_selectivity;
pub(crate) use self::physical_builder_costing_and_keys::estimate_hybrid_function_rows;
pub(crate) use self::physical_builder_costing_and_keys::exposed_plan_width;
pub(crate) use self::physical_builder_costing_and_keys::plan_sorted_on_keys;
pub(crate) use self::physical_builder_costing_and_keys::plan_sorted_prefix;
pub use self::physical_builder_costing_and_keys::{
    estimate_join_condition_selectivity, estimate_plan_rows, is_const_expr,
};
use self::physical_builder_costing_and_keys::{
    extract_equi_join_keys, inputs_sorted_on_keys, is_set_returning_function, join_child_widths,
    sorted_prefix_matches_keys,
};

/// Public wrapper for `extract_equi_join_keys` used by the optimizer
/// to detect parameterized index join opportunities.
pub fn extract_equi_join_keys_public(
    expr: Option<&TypedExpr>,
    left_width: usize,
    right_width: usize,
) -> Option<(Vec<usize>, Vec<usize>, Option<TypedExpr>)> {
    extract_equi_join_keys(expr, left_width, right_width)
}

pub struct PhysicalBuilder;

/// Threshold above which a full SeqScan is wrapped in a `Gather` node so it
/// can be split across worker threads when the session permits parallelism
/// (`max_parallel_workers_per_query > 1`). Below this row estimate the
/// fork/join overhead would dominate, so we stay single-threaded.
const PARALLEL_SEQ_SCAN_MIN_ROWS: f64 = 10_000.0;

/// If `plan` is a `ProjectTable` over a plain `SeqScan` with no order-by,
/// limit, distinct, or aggregate that would break under independent
/// per-worker execution, wrap it in a `Gather` node so the executor can
/// dispatch worker partitions in parallel. `num_workers = 0` lets the
/// executor pick `max_parallel_workers_per_query` at runtime.
fn maybe_wrap_seq_scan_in_gather(plan: PhysicalPlan) -> PhysicalPlan {
    let estimated_rows = estimate_plan_rows(&plan);
    let eligible = matches!(
        &plan,
        PhysicalPlan::ProjectTable {
            access_path: ScanAccessPath::SeqScan,
            order_by,
            limit: None,
            offset: None,
            distinct: false,
            distinct_on,
            outputs,
            ..
        }
            if order_by.is_empty()
                && distinct_on.is_empty()
                && !outputs.iter().any(|expr| project_table_output_blocks_parallel(&expr.expr))
    );
    if !eligible || estimated_rows < PARALLEL_SEQ_SCAN_MIN_ROWS {
        return plan;
    }
    let output_fields = plan.output_fields();
    PhysicalPlan::Gather {
        child: Box::new(plan),
        num_workers: 0,
        output_fields,
        preserve_order: false,
    }
}

/// An output column that aggregates, calls a window function, or otherwise
/// folds across the whole table cannot be split across workers without a
/// real partial-aggregate / merge stage, which we don't have yet.
/// Walks the typed expression and reports any aggregate or window function;
/// matches the variant names defined in `aiondb-plan::expr::TypedExprKind`.
/// Sub-trees we don't recognise are treated as containing no aggregate,
/// which is safe (we just miss the parallel opportunity for that plan).
fn project_table_output_blocks_parallel(expr: &TypedExpr) -> bool {
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
                | TypedExprKind::WindowFunction { .. }
        ) {
            return true;
        }
        match &expr.kind {
            TypedExprKind::ScalarFunction { args, .. } => stack.extend(args.iter()),
            TypedExprKind::LogicalAnd { left, right }
            | TypedExprKind::LogicalOr { left, right }
            | TypedExprKind::BinaryEq { left, right }
            | TypedExprKind::BinaryNe { left, right }
            | TypedExprKind::BinaryGt { left, right }
            | TypedExprKind::BinaryGe { left, right }
            | TypedExprKind::BinaryLt { left, right }
            | TypedExprKind::BinaryLe { left, right }
            | TypedExprKind::ArithAdd { left, right }
            | TypedExprKind::ArithSub { left, right }
            | TypedExprKind::ArithMul { left, right }
            | TypedExprKind::ArithDiv { left, right }
            | TypedExprKind::ArithMod { left, right }
            | TypedExprKind::Concat { left, right }
            | TypedExprKind::IsDistinctFrom { left, right, .. }
            | TypedExprKind::Nullif { left, right } => {
                stack.push(left);
                stack.push(right);
            }
            TypedExprKind::LogicalNot { expr: inner }
            | TypedExprKind::Negate { expr: inner }
            | TypedExprKind::IsNull { expr: inner, .. }
            | TypedExprKind::Cast { expr: inner, .. } => stack.push(inner),
            _ => {}
        }
    }
    false
}

fn contains_quantified_array_comparison(expr: Option<&TypedExpr>) -> bool {
    fn visit(expr: &TypedExpr) -> bool {
        match &expr.kind {
            TypedExprKind::ScalarFunction { func, args } => {
                matches!(
                    func,
                    ScalarFunction::Generic(name)
                        if name.starts_with("__aiondb_quantified_any_")
                            || name.starts_with("__aiondb_quantified_all_")
                ) || args.iter().any(visit)
            }
            TypedExprKind::LogicalAnd { left, right }
            | TypedExprKind::LogicalOr { left, right }
            | TypedExprKind::BinaryEq { left, right }
            | TypedExprKind::BinaryNe { left, right }
            | TypedExprKind::BinaryGt { left, right }
            | TypedExprKind::BinaryGe { left, right }
            | TypedExprKind::BinaryLt { left, right }
            | TypedExprKind::BinaryLe { left, right }
            | TypedExprKind::ArithAdd { left, right }
            | TypedExprKind::ArithSub { left, right }
            | TypedExprKind::ArithMul { left, right }
            | TypedExprKind::ArithDiv { left, right }
            | TypedExprKind::ArithMod { left, right }
            | TypedExprKind::Concat { left, right }
            | TypedExprKind::IsDistinctFrom { left, right, .. }
            | TypedExprKind::Nullif { left, right }
            | TypedExprKind::JsonGet { left, right }
            | TypedExprKind::JsonGetText { left, right }
            | TypedExprKind::JsonContains { left, right } => visit(left) || visit(right),
            TypedExprKind::LogicalNot { expr }
            | TypedExprKind::Negate { expr }
            | TypedExprKind::Cast { expr, .. }
            | TypedExprKind::IsNull { expr, .. } => visit(expr),
            TypedExprKind::Like { expr, pattern, .. } => visit(expr) || visit(pattern),
            TypedExprKind::InList { expr, list, .. } => visit(expr) || list.iter().any(visit),
            TypedExprKind::Between {
                expr, low, high, ..
            } => visit(expr) || visit(low) || visit(high),
            TypedExprKind::CaseWhen {
                conditions,
                results,
                else_result,
            } => {
                conditions.iter().any(visit)
                    || results.iter().any(visit)
                    || else_result.as_ref().is_some_and(|expr| visit(expr))
            }
            TypedExprKind::Coalesce { args } => args.iter().any(visit),
            TypedExprKind::AggCount { expr, filter, .. } => {
                expr.as_ref().is_some_and(|expr| visit(expr))
                    || filter.as_ref().is_some_and(|expr| visit(expr))
            }
            TypedExprKind::AggSum { expr, filter, .. }
            | TypedExprKind::AggAnyValue { expr, filter }
            | TypedExprKind::AggMin { expr, filter }
            | TypedExprKind::AggMax { expr, filter } => {
                visit(expr) || filter.as_ref().is_some_and(|expr| visit(expr))
            }
            _ => false,
        }
    }

    expr.is_some_and(visit)
}

fn plan_contains_project_values(plan: &PhysicalPlan) -> bool {
    match plan {
        PhysicalPlan::ProjectValues { .. } => true,
        PhysicalPlan::NestedLoopJoin { left, right, .. }
        | PhysicalPlan::HashJoin { left, right, .. }
        | PhysicalPlan::MergeJoin { left, right, .. }
        | PhysicalPlan::SetOperation { left, right, .. } => {
            plan_contains_project_values(left) || plan_contains_project_values(right)
        }
        PhysicalPlan::NestedLoopIndexJoin { left, .. }
        | PhysicalPlan::AggregateSource { source: left, .. } => plan_contains_project_values(left),
        PhysicalPlan::Gather { child, .. } => plan_contains_project_values(child),
        PhysicalPlan::DistributedAppend { fragments, .. } => {
            fragments.iter().any(plan_contains_project_values)
        }
        PhysicalPlan::RecursiveCte {
            base, recursive, ..
        } => plan_contains_project_values(base) || plan_contains_project_values(recursive),
        _ => false,
    }
}

pub(crate) fn collect_union_all_append_fragments(
    plan: PhysicalPlan,
    fragments: &mut Vec<PhysicalPlan>,
) {
    let mut stack = vec![plan];
    while let Some(plan) = stack.pop() {
        match plan {
            PhysicalPlan::SetOperation {
                op: SetOperationType::Union,
                all: true,
                left,
                right,
                order_by,
                limit,
                offset,
                ..
            } if order_by.is_empty() && limit.is_none() && offset.is_none() => {
                stack.push(*right);
                stack.push(*left);
            }
            PhysicalPlan::DistributedAppend {
                fragments: nested,
                order_by,
                limit,
                offset,
                ..
            } if order_by.is_empty() && limit.is_none() && offset.is_none() => {
                stack.extend(nested.into_iter().rev());
            }
            other => fragments.push(other),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ExposedJoinSwapPolicy {
    Disallow,
    SingleRowOnly,
    AllowMultiRow,
}

impl ExposedJoinSwapPolicy {
    pub(crate) fn allows_empty_outputs(self) -> bool {
        !matches!(self, Self::Disallow)
    }

    fn allows_multi_row_hybrid(self) -> bool {
        matches!(self, Self::AllowMultiRow)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct JoinSwapOrdinalRemap {
    left_width: usize,
    right_width: usize,
}

impl JoinSwapOrdinalRemap {
    pub(crate) fn new(left_width: usize, right_width: usize) -> Self {
        Self {
            left_width,
            right_width,
        }
    }

    pub(crate) fn total_width(self) -> usize {
        self.left_width.saturating_add(self.right_width)
    }

    pub(crate) fn remap_ordinal(self, ordinal: usize) -> usize {
        swap_join_ordinal(ordinal, self.left_width, self.right_width)
    }
}

impl PhysicalBuilder {
    pub fn build(&self, logical: LogicalPlan) -> PhysicalPlan {
        match logical {
            LogicalPlan::ProjectOnce {
                outputs,
                filter,
                order_by,
                limit,
                offset,
                distinct,
                distinct_on,
            } => PhysicalPlan::ProjectOnce {
                outputs,
                filter,
                order_by,
                limit,
                offset,
                distinct,
                distinct_on,
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
            } => {
                // Constant-fold: when the WHERE filter is a pure constant
                // expression that evaluates to FALSE (or NULL), replace
                // the table scan with a no-row Result node, matching
                // PostgreSQL's One-Time Filter optimisation.
                if let Some(ref f) = filter {
                    if is_const_expr(f) {
                        let evaluator = aiondb_eval::ExpressionEvaluator;
                        if let Ok(val) = evaluator.evaluate(f) {
                            let is_false = matches!(val, Value::Boolean(false) | Value::Null);
                            if is_false {
                                return PhysicalPlan::ProjectOnce {
                                    outputs,
                                    filter: Some(TypedExpr::literal(
                                        Value::Boolean(false),
                                        DataType::Boolean,
                                        false,
                                    )),
                                    order_by,
                                    limit,
                                    offset,
                                    distinct,
                                    distinct_on,
                                };
                            }
                        }
                    }
                }
                maybe_wrap_seq_scan_in_gather(PhysicalPlan::ProjectTable {
                    table_id,
                    outputs,
                    filter,
                    order_by,
                    limit,
                    offset,
                    distinct,
                    distinct_on,
                    access_path: ScanAccessPath::SeqScan,
                })
            }
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
            } => {
                // Keep the same one-time FALSE/NULL filter shortcut used for
                // plain table projections. No rows means no row-level locks.
                if let Some(ref f) = filter {
                    if is_const_expr(f) {
                        let evaluator = aiondb_eval::ExpressionEvaluator;
                        if let Ok(val) = evaluator.evaluate(f) {
                            let is_false = matches!(val, Value::Boolean(false) | Value::Null);
                            if is_false {
                                return PhysicalPlan::ProjectOnce {
                                    outputs,
                                    filter: Some(TypedExpr::literal(
                                        Value::Boolean(false),
                                        DataType::Boolean,
                                        false,
                                    )),
                                    order_by,
                                    limit,
                                    offset,
                                    distinct,
                                    distinct_on,
                                };
                            }
                        }
                    }
                }
                PhysicalPlan::LockingProjectTable {
                    table_id,
                    outputs,
                    filter,
                    order_by,
                    limit,
                    offset,
                    distinct,
                    distinct_on,
                    access_path: ScanAccessPath::SeqScan,
                    row_lock,
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
            } => PhysicalPlan::ProjectSource {
                source: Box::new(self.build(*source)),
                outputs,
                filter,
                order_by,
                limit,
                offset,
                distinct,
                distinct_on,
            },
            LogicalPlan::HybridFunctionScan {
                function_name,
                args,
                output_fields,
            } => PhysicalPlan::HybridFunctionScan {
                function_name,
                args,
                output_fields,
            },
            LogicalPlan::CreateTable {
                relation_name,
                columns,
                defaults,
                identities,
                typed_table_of,
                primary_key_columns,
                unique_constraints,
                foreign_keys,
                check_constraints,
                shard_key_columns,
                shard_count,
            } => PhysicalPlan::CreateTable {
                relation_name,
                columns,
                defaults,
                identities,
                typed_table_of,
                primary_key_columns,
                unique_constraints,
                foreign_keys,
                check_constraints,
                shard_key_columns,
                shard_count,
            },
            LogicalPlan::CreateSequence { sequence_name } => {
                PhysicalPlan::CreateSequence { sequence_name }
            }
            LogicalPlan::CreateIndex {
                index_name,
                table_id,
                key_columns,
                key_expressions,
                hnsw_params,
                gin,
                unique,
                nulls_not_distinct,
                concurrently,
            } => PhysicalPlan::CreateIndex {
                index_name,
                table_id,
                key_columns,
                key_expressions,
                hnsw_params,
                gin,
                unique,
                nulls_not_distinct,
                concurrently,
            },
            LogicalPlan::TruncateTable { table_id } => PhysicalPlan::TruncateTable { table_id },
            LogicalPlan::DropTable { table_id, cascade } => {
                PhysicalPlan::DropTable { table_id, cascade }
            }
            LogicalPlan::DropIndex { index_ids } => PhysicalPlan::DropIndex { index_ids },
            LogicalPlan::DropSequence { sequence_id } => PhysicalPlan::DropSequence { sequence_id },
            LogicalPlan::InsertValues {
                table_id,
                columns,
                rows,
                on_conflict,
                returning,
            } => PhysicalPlan::InsertValues {
                table_id,
                columns,
                rows,
                on_conflict,
                returning,
            },
            LogicalPlan::InsertSelect {
                table_id,
                columns,
                assignments,
                source,
                on_conflict,
                returning,
            } => PhysicalPlan::InsertSelect {
                table_id,
                columns,
                assignments,
                source: Box::new(self.build(*source)),
                on_conflict,
                returning,
            },
            LogicalPlan::CreateTableAs {
                relation_name,
                columns,
                with_no_data,
                source,
            } => PhysicalPlan::CreateTableAs {
                relation_name,
                columns,
                with_no_data,
                source: Box::new(self.build(*source)),
            },
            LogicalPlan::CreateView {
                view_name,
                query_sql,
                creation_search_path_schemas,
                or_replace,
                columns,
                check_option,
            } => PhysicalPlan::CreateView {
                view_name,
                query_sql,
                creation_search_path_schemas,
                or_replace,
                columns,
                check_option,
            },
            LogicalPlan::DropView { view_id } => PhysicalPlan::DropView { view_id },
            LogicalPlan::CopyFrom { table_id, columns } => {
                PhysicalPlan::CopyFrom { table_id, columns }
            }
            LogicalPlan::CopyTo { table_id, columns } => PhysicalPlan::CopyTo { table_id, columns },
            LogicalPlan::DeleteFromTable {
                table_id,
                filter,
                returning,
                using_table_ids,
            } => PhysicalPlan::DeleteFromTable {
                table_id,
                filter,
                returning,
                using_table_ids,
            },
            LogicalPlan::UpdateTable {
                table_id,
                assignments,
                filter,
                returning,
                from_table_ids,
            } => PhysicalPlan::UpdateTable {
                table_id,
                assignments,
                filter,
                returning,
                from_table_ids,
            },
            LogicalPlan::AlterTableAddColumn {
                table_id,
                column,
                default,
            } => PhysicalPlan::AlterTableAddColumn {
                table_id,
                column,
                default,
            },
            LogicalPlan::AlterTableDropColumn {
                table_id,
                column_id,
            } => PhysicalPlan::AlterTableDropColumn {
                table_id,
                column_id,
            },
            LogicalPlan::AlterTableRename { table_id, new_name } => {
                PhysicalPlan::AlterTableRename { table_id, new_name }
            }
            LogicalPlan::AlterTableRenameColumn {
                table_id,
                old_column_id,
                new_column_name,
            } => PhysicalPlan::AlterTableRenameColumn {
                table_id,
                old_column_id,
                new_column_name,
            },
            LogicalPlan::AlterTableSetDefault {
                table_id,
                column_id,
                default_expr,
            } => PhysicalPlan::AlterTableSetDefault {
                table_id,
                column_id,
                default_expr,
            },
            LogicalPlan::AlterTableDropDefault {
                table_id,
                column_id,
            } => PhysicalPlan::AlterTableDropDefault {
                table_id,
                column_id,
            },
            LogicalPlan::AlterTableSetNotNull {
                table_id,
                column_id,
            } => PhysicalPlan::AlterTableSetNotNull {
                table_id,
                column_id,
            },
            LogicalPlan::AlterTableDropNotNull {
                table_id,
                column_id,
            } => PhysicalPlan::AlterTableDropNotNull {
                table_id,
                column_id,
            },
            LogicalPlan::AlterTableAddConstraint {
                table_id,
                constraint_type,
                constraint_name,
                columns,
                check_expr,
                ref_table,
                ref_columns,
                on_delete,
                on_update,
                on_delete_set_columns,
                on_update_set_columns,
                match_type,
            } => PhysicalPlan::AlterTableAddConstraint {
                table_id,
                constraint_type,
                constraint_name,
                columns,
                check_expr,
                ref_table,
                ref_columns,
                on_delete,
                on_update,
                on_delete_set_columns,
                on_update_set_columns,
                match_type,
            },
            LogicalPlan::AlterTableDropConstraint {
                table_id,
                constraint_name,
            } => PhysicalPlan::AlterTableDropConstraint {
                table_id,
                constraint_name,
            },
            LogicalPlan::AlterTableAlterColumnType {
                table_id,
                column_id,
                new_type,
                raw_type_name,
                text_type_modifier,
            } => PhysicalPlan::AlterTableAlterColumnType {
                table_id,
                column_id,
                new_type,
                raw_type_name,
                text_type_modifier,
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
                let phys_left = self.build(*left);
                let phys_right = self.build(*right);
                self.build_join_from_physical(
                    phys_left,
                    phys_right,
                    join_type,
                    condition,
                    outputs,
                    filter,
                    order_by,
                    limit,
                    offset,
                    distinct,
                    distinct_on,
                )
            }
            LogicalPlan::SeqScan { table_id } => PhysicalPlan::SeqScan { table_id },
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
            } => PhysicalPlan::Aggregate {
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
                access_path: ScanAccessPath::SeqScan,
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
            } => PhysicalPlan::AggregateSource {
                source: Box::new(self.build(*source)),
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
            } => {
                let left = self.build(*left);
                let right = self.build(*right);

                if matches!(op, SetOperationType::Union) && all {
                    let mut fragments = Vec::new();
                    collect_union_all_append_fragments(left, &mut fragments);
                    collect_union_all_append_fragments(right, &mut fragments);
                    if fragments.len() > 2 {
                        return PhysicalPlan::DistributedAppend {
                            fragments,
                            output_fields,
                            order_by,
                            limit,
                            offset,
                        };
                    }
                    let mut iter = fragments.into_iter();
                    match (iter.next(), iter.next(), iter.next()) {
                        (Some(left), Some(right), None) => PhysicalPlan::SetOperation {
                            op,
                            all,
                            left: Box::new(left),
                            right: Box::new(right),
                            output_fields,
                            order_by,
                            limit,
                            offset,
                        },
                        (first, second, third) => {
                            let mut fragments = Vec::new();
                            if let Some(fragment) = first {
                                fragments.push(fragment);
                            }
                            if let Some(fragment) = second {
                                fragments.push(fragment);
                            }
                            if let Some(fragment) = third {
                                fragments.push(fragment);
                            }
                            fragments.extend(iter);
                            PhysicalPlan::DistributedAppend {
                                fragments,
                                output_fields,
                                order_by,
                                limit,
                                offset,
                            }
                        }
                    }
                } else {
                    PhysicalPlan::SetOperation {
                        op,
                        all,
                        left: Box::new(left),
                        right: Box::new(right),
                        output_fields,
                        order_by,
                        limit,
                        offset,
                    }
                }
            }
            LogicalPlan::ProjectValues {
                output_fields,
                rows,
                order_by,
                limit,
                offset,
            } => PhysicalPlan::ProjectValues {
                output_fields,
                rows,
                order_by,
                limit,
                offset,
            },
            LogicalPlan::CreateNodeLabel { label, table_id } => {
                PhysicalPlan::CreateNodeLabel { label, table_id }
            }
            LogicalPlan::CreateEdgeLabel {
                label,
                table_id,
                source_label,
                target_label,
                endpoints,
            } => PhysicalPlan::CreateEdgeLabel {
                label,
                table_id,
                source_label,
                target_label,
                endpoints,
            },
            LogicalPlan::DropNodeLabel { label } => PhysicalPlan::DropNodeLabel { label },
            LogicalPlan::DropEdgeLabel { label } => PhysicalPlan::DropEdgeLabel { label },
            LogicalPlan::CreateRole {
                name,
                login,
                superuser,
                password,
                inherit,
                createdb,
                createrole,
                replication,
                bypassrls,
                connection_limit,
                valid_until,
            } => PhysicalPlan::CreateRole {
                name,
                login,
                superuser,
                password,
                inherit,
                createdb,
                createrole,
                replication,
                bypassrls,
                connection_limit,
                valid_until,
            },
            LogicalPlan::DropRole { name } => PhysicalPlan::DropRole { name },
            LogicalPlan::AlterRole {
                name,
                login,
                superuser,
                current_password_hash,
                new_password,
                inherit,
                createdb,
                createrole,
                replication,
                bypassrls,
                connection_limit,
                valid_until,
            } => PhysicalPlan::AlterRole {
                name,
                login,
                superuser,
                current_password_hash,
                new_password,
                inherit,
                createdb,
                createrole,
                replication,
                bypassrls,
                connection_limit,
                valid_until,
            },
            LogicalPlan::Grant {
                privileges,
                target,
                role_name,
            } => PhysicalPlan::Grant {
                privileges,
                target,
                role_name,
            },
            LogicalPlan::Revoke {
                privileges,
                target,
                role_name,
            } => PhysicalPlan::Revoke {
                privileges,
                target,
                role_name,
            },
            LogicalPlan::Analyze { table_id } => PhysicalPlan::Analyze { table_id },
            LogicalPlan::Vacuum { table_id } => PhysicalPlan::Vacuum { table_id },
            LogicalPlan::Checkpoint => PhysicalPlan::Checkpoint,
            LogicalPlan::Lock {
                table_ids,
                mode,
                nowait,
            } => PhysicalPlan::Lock {
                table_ids,
                mode,
                nowait,
            },
            LogicalPlan::CreateSchema { name } => PhysicalPlan::CreateSchema { name },
            LogicalPlan::DropSchema {
                schema_id,
                name,
                cascade,
            } => PhysicalPlan::DropSchema {
                schema_id,
                name,
                cascade,
            },
            LogicalPlan::PgObjectCommand {
                action,
                kind,
                tag,
                notice,
            } => PhysicalPlan::PgObjectCommand {
                action,
                kind,
                tag,
                notice,
            },
            LogicalPlan::MergeTable(plan) => PhysicalPlan::MergeTable(plan),
            LogicalPlan::CypherQuery(plan) => PhysicalPlan::CypherQuery(Box::new(plan)),
            LogicalPlan::InternalNoOp { tag, notice } => PhysicalPlan::InternalNoOp { tag, notice },
            LogicalPlan::PgCompatUtility { tag, notice } => {
                PhysicalPlan::PgCompatUtility { tag, notice }
            }
            LogicalPlan::Discard { target } => PhysicalPlan::Discard { target },
            LogicalPlan::RecursiveCte {
                base,
                recursive,
                union_all,
                output_fields,
            } => PhysicalPlan::RecursiveCte {
                base: Box::new(self.build(*base)),
                recursive: Box::new(self.build(*recursive)),
                union_all,
                output_fields,
            },
        }
    }

    /// Build a join node from already-optimized physical children.
    ///
    /// This is called from `Optimizer::optimize` so that children have
    /// already gone through access-path selection and their output widths
    /// are correctly known.
    pub fn build_join_from_physical(
        &self,
        phys_left: PhysicalPlan,
        phys_right: PhysicalPlan,
        join_type: JoinType,
        condition: Option<TypedExpr>,
        outputs: Vec<ProjectionExpr>,
        filter: Option<TypedExpr>,
        order_by: Vec<SortExpr>,
        limit: Option<TypedExpr>,
        offset: Option<TypedExpr>,
        distinct: bool,
        distinct_on: Vec<TypedExpr>,
    ) -> PhysicalPlan {
        self.build_join_from_physical_with_exposed_swap(
            phys_left,
            phys_right,
            join_type,
            condition,
            outputs,
            filter,
            order_by,
            limit,
            offset,
            distinct,
            distinct_on,
            ExposedJoinSwapPolicy::Disallow,
            None,
        )
        .0
    }

    pub(crate) fn build_join_from_physical_with_exposed_swap(
        &self,
        phys_left: PhysicalPlan,
        phys_right: PhysicalPlan,
        join_type: JoinType,
        condition: Option<TypedExpr>,
        outputs: Vec<ProjectionExpr>,
        filter: Option<TypedExpr>,
        order_by: Vec<SortExpr>,
        limit: Option<TypedExpr>,
        offset: Option<TypedExpr>,
        distinct: bool,
        distinct_on: Vec<TypedExpr>,
        exposed_swap_policy: ExposedJoinSwapPolicy,
        logical_input_widths: Option<(usize, usize)>,
    ) -> (PhysicalPlan, Option<JoinSwapOrdinalRemap>) {
        let exposes_child_rows = outputs.is_empty();
        let (
            phys_left,
            phys_right,
            condition,
            outputs,
            filter,
            order_by,
            limit,
            offset,
            distinct_on,
            swap_remap,
        ) = maybe_swap_join_inputs_for_cost(
            phys_left,
            phys_right,
            join_type,
            condition,
            outputs,
            filter,
            order_by,
            limit,
            offset,
            distinct_on,
            exposed_swap_policy,
            logical_input_widths,
        );
        let exposed_swap_remap = exposes_child_rows.then_some(swap_remap).flatten();
        let current_logical_input_widths = match (logical_input_widths, swap_remap) {
            (Some((left_width, right_width)), Some(_)) => Some((right_width, left_width)),
            (widths, None) => widths,
            (None, Some(_)) => None,
        };
        let original_condition = condition.clone();
        let original_filter = filter.clone();

        let (left_width, right_width) = join_child_widths(
            &phys_left,
            &phys_right,
            condition.as_ref(),
            filter.as_ref(),
            &outputs,
            &order_by,
            &distinct_on,
            current_logical_input_widths,
        );
        // Try extracting equi-join keys from condition first, then from filter.
        let equi_from_condition =
            extract_equi_join_keys(condition.as_ref(), left_width, right_width);

        let (equi_result, final_condition, final_filter) = if let Some((lk, rk, residual)) =
            equi_from_condition
        {
            (Some((lk, rk)), residual, filter)
        } else {
            // Try extracting equi-keys from filter
            match extract_equi_join_keys(filter.as_ref(), left_width, right_width) {
                Some((lk, rk, residual)) => (Some((lk, rk)), condition, residual),
                None => {
                    // Try combining condition + filter
                    let combined = match (&condition, &filter) {
                        (Some(c), Some(f)) => Some(TypedExpr::logical_and(c.clone(), f.clone())),
                        (Some(c), None) => Some(c.clone()),
                        (None, Some(f)) => Some(f.clone()),
                        (None, None) => None,
                    };
                    match extract_equi_join_keys(combined.as_ref(), left_width, right_width) {
                        Some((lk, rk, residual)) => (Some((lk, rk)), None, residual),
                        None => (None, None, None),
                    }
                }
            }
        };

        if let Some((left_keys, right_keys)) = equi_result {
            let left_rows = estimate_plan_rows(&phys_left);
            let right_rows = estimate_plan_rows(&phys_right);
            let has_order_sensitive_quantified_join =
                contains_quantified_array_comparison(original_condition.as_ref())
                    || contains_quantified_array_comparison(original_filter.as_ref())
                    || contains_quantified_array_comparison(final_condition.as_ref())
                    || contains_quantified_array_comparison(final_filter.as_ref());
            let touches_catalog_project_values = plan_contains_project_values(&phys_left)
                || plan_contains_project_values(&phys_right);
            let inputs_presorted =
                inputs_sorted_on_keys(&phys_left, &left_keys, &phys_right, &right_keys);
            let left_sorted = inputs_presorted
                || sorted_prefix_matches_keys(&plan_sorted_prefix(&phys_left), &left_keys);
            let right_sorted = inputs_presorted
                || sorted_prefix_matches_keys(&plan_sorted_prefix(&phys_right), &right_keys);
            let merge_join_inputs_ready = left_sorted && right_sorted;

            let (nlj_cost, hj_cost, mj_cost) = match join_type {
                JoinType::Semi => (
                    PlanCost::nested_loop_join(left_rows, right_rows),
                    PlanCost::semi_join(left_rows, right_rows),
                    PlanCost::merge_join(left_rows, right_rows, left_sorted, right_sorted),
                ),
                JoinType::Anti => (
                    PlanCost::nested_loop_join(left_rows, right_rows),
                    PlanCost::anti_join(left_rows, right_rows),
                    PlanCost::merge_join(left_rows, right_rows, left_sorted, right_sorted),
                ),
                _ => (
                    PlanCost::nested_loop_join(left_rows, right_rows),
                    PlanCost::hash_join(left_rows, right_rows),
                    PlanCost::merge_join(left_rows, right_rows, left_sorted, right_sorted),
                ),
            };

            if has_order_sensitive_quantified_join || touches_catalog_project_values {
                (
                    PhysicalPlan::NestedLoopJoin {
                        left: Box::new(phys_left),
                        right: Box::new(phys_right),
                        join_type,
                        condition: original_condition,
                        outputs,
                        filter: original_filter,
                        order_by,
                        limit,
                        offset,
                        distinct,
                        distinct_on,
                    },
                    exposed_swap_remap,
                )
            } else if merge_join_inputs_ready
                && mj_cost.cheaper_than(hj_cost)
                && mj_cost.cheaper_than(nlj_cost)
            {
                (
                    PhysicalPlan::MergeJoin {
                        left: Box::new(phys_left),
                        right: Box::new(phys_right),
                        join_type,
                        left_keys,
                        right_keys,
                        residual: final_condition,
                        outputs,
                        filter: final_filter,
                        order_by,
                        limit,
                        offset,
                        distinct,
                        distinct_on,
                    },
                    exposed_swap_remap,
                )
            } else if hj_cost.cheaper_than(nlj_cost) {
                // For hash joins the right side is the build side (fully
                // materialized into a HashMap). Put the smaller input there
                // to minimise memory usage, but only when the join semantics
                // are commutative for this executor path.
                // Don't swap when:
                // - either side has outer column refs (correlated subqueries)
                // - maybe_swap_join_inputs_for_cost already swapped (avoid double-swap)
                // - exposed_swap_policy disallows it (parent depends on column order)
                // Only swap hash join inputs when safe:
                // - No prior swap at this level
                // - No child-exposed swap (would conflict with parent remap)
                // - No outer column refs (correlated subqueries)
                // - Top-level policy (Disallow = no parent depends on our order)
                // - Only for commutative join types (Inner)
                let already_swapped = swap_remap.is_some();
                let child_exposed = exposed_swap_remap.is_some();
                let has_outer_refs =
                    plan_has_outer_refs(&phys_left) || plan_has_outer_refs(&phys_right);
                let at_top_level = matches!(exposed_swap_policy, ExposedJoinSwapPolicy::Disallow);
                // Only swap leaf-to-leaf joins (both sides are scans or
                // simple projections, not nested joins). For multi-level
                // join trees, the child-level swap + maybe_swap already
                // handles optimal placement.
                let left_is_join = matches!(
                    phys_left,
                    PhysicalPlan::NestedLoopJoin { .. }
                        | PhysicalPlan::HashJoin { .. }
                        | PhysicalPlan::MergeJoin { .. }
                );
                let right_is_join = matches!(
                    phys_right,
                    PhysicalPlan::NestedLoopJoin { .. }
                        | PhysicalPlan::HashJoin { .. }
                        | PhysicalPlan::MergeJoin { .. }
                );
                let should_swap_for_hash = left_rows < right_rows
                    && !already_swapped
                    && !child_exposed
                    && !has_outer_refs
                    && at_top_level
                    && !left_is_join
                    && !right_is_join
                    && matches!(join_type, JoinType::Inner);
                let (
                    hj_left,
                    hj_right,
                    hj_left_keys,
                    hj_right_keys,
                    hj_condition,
                    hj_outputs,
                    hj_filter,
                    hj_order_by,
                    hj_limit,
                    hj_offset,
                    hj_distinct_on,
                ) = if should_swap_for_hash {
                    let remap = JoinSwapOrdinalRemap {
                        left_width,
                        right_width,
                    };
                    (
                        phys_right,
                        phys_left,
                        right_keys,
                        left_keys,
                        final_condition.map(|expr| remap_typed_expr_for_join_swap(expr, remap)),
                        outputs
                            .into_iter()
                            .map(|p| remap_projection_expr_for_join_swap(p, remap))
                            .collect(),
                        final_filter.map(|expr| remap_typed_expr_for_join_swap(expr, remap)),
                        order_by
                            .into_iter()
                            .map(|s| remap_sort_expr_for_join_swap(s, remap))
                            .collect(),
                        limit.map(|expr| remap_typed_expr_for_join_swap(expr, remap)),
                        offset.map(|expr| remap_typed_expr_for_join_swap(expr, remap)),
                        distinct_on
                            .into_iter()
                            .map(|expr| remap_typed_expr_for_join_swap(expr, remap))
                            .collect(),
                    )
                } else {
                    (
                        phys_left,
                        phys_right,
                        left_keys,
                        right_keys,
                        final_condition,
                        outputs,
                        final_filter,
                        order_by,
                        limit,
                        offset,
                        distinct_on,
                    )
                };
                (
                    PhysicalPlan::HashJoin {
                        left: Box::new(hj_left),
                        right: Box::new(hj_right),
                        join_type,
                        left_keys: hj_left_keys,
                        right_keys: hj_right_keys,
                        condition: hj_condition,
                        outputs: hj_outputs,
                        filter: hj_filter,
                        order_by: hj_order_by,
                        limit: hj_limit,
                        offset: hj_offset,
                        distinct,
                        distinct_on: hj_distinct_on,
                    },
                    exposed_swap_remap,
                )
            } else {
                (
                    PhysicalPlan::NestedLoopJoin {
                        left: Box::new(phys_left),
                        right: Box::new(phys_right),
                        join_type,
                        condition: original_condition,
                        outputs,
                        filter: original_filter,
                        order_by,
                        limit,
                        offset,
                        distinct,
                        distinct_on,
                    },
                    exposed_swap_remap,
                )
            }
        } else {
            (
                PhysicalPlan::NestedLoopJoin {
                    left: Box::new(phys_left),
                    right: Box::new(phys_right),
                    join_type,
                    condition: original_condition,
                    outputs,
                    filter: original_filter,
                    order_by,
                    limit,
                    offset,
                    distinct,
                    distinct_on,
                },
                exposed_swap_remap,
            )
        }
    }
}

pub(crate) fn maybe_swap_join_inputs_for_cost(
    phys_left: PhysicalPlan,
    phys_right: PhysicalPlan,
    join_type: JoinType,
    condition: Option<TypedExpr>,
    outputs: Vec<ProjectionExpr>,
    filter: Option<TypedExpr>,
    order_by: Vec<SortExpr>,
    limit: Option<TypedExpr>,
    offset: Option<TypedExpr>,
    distinct_on: Vec<TypedExpr>,
    exposed_swap_policy: ExposedJoinSwapPolicy,
    logical_input_widths: Option<(usize, usize)>,
) -> (
    PhysicalPlan,
    PhysicalPlan,
    Option<TypedExpr>,
    Vec<ProjectionExpr>,
    Option<TypedExpr>,
    Vec<SortExpr>,
    Option<TypedExpr>,
    Option<TypedExpr>,
    Vec<TypedExpr>,
    Option<JoinSwapOrdinalRemap>,
) {
    let left_swappable_hybrid = plan_is_swappable_hybrid_source(&phys_left);
    let right_swappable_hybrid = plan_is_swappable_hybrid_source(&phys_right);
    if join_type != JoinType::Inner
        || (outputs.is_empty() && !exposed_swap_policy.allows_empty_outputs())
        || (!left_swappable_hybrid && !right_swappable_hybrid)
    {
        return (
            phys_left,
            phys_right,
            condition,
            outputs,
            filter,
            order_by,
            limit,
            offset,
            distinct_on,
            None,
        );
    }

    if outputs.is_empty()
        && exposed_swap_policy.allows_empty_outputs()
        && !exposed_swap_policy.allows_multi_row_hybrid()
        && (!left_swappable_hybrid || estimate_plan_rows(&phys_left) > 1.0)
        && (!right_swappable_hybrid || estimate_plan_rows(&phys_right) > 1.0)
    {
        return (
            phys_left,
            phys_right,
            condition,
            outputs,
            filter,
            order_by,
            limit,
            offset,
            distinct_on,
            None,
        );
    }

    let (left_width, right_width) = join_child_widths(
        &phys_left,
        &phys_right,
        condition.as_ref(),
        filter.as_ref(),
        &outputs,
        &order_by,
        &distinct_on,
        logical_input_widths,
    );
    let left_rows = estimate_plan_rows(&phys_left);
    let right_rows = estimate_plan_rows(&phys_right);
    let equi_keys =
        extract_join_equi_keys(condition.as_ref(), filter.as_ref(), left_width, right_width);
    let (left_sorted, right_sorted) =
        equi_keys
            .as_ref()
            .map_or((false, false), |(left_keys, right_keys)| {
                (
                    plan_sorted_on_keys(&phys_left, left_keys),
                    plan_sorted_on_keys(&phys_right, right_keys),
                )
            });

    let current_cost = best_join_orientation_cost(
        left_rows,
        right_rows,
        equi_keys.is_some(),
        left_sorted,
        right_sorted,
    );
    let swapped_cost = best_join_orientation_cost(
        right_rows,
        left_rows,
        equi_keys.is_some(),
        right_sorted,
        left_sorted,
    );
    if !swapped_cost.cheaper_than(current_cost) {
        return (
            phys_left,
            phys_right,
            condition,
            outputs,
            filter,
            order_by,
            limit,
            offset,
            distinct_on,
            None,
        );
    }

    let remap = JoinSwapOrdinalRemap {
        left_width,
        right_width,
    };
    (
        phys_right,
        phys_left,
        condition.map(|expr| remap_typed_expr_for_join_swap(expr, remap)),
        outputs
            .into_iter()
            .map(|projection| remap_projection_expr_for_join_swap(projection, remap))
            .collect(),
        filter.map(|expr| remap_typed_expr_for_join_swap(expr, remap)),
        order_by
            .into_iter()
            .map(|sort| remap_sort_expr_for_join_swap(sort, remap))
            .collect(),
        limit.map(|expr| remap_typed_expr_for_join_swap(expr, remap)),
        offset.map(|expr| remap_typed_expr_for_join_swap(expr, remap)),
        distinct_on
            .into_iter()
            .map(|expr| remap_typed_expr_for_join_swap(expr, remap))
            .collect(),
        Some(remap),
    )
}

include!("physical_builder_join_swap_support.rs");
