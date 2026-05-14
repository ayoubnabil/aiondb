//! Replace scans of synthetic relations (`pg_catalog`, `information_schema`)
//! with concrete `ProjectValues` sources before the optimizer sees them.
//!
//! Up-stream binding emits regular `SeqScan { table_id }` / `ProjectTable`
//! / `Aggregate` nodes that pretend the synthetic tables are real, so the
//! type checker can resolve columns uniformly. This pass walks the bound
//! plan and, for any `table_id` that resolves to a synthetic source via
//! [`pg_catalog::table_name_for_synthetic_id`] or
//! [`information_schema::table_name_for_synthetic_id`], substitutes a
//! materialised `ProjectValues` produced from the catalog. Real user tables
//! pass through unchanged.
//!
//! The depth guards exist because plans and expressions can be deeply
//! nested (recursive CTEs, large IN-lists), so we cap recursion to avoid
//! blowing the host stack on adversarial input.

use std::cell::Cell;
use std::sync::Arc;

use aiondb_catalog::CatalogReader;
use aiondb_core::{DataType, DbError, DbResult, RelationId, TidValue, TxnId, Value};
use aiondb_plan::{
    InsertOnConflict, LogicalPlan, OnConflictActionPlan, ProjectionExpr, SortExpr, TypedExpr,
    TypedExprKind, UpdateAssignment,
};

use crate::{information_schema, pg_catalog};

/// PostgreSQL TIDs are `(block, offset)` pairs with offset starting at 1.
/// We pack 10 synthetic rows per virtual "page" so generated TIDs look
/// plausibly compact to clients comparing them with `ctid` ordering.
const COMPAT_TID_PAGE_WIDTH: u64 = 10;
const MAX_VIRTUAL_SCAN_REWRITE_PLAN_DEPTH: usize = 512;
const MAX_VIRTUAL_SCAN_REWRITE_EXPR_DEPTH: usize = 1024;

pub(crate) fn rewrite(
    catalog: &Arc<dyn CatalogReader>,
    plan: LogicalPlan,
    txn_id: TxnId,
    default_schema: Option<&str>,
    session_user: Option<&str>,
    database_name: Option<&str>,
) -> DbResult<LogicalPlan> {
    let rewriter = VirtualScanRewriter {
        catalog,
        txn_id,
        default_schema,
        session_user,
        database_name,
        plan_depth: Cell::new(0),
        expr_depth: Cell::new(0),
    };
    rewriter.rewrite_plan(plan)
}

struct VirtualScanRewriter<'a> {
    catalog: &'a Arc<dyn CatalogReader>,
    txn_id: TxnId,
    default_schema: Option<&'a str>,
    session_user: Option<&'a str>,
    database_name: Option<&'a str>,
    plan_depth: Cell<usize>,
    expr_depth: Cell<usize>,
}

struct RewriteDepthGuard<'a> {
    depth: &'a Cell<usize>,
}

impl Drop for RewriteDepthGuard<'_> {
    fn drop(&mut self) {
        self.depth.set(self.depth.get().saturating_sub(1));
    }
}

impl VirtualScanRewriter<'_> {
    fn enter_plan_rewrite(&self) -> DbResult<RewriteDepthGuard<'_>> {
        let current = self.plan_depth.get();
        if current >= MAX_VIRTUAL_SCAN_REWRITE_PLAN_DEPTH {
            return Err(DbError::program_limit(format!(
                "virtual scan rewrite plan depth exceeds limit {MAX_VIRTUAL_SCAN_REWRITE_PLAN_DEPTH}"
            )));
        }
        self.plan_depth.set(current + 1);
        Ok(RewriteDepthGuard {
            depth: &self.plan_depth,
        })
    }

    fn enter_expr_rewrite(&self) -> DbResult<RewriteDepthGuard<'_>> {
        let current = self.expr_depth.get();
        if current >= MAX_VIRTUAL_SCAN_REWRITE_EXPR_DEPTH {
            return Err(DbError::program_limit(format!(
                "virtual scan rewrite expression depth exceeds limit {MAX_VIRTUAL_SCAN_REWRITE_EXPR_DEPTH}"
            )));
        }
        self.expr_depth.set(current + 1);
        Ok(RewriteDepthGuard {
            depth: &self.expr_depth,
        })
    }

    fn rewrite_plan(&self, plan: LogicalPlan) -> DbResult<LogicalPlan> {
        let _guard = self.enter_plan_rewrite()?;
        match plan {
            LogicalPlan::ProjectOnce {
                outputs,
                filter,
                order_by,
                limit,
                offset,
                distinct,
                distinct_on,
            } => Ok(LogicalPlan::ProjectOnce {
                outputs: self.rewrite_projection_exprs(outputs)?,
                filter: self.rewrite_optional_expr(filter)?,
                order_by: self.rewrite_sort_exprs(order_by)?,
                limit: self.rewrite_optional_expr(limit)?,
                offset: self.rewrite_optional_expr(offset)?,
                distinct,
                distinct_on: self.rewrite_exprs(distinct_on)?,
            }),
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
                let outputs = self.rewrite_projection_exprs(outputs)?;
                let filter = self.rewrite_optional_expr(filter)?;
                let order_by = self.rewrite_sort_exprs(order_by)?;
                let limit = self.rewrite_optional_expr(limit)?;
                let offset = self.rewrite_optional_expr(offset)?;
                let distinct_on = self.rewrite_exprs(distinct_on)?;
                if let Some(source) = self.virtual_source(table_id)? {
                    Ok(LogicalPlan::ProjectSource {
                        source: Box::new(source),
                        outputs,
                        filter,
                        order_by,
                        limit,
                        offset,
                        distinct,
                        distinct_on,
                    })
                } else {
                    Ok(LogicalPlan::ProjectTable {
                        table_id,
                        outputs,
                        filter,
                        order_by,
                        limit,
                        offset,
                        distinct,
                        distinct_on,
                    })
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
            } => Ok(LogicalPlan::ProjectSource {
                source: Box::new(self.rewrite_plan(*source)?),
                outputs: self.rewrite_projection_exprs(outputs)?,
                filter: self.rewrite_optional_expr(filter)?,
                order_by: self.rewrite_sort_exprs(order_by)?,
                limit: self.rewrite_optional_expr(limit)?,
                offset: self.rewrite_optional_expr(offset)?,
                distinct,
                distinct_on: self.rewrite_exprs(distinct_on)?,
            }),
            LogicalPlan::SeqScan { table_id } => self
                .virtual_source(table_id)?
                .map_or(Ok(LogicalPlan::SeqScan { table_id }), Ok),
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
            } => {
                let group_by = self.rewrite_exprs(group_by)?;
                let aggregates = self.rewrite_projection_exprs(aggregates)?;
                let having = self.rewrite_optional_expr(having)?;
                let filter = self.rewrite_optional_expr(filter)?;
                let order_by = self.rewrite_sort_exprs(order_by)?;
                let limit = self.rewrite_optional_expr(limit)?;
                let offset = self.rewrite_optional_expr(offset)?;
                let distinct_on = self.rewrite_exprs(distinct_on)?;
                if let Some(source) = self.virtual_source(table_id)? {
                    Ok(LogicalPlan::AggregateSource {
                        source: Box::new(source),
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
                    })
                } else {
                    Ok(LogicalPlan::Aggregate {
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
                    })
                }
            }
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
            } => Ok(LogicalPlan::AggregateSource {
                source: Box::new(self.rewrite_plan(*source)?),
                group_by: self.rewrite_exprs(group_by)?,
                grouping_sets,
                aggregates: self.rewrite_projection_exprs(aggregates)?,
                having: self.rewrite_optional_expr(having)?,
                filter: self.rewrite_optional_expr(filter)?,
                order_by: self.rewrite_sort_exprs(order_by)?,
                limit: self.rewrite_optional_expr(limit)?,
                offset: self.rewrite_optional_expr(offset)?,
                distinct,
                distinct_on: self.rewrite_exprs(distinct_on)?,
            }),
            LogicalPlan::NestedLoopJoin { .. } => self.rewrite_nested_loop_join_plan(plan),
            LogicalPlan::SetOperation {
                op,
                all,
                left,
                right,
                output_fields,
                order_by,
                limit,
                offset,
            } => Ok(LogicalPlan::SetOperation {
                op,
                all,
                left: Box::new(self.rewrite_plan(*left)?),
                right: Box::new(self.rewrite_plan(*right)?),
                output_fields,
                order_by: self.rewrite_sort_exprs(order_by)?,
                limit: self.rewrite_optional_expr(limit)?,
                offset: self.rewrite_optional_expr(offset)?,
            }),
            LogicalPlan::ProjectValues {
                output_fields,
                rows,
                order_by,
                limit,
                offset,
            } => Ok(LogicalPlan::ProjectValues {
                output_fields,
                rows: rows
                    .into_iter()
                    .map(|row| self.rewrite_exprs(row))
                    .collect::<DbResult<Vec<_>>>()?,
                order_by: self.rewrite_sort_exprs(order_by)?,
                limit: self.rewrite_optional_expr(limit)?,
                offset: self.rewrite_optional_expr(offset)?,
            }),
            LogicalPlan::CreateTableAs {
                relation_name,
                columns,
                with_no_data,
                source,
            } => Ok(LogicalPlan::CreateTableAs {
                relation_name,
                columns,
                with_no_data,
                source: Box::new(self.rewrite_plan(*source)?),
            }),
            LogicalPlan::InsertValues {
                table_id,
                columns,
                rows,
                on_conflict,
                returning,
            } => Ok(LogicalPlan::InsertValues {
                table_id,
                columns,
                rows: rows
                    .into_iter()
                    .map(|row| self.rewrite_exprs(row))
                    .collect::<DbResult<Vec<_>>>()?,
                on_conflict: self.rewrite_on_conflict(on_conflict)?,
                returning: self.rewrite_projection_exprs(returning)?,
            }),
            LogicalPlan::InsertSelect {
                table_id,
                columns,
                assignments,
                source,
                on_conflict,
                returning,
            } => Ok(LogicalPlan::InsertSelect {
                table_id,
                columns,
                assignments: self.rewrite_exprs(assignments)?,
                source: Box::new(self.rewrite_plan(*source)?),
                on_conflict: self.rewrite_on_conflict(on_conflict)?,
                returning: self.rewrite_projection_exprs(returning)?,
            }),
            LogicalPlan::DeleteFromTable {
                table_id,
                filter,
                returning,
                using_table_ids,
            } => Ok(LogicalPlan::DeleteFromTable {
                table_id,
                filter: self.rewrite_optional_expr(filter)?,
                returning: self.rewrite_projection_exprs(returning)?,
                using_table_ids,
            }),
            LogicalPlan::UpdateTable {
                table_id,
                assignments,
                filter,
                returning,
                from_table_ids,
            } => Ok(LogicalPlan::UpdateTable {
                table_id,
                assignments: self.rewrite_assignments(assignments)?,
                filter: self.rewrite_optional_expr(filter)?,
                returning: self.rewrite_projection_exprs(returning)?,
                from_table_ids,
            }),
            LogicalPlan::MergeTable(mut merge) => {
                merge.on_condition = self.rewrite_expr(&merge.on_condition)?;
                for when_clause in &mut merge.when_clauses {
                    when_clause.condition = when_clause
                        .condition
                        .take()
                        .map(|expr| self.rewrite_expr(&expr))
                        .transpose()?;
                    match &mut when_clause.action {
                        aiondb_plan::MergeActionPlan::Update { assignments } => {
                            let taken = std::mem::take(assignments);
                            *assignments = self.rewrite_assignments(taken)?;
                        }
                        aiondb_plan::MergeActionPlan::Insert { values } => {
                            let taken = std::mem::take(values);
                            *values = self.rewrite_exprs(taken)?;
                        }
                        aiondb_plan::MergeActionPlan::Delete
                        | aiondb_plan::MergeActionPlan::InsertDefaultValues
                        | aiondb_plan::MergeActionPlan::DoNothing => {}
                    }
                }
                Ok(LogicalPlan::MergeTable(merge))
            }
            LogicalPlan::RecursiveCte {
                base,
                recursive,
                union_all,
                output_fields,
            } => Ok(LogicalPlan::RecursiveCte {
                base: Box::new(self.rewrite_plan(*base)?),
                recursive: Box::new(self.rewrite_plan(*recursive)?),
                union_all,
                output_fields,
            }),
            other => Ok(other),
        }
    }

    fn rewrite_nested_loop_join_plan(&self, mut plan: LogicalPlan) -> DbResult<LogicalPlan> {
        struct PendingJoin {
            right: LogicalPlan,
            join_type: aiondb_plan::JoinType,
            condition: Option<TypedExpr>,
            outputs: Vec<ProjectionExpr>,
            filter: Option<TypedExpr>,
            order_by: Vec<SortExpr>,
            limit: Option<TypedExpr>,
            offset: Option<TypedExpr>,
            distinct: bool,
            distinct_on: Vec<TypedExpr>,
        }

        let mut spine: Vec<PendingJoin> = Vec::new();

        loop {
            match plan {
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
                    let right = self.rewrite_plan(*right)?;
                    let condition = self.rewrite_optional_expr(condition)?;
                    let outputs = self.rewrite_projection_exprs(outputs)?;
                    let filter = self.rewrite_optional_expr(filter)?;
                    let order_by = self.rewrite_sort_exprs(order_by)?;
                    let limit = self.rewrite_optional_expr(limit)?;
                    let offset = self.rewrite_optional_expr(offset)?;
                    let distinct_on = self.rewrite_exprs(distinct_on)?;
                    spine.push(PendingJoin {
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
                    plan = *left;
                }
                other => {
                    let mut rewritten = self.rewrite_plan(other)?;
                    while let Some(pending) = spine.pop() {
                        rewritten = LogicalPlan::NestedLoopJoin {
                            left: Box::new(rewritten),
                            right: Box::new(pending.right),
                            join_type: pending.join_type,
                            condition: pending.condition,
                            outputs: pending.outputs,
                            filter: pending.filter,
                            order_by: pending.order_by,
                            limit: pending.limit,
                            offset: pending.offset,
                            distinct: pending.distinct,
                            distinct_on: pending.distinct_on,
                        };
                    }
                    return Ok(rewritten);
                }
            }
        }
    }

    fn rewrite_projection_exprs(
        &self,
        outputs: Vec<ProjectionExpr>,
    ) -> DbResult<Vec<ProjectionExpr>> {
        outputs
            .into_iter()
            .map(|output| {
                Ok(ProjectionExpr {
                    field: output.field,
                    expr: self.rewrite_expr(&output.expr)?,
                })
            })
            .collect()
    }

    fn rewrite_sort_exprs(&self, order_by: Vec<SortExpr>) -> DbResult<Vec<SortExpr>> {
        order_by
            .into_iter()
            .map(|sort| {
                Ok(SortExpr {
                    expr: self.rewrite_expr(&sort.expr)?,
                    descending: sort.descending,
                    nulls_first: sort.nulls_first,
                })
            })
            .collect()
    }

    fn rewrite_optional_expr(&self, expr: Option<TypedExpr>) -> DbResult<Option<TypedExpr>> {
        expr.map(|expr| self.rewrite_expr(&expr)).transpose()
    }

    fn rewrite_exprs(&self, exprs: Vec<TypedExpr>) -> DbResult<Vec<TypedExpr>> {
        exprs
            .into_iter()
            .map(|expr| self.rewrite_expr(&expr))
            .collect()
    }

    fn rewrite_assignments(
        &self,
        assignments: Vec<UpdateAssignment>,
    ) -> DbResult<Vec<UpdateAssignment>> {
        assignments
            .into_iter()
            .map(|assignment| {
                Ok(UpdateAssignment {
                    column_ordinal: assignment.column_ordinal,
                    data_type: assignment.data_type,
                    nullable: assignment.nullable,
                    expr: self.rewrite_expr(&assignment.expr)?,
                })
            })
            .collect()
    }

    fn rewrite_on_conflict(
        &self,
        on_conflict: Option<InsertOnConflict>,
    ) -> DbResult<Option<InsertOnConflict>> {
        on_conflict
            .map(|on_conflict| {
                let action = match on_conflict.action {
                    OnConflictActionPlan::DoNothing => OnConflictActionPlan::DoNothing,
                    OnConflictActionPlan::DoUpdate {
                        assignments,
                        where_clause,
                    } => OnConflictActionPlan::DoUpdate {
                        assignments: self.rewrite_assignments(assignments)?,
                        where_clause: self.rewrite_optional_expr(where_clause)?,
                    },
                };
                Ok(InsertOnConflict {
                    columns: on_conflict.columns,
                    action,
                })
            })
            .transpose()
    }

    fn rewrite_expr(&self, expr: &TypedExpr) -> DbResult<TypedExpr> {
        let _guard = self.enter_expr_rewrite()?;
        macro_rules! rewrite_box {
            ($inner:expr) => {
                Box::new(self.rewrite_expr($inner)?)
            };
        }

        macro_rules! rewrite_vec {
            ($exprs:expr) => {
                $exprs
                    .iter()
                    .map(|inner| self.rewrite_expr(inner))
                    .collect::<DbResult<Vec<_>>>()?
            };
        }

        macro_rules! rewrite_optional_box {
            ($expr:expr) => {
                $expr
                    .as_ref()
                    .map(|inner| self.rewrite_expr(inner))
                    .transpose()?
                    .map(Box::new)
            };
        }

        let kind = match &expr.kind {
            TypedExprKind::Cast {
                expr: inner,
                target_type,
            } => TypedExprKind::Cast {
                expr: rewrite_box!(inner),
                target_type: target_type.clone(),
            },
            TypedExprKind::Negate { expr: inner } => TypedExprKind::Negate {
                expr: rewrite_box!(inner),
            },
            TypedExprKind::LogicalNot { expr: inner } => TypedExprKind::LogicalNot {
                expr: rewrite_box!(inner),
            },
            TypedExprKind::IsNull {
                expr: inner,
                negated,
            } => TypedExprKind::IsNull {
                expr: rewrite_box!(inner),
                negated: *negated,
            },
            TypedExprKind::ArithAdd { left, right } => TypedExprKind::ArithAdd {
                left: rewrite_box!(left),
                right: rewrite_box!(right),
            },
            TypedExprKind::ArithSub { left, right } => TypedExprKind::ArithSub {
                left: rewrite_box!(left),
                right: rewrite_box!(right),
            },
            TypedExprKind::ArithMul { left, right } => TypedExprKind::ArithMul {
                left: rewrite_box!(left),
                right: rewrite_box!(right),
            },
            TypedExprKind::ArithDiv { left, right } => TypedExprKind::ArithDiv {
                left: rewrite_box!(left),
                right: rewrite_box!(right),
            },
            TypedExprKind::ArithMod { left, right } => TypedExprKind::ArithMod {
                left: rewrite_box!(left),
                right: rewrite_box!(right),
            },
            TypedExprKind::BinaryEq { left, right } => TypedExprKind::BinaryEq {
                left: rewrite_box!(left),
                right: rewrite_box!(right),
            },
            TypedExprKind::BinaryNe { left, right } => TypedExprKind::BinaryNe {
                left: rewrite_box!(left),
                right: rewrite_box!(right),
            },
            TypedExprKind::BinaryLt { left, right } => TypedExprKind::BinaryLt {
                left: rewrite_box!(left),
                right: rewrite_box!(right),
            },
            TypedExprKind::BinaryLe { left, right } => TypedExprKind::BinaryLe {
                left: rewrite_box!(left),
                right: rewrite_box!(right),
            },
            TypedExprKind::BinaryGt { left, right } => TypedExprKind::BinaryGt {
                left: rewrite_box!(left),
                right: rewrite_box!(right),
            },
            TypedExprKind::BinaryGe { left, right } => TypedExprKind::BinaryGe {
                left: rewrite_box!(left),
                right: rewrite_box!(right),
            },
            TypedExprKind::LogicalAnd { left, right } => TypedExprKind::LogicalAnd {
                left: rewrite_box!(left),
                right: rewrite_box!(right),
            },
            TypedExprKind::LogicalOr { left, right } => TypedExprKind::LogicalOr {
                left: rewrite_box!(left),
                right: rewrite_box!(right),
            },
            TypedExprKind::Concat { left, right } => TypedExprKind::Concat {
                left: rewrite_box!(left),
                right: rewrite_box!(right),
            },
            TypedExprKind::JsonGet { left, right } => TypedExprKind::JsonGet {
                left: rewrite_box!(left),
                right: rewrite_box!(right),
            },
            TypedExprKind::JsonGetText { left, right } => TypedExprKind::JsonGetText {
                left: rewrite_box!(left),
                right: rewrite_box!(right),
            },
            TypedExprKind::JsonContains { left, right } => TypedExprKind::JsonContains {
                left: rewrite_box!(left),
                right: rewrite_box!(right),
            },
            TypedExprKind::JsonContainedBy { left, right } => TypedExprKind::JsonContainedBy {
                left: rewrite_box!(left),
                right: rewrite_box!(right),
            },
            TypedExprKind::JsonKeyExists { left, right } => TypedExprKind::JsonKeyExists {
                left: rewrite_box!(left),
                right: rewrite_box!(right),
            },
            TypedExprKind::JsonAnyKeyExists { left, right } => TypedExprKind::JsonAnyKeyExists {
                left: rewrite_box!(left),
                right: rewrite_box!(right),
            },
            TypedExprKind::JsonAllKeysExist { left, right } => TypedExprKind::JsonAllKeysExist {
                left: rewrite_box!(left),
                right: rewrite_box!(right),
            },
            TypedExprKind::ArrayConcat { left, right } => TypedExprKind::ArrayConcat {
                left: rewrite_box!(left),
                right: rewrite_box!(right),
            },
            TypedExprKind::ArrayContains { left, right } => TypedExprKind::ArrayContains {
                left: rewrite_box!(left),
                right: rewrite_box!(right),
            },
            TypedExprKind::ArrayContainedBy { left, right } => TypedExprKind::ArrayContainedBy {
                left: rewrite_box!(left),
                right: rewrite_box!(right),
            },
            TypedExprKind::ArrayOverlap { left, right } => TypedExprKind::ArrayOverlap {
                left: rewrite_box!(left),
                right: rewrite_box!(right),
            },
            TypedExprKind::Nullif { left, right } => TypedExprKind::Nullif {
                left: rewrite_box!(left),
                right: rewrite_box!(right),
            },
            TypedExprKind::CaseWhen {
                conditions,
                results,
                else_result,
            } => TypedExprKind::CaseWhen {
                conditions: rewrite_vec!(conditions),
                results: rewrite_vec!(results),
                else_result: rewrite_optional_box!(else_result),
            },
            TypedExprKind::Coalesce { args } => TypedExprKind::Coalesce {
                args: rewrite_vec!(args),
            },
            TypedExprKind::InList {
                expr: inner,
                list,
                negated,
            } => TypedExprKind::InList {
                expr: rewrite_box!(inner),
                list: rewrite_vec!(list),
                negated: *negated,
            },
            TypedExprKind::Between {
                expr: inner,
                low,
                high,
                negated,
            } => TypedExprKind::Between {
                expr: rewrite_box!(inner),
                low: rewrite_box!(low),
                high: rewrite_box!(high),
                negated: *negated,
            },
            TypedExprKind::Like {
                expr: inner,
                pattern,
                negated,
                case_insensitive,
            } => TypedExprKind::Like {
                expr: rewrite_box!(inner),
                pattern: rewrite_box!(pattern),
                negated: *negated,
                case_insensitive: *case_insensitive,
            },
            TypedExprKind::IsDistinctFrom {
                left,
                right,
                negated,
            } => TypedExprKind::IsDistinctFrom {
                left: rewrite_box!(left),
                right: rewrite_box!(right),
                negated: *negated,
            },
            TypedExprKind::ScalarFunction { func, args } => TypedExprKind::ScalarFunction {
                func: func.clone(),
                args: rewrite_vec!(args),
            },
            TypedExprKind::ArrayConstruct { elements } => TypedExprKind::ArrayConstruct {
                elements: rewrite_vec!(elements),
            },
            TypedExprKind::UserFunction {
                name,
                args,
                body,
                params,
                language,
            } => TypedExprKind::UserFunction {
                name: name.clone(),
                args: rewrite_vec!(args),
                body: body.clone(),
                params: params.clone(),
                language: language.clone(),
            },
            TypedExprKind::AggCount {
                expr: inner,
                distinct,
                filter,
            } => TypedExprKind::AggCount {
                expr: rewrite_optional_box!(inner),
                distinct: *distinct,
                filter: rewrite_optional_box!(filter),
            },
            TypedExprKind::AggSum {
                expr: inner,
                distinct,
                filter,
            } => TypedExprKind::AggSum {
                expr: rewrite_box!(inner),
                distinct: *distinct,
                filter: rewrite_optional_box!(filter),
            },
            TypedExprKind::AggAvg {
                expr: inner,
                distinct,
                filter,
            } => TypedExprKind::AggAvg {
                expr: rewrite_box!(inner),
                distinct: *distinct,
                filter: rewrite_optional_box!(filter),
            },
            TypedExprKind::AggAnyValue {
                expr: inner,
                filter,
            } => TypedExprKind::AggAnyValue {
                expr: rewrite_box!(inner),
                filter: rewrite_optional_box!(filter),
            },
            TypedExprKind::AggMin {
                expr: inner,
                filter,
            } => TypedExprKind::AggMin {
                expr: rewrite_box!(inner),
                filter: rewrite_optional_box!(filter),
            },
            TypedExprKind::AggMax {
                expr: inner,
                filter,
            } => TypedExprKind::AggMax {
                expr: rewrite_box!(inner),
                filter: rewrite_optional_box!(filter),
            },
            TypedExprKind::AggStringAgg {
                expr: inner,
                delimiter,
                distinct,
                filter,
            } => TypedExprKind::AggStringAgg {
                expr: rewrite_box!(inner),
                delimiter: rewrite_box!(delimiter),
                distinct: *distinct,
                filter: rewrite_optional_box!(filter),
            },
            TypedExprKind::AggArrayAgg {
                expr: inner,
                distinct,
                filter,
                order_descending,
            } => TypedExprKind::AggArrayAgg {
                expr: rewrite_box!(inner),
                distinct: *distinct,
                filter: rewrite_optional_box!(filter),
                order_descending: *order_descending,
            },
            TypedExprKind::AggBoolAnd {
                expr: inner,
                filter,
            } => TypedExprKind::AggBoolAnd {
                expr: rewrite_box!(inner),
                filter: rewrite_optional_box!(filter),
            },
            TypedExprKind::AggBoolOr {
                expr: inner,
                filter,
            } => TypedExprKind::AggBoolOr {
                expr: rewrite_box!(inner),
                filter: rewrite_optional_box!(filter),
            },
            TypedExprKind::AggStddevPop {
                expr: inner,
                filter,
            } => TypedExprKind::AggStddevPop {
                expr: rewrite_box!(inner),
                filter: rewrite_optional_box!(filter),
            },
            TypedExprKind::AggStddevSamp {
                expr: inner,
                filter,
            } => TypedExprKind::AggStddevSamp {
                expr: rewrite_box!(inner),
                filter: rewrite_optional_box!(filter),
            },
            TypedExprKind::AggVarPop {
                expr: inner,
                filter,
            } => TypedExprKind::AggVarPop {
                expr: rewrite_box!(inner),
                filter: rewrite_optional_box!(filter),
            },
            TypedExprKind::AggVarSamp {
                expr: inner,
                filter,
            } => TypedExprKind::AggVarSamp {
                expr: rewrite_box!(inner),
                filter: rewrite_optional_box!(filter),
            },
            TypedExprKind::ScalarSubquery { plan } => TypedExprKind::ScalarSubquery {
                plan: Box::new(self.rewrite_plan((**plan).clone())?),
            },
            TypedExprKind::ArraySubquery { plan } => TypedExprKind::ArraySubquery {
                plan: Box::new(self.rewrite_plan((**plan).clone())?),
            },
            TypedExprKind::InSubquery {
                expr: inner,
                plan,
                negated,
            } => TypedExprKind::InSubquery {
                expr: rewrite_box!(inner),
                plan: Box::new(self.rewrite_plan((**plan).clone())?),
                negated: *negated,
            },
            TypedExprKind::ExistsSubquery { plan, negated } => TypedExprKind::ExistsSubquery {
                plan: Box::new(self.rewrite_plan((**plan).clone())?),
                negated: *negated,
            },
            TypedExprKind::WindowFunction {
                func,
                args,
                partition_by,
                order_by,
            } => TypedExprKind::WindowFunction {
                func: func.clone(),
                args: rewrite_vec!(args),
                partition_by: rewrite_vec!(partition_by),
                order_by: self.rewrite_sort_exprs(order_by.clone())?,
            },
            other => other.clone(),
        };

        Ok(TypedExpr {
            kind,
            data_type: expr.data_type.clone(),
            nullable: expr.nullable,
        })
    }

    fn virtual_source(&self, table_id: RelationId) -> DbResult<Option<LogicalPlan>> {
        if let Some(table_name) = pg_catalog::table_name_for_synthetic_id(table_id.get()) {
            let Some(plan) = pg_catalog::build_plan(
                self.catalog,
                self.txn_id,
                table_name,
                self.default_schema,
                self.session_user,
                self.database_name,
            )?
            else {
                return Ok(None);
            };
            return Ok(Some(augment_virtual_project_values(plan)));
        }

        if let Some(table_name) = information_schema::table_name_for_synthetic_id(table_id.get()) {
            let Some(plan) = information_schema::build_plan(
                self.catalog,
                self.txn_id,
                table_name,
                self.default_schema,
                self.database_name,
            )?
            else {
                return Ok(None);
            };
            return Ok(Some(augment_virtual_project_values(plan)));
        }

        Ok(None)
    }
}

fn augment_virtual_project_values(plan: LogicalPlan) -> LogicalPlan {
    let LogicalPlan::ProjectValues {
        output_fields,
        rows,
        order_by,
        limit,
        offset,
    } = plan
    else {
        return plan;
    };

    let existing_names = output_fields
        .iter()
        .map(|field| field.name.to_ascii_lowercase())
        .collect::<Vec<_>>();
    let extra_columns = [
        ("ctid", DataType::Tid, false),
        ("tableoid", DataType::Int, true),
        ("xmin", DataType::Int, true),
        ("xmax", DataType::Int, true),
        ("cmin", DataType::Int, true),
        ("cmax", DataType::Int, true),
        ("oid", DataType::Int, true),
    ]
    .into_iter()
    .filter(|(name, _, _)| !existing_names.iter().any(|existing| existing == name))
    .collect::<Vec<_>>();

    if extra_columns.is_empty() {
        return LogicalPlan::ProjectValues {
            output_fields,
            rows,
            order_by,
            limit,
            offset,
        };
    }

    let mut output_fields = output_fields;
    output_fields.extend(extra_columns.iter().map(|(name, data_type, nullable)| {
        aiondb_plan::ResultField {
            name: (*name).to_owned(),
            data_type: data_type.clone(),
            text_type_modifier: None,
            nullable: *nullable,
        }
    }));

    let rows = rows
        .into_iter()
        .enumerate()
        .map(|(index, mut row)| {
            let row_number = u64::try_from(index).unwrap_or(u64::MAX).saturating_add(1);
            let row_tid = compat_tid_for_virtual_row(row_number);
            row.extend(extra_columns.iter().map(|(name, data_type, nullable)| {
                let value = if *name == "ctid" {
                    Value::Tid(row_tid)
                } else {
                    Value::Null
                };
                TypedExpr::literal(value, data_type.clone(), *nullable)
            }));
            row
        })
        .collect();

    LogicalPlan::ProjectValues {
        output_fields,
        rows,
        order_by,
        limit,
        offset,
    }
}

fn compat_tid_for_virtual_row(row_number: u64) -> TidValue {
    let zero_based = row_number.saturating_sub(1);
    let block = u32::try_from(zero_based / COMPAT_TID_PAGE_WIDTH).unwrap_or(u32::MAX);
    let offset =
        u16::try_from((zero_based % COMPAT_TID_PAGE_WIDTH).saturating_add(1)).unwrap_or(u16::MAX);
    TidValue::new(block, offset)
}
