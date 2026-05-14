#![allow(
    clippy::doc_markdown,
    clippy::map_unwrap_or,
    clippy::match_same_arms,
    clippy::needless_pass_by_value,
    clippy::too_many_lines,
    clippy::wildcard_imports
)]

use super::*;
use crate::engine::compat::router_helpers::CompatHandlerPlan;
use crate::prepared::{portal_batch_copy_in_tag, PORTAL_BATCH_COPY_OUT_TAG};
use aiondb_core::{Row, Value};

#[derive(Clone, Copy, Debug, Default)]
pub(super) struct PortalCompatHints {
    pub uses_command_hooks: bool,
    pub uses_rule_dml: bool,
    pub may_use_drop_if_exists_notice: bool,
}

use super::query_api::parser_expr_strip_casts;

fn parser_literal_to_value(literal: &aiondb_parser::Literal) -> Option<Value> {
    match literal {
        aiondb_parser::Literal::Integer(value) => {
            if let Ok(value) = i32::try_from(*value) {
                Some(Value::Int(value))
            } else {
                Some(Value::BigInt(*value))
            }
        }
        aiondb_parser::Literal::String(value) => Some(Value::Text(value.clone())),
        aiondb_parser::Literal::Boolean(value) => Some(Value::Boolean(*value)),
        aiondb_parser::Literal::Null => Some(Value::Null),
        aiondb_parser::Literal::NumericLit(_) => None,
    }
}

/// Pick a literal `Value` from either operand of a binary operator (the side
/// that is a `Literal` after stripping casts). Returns `None` when neither
/// side is a recognised literal, or when the literal kind isn't representable
/// (e.g. `NumericLit`).
fn parser_binop_either_literal_value(
    left: &aiondb_parser::Expr,
    right: &aiondb_parser::Expr,
) -> Option<Value> {
    let left = parser_expr_strip_casts(left);
    let right = parser_expr_strip_casts(right);
    match (left, right) {
        (aiondb_parser::Expr::Literal(literal, _), _) => parser_literal_to_value(literal),
        (_, aiondb_parser::Expr::Literal(literal, _)) => parser_literal_to_value(literal),
        _ => None,
    }
}

fn extract_bound_statement_eq_literal(statement: &Statement) -> Option<Value> {
    let selection = match statement {
        Statement::Select(select) => select.selection.as_ref()?,
        // DELETE FROM t WHERE col = ?: same shape, lets the param
        // literal substituted by `bind_statement_params` re-route the
        // cached \`DeleteFromTable\` plan to the index_eq fast path.
        // RETURNING is irrelevant for the rewrite; the filter
        // substitution doesn't touch the returning projection.
        Statement::Delete(delete) => delete.selection.as_ref()?,
        // UPDATE t SET col = expr WHERE col = ?: even when the
        // assignment is computed (not a substitutable literal), we
        // still want to substitute the WHERE clause literal so the
        // cached UpdateTable plan's filter loses its Param and the
        // \`extract_dml_simple_eq_literal_filter\` fast path kicks in.
        // \`from_tables\` non-empty (UPDATE ... FROM) keeps its own
        // path; too many cross-row interactions for a literal swap
        // to be safe.
        Statement::Update(update) if update.from_tables.is_empty() => update.selection.as_ref()?,
        _ => return None,
    };
    let selection = parser_expr_strip_casts(selection);
    let aiondb_parser::Expr::BinaryOp {
        left, op, right, ..
    } = selection
    else {
        return None;
    };
    if *op != aiondb_parser::BinaryOperator::Eq {
        return None;
    }
    parser_binop_either_literal_value(left, right)
}

fn extract_bound_update_literals(statement: &Statement) -> Option<(Value, Value)> {
    let Statement::Update(update) = statement else {
        return None;
    };
    if !update.from_tables.is_empty() || update.assignments.len() != 1 {
        return None;
    }
    let assignment = &update.assignments[0];
    let aiondb_parser::Expr::BinaryOp {
        left, op: _, right, ..
    } = parser_expr_strip_casts(&assignment.expr)
    else {
        return None;
    };
    let assignment_literal = parser_binop_either_literal_value(left, right)?;

    let selection = parser_expr_strip_casts(update.selection.as_ref()?);
    let aiondb_parser::Expr::BinaryOp {
        left, op, right, ..
    } = selection
    else {
        return None;
    };
    if *op != aiondb_parser::BinaryOperator::Eq {
        return None;
    }
    let filter_literal = parser_binop_either_literal_value(left, right)?;

    Some((assignment_literal, filter_literal))
}

fn extract_bound_insert_values_literals(statement: &Statement) -> Option<Vec<Option<Value>>> {
    let Statement::Insert(insert) = statement else {
        return None;
    };
    if insert.query.is_some() || insert.rows.len() != 1 {
        return None;
    }
    Some(
        insert.rows[0]
            .iter()
            .map(|expr| match parser_expr_strip_casts(expr) {
                aiondb_parser::Expr::Literal(literal, _) => parser_literal_to_value(literal),
                _ => None,
            })
            .collect(),
    )
}

use super::statement_exec::{
    insert_values_storage_autocommit_candidate, is_transaction_not_active_in_storage_error,
};

fn rewrite_typed_literal_side(
    expr: &aiondb_plan::TypedExpr,
    literal: &Value,
) -> Option<aiondb_plan::TypedExpr> {
    match &expr.kind {
        aiondb_plan::TypedExprKind::Literal(_) => Some(aiondb_plan::TypedExpr {
            kind: aiondb_plan::TypedExprKind::Literal(literal.clone()),
            data_type: expr.data_type.clone(),
            nullable: expr.nullable,
        }),
        aiondb_plan::TypedExprKind::Cast {
            expr: inner,
            target_type,
        } => {
            let rewritten_inner = rewrite_typed_literal_side(inner, literal)?;
            Some(aiondb_plan::TypedExpr {
                kind: aiondb_plan::TypedExprKind::Cast {
                    expr: Box::new(rewritten_inner),
                    target_type: target_type.clone(),
                },
                data_type: expr.data_type.clone(),
                nullable: expr.nullable,
            })
        }
        _ => None,
    }
}

fn rewrite_typed_assignment_literal(
    expr: &aiondb_plan::TypedExpr,
    literal: &Value,
) -> Option<aiondb_plan::TypedExpr> {
    match &expr.kind {
        aiondb_plan::TypedExprKind::ArithAdd { left, right } => {
            rewrite_typed_binary_literal(expr, literal, left, right, |left, right| {
                aiondb_plan::TypedExprKind::ArithAdd { left, right }
            })
        }
        aiondb_plan::TypedExprKind::ArithSub { left, right } => {
            rewrite_typed_binary_literal(expr, literal, left, right, |left, right| {
                aiondb_plan::TypedExprKind::ArithSub { left, right }
            })
        }
        aiondb_plan::TypedExprKind::ArithMul { left, right } => {
            rewrite_typed_binary_literal(expr, literal, left, right, |left, right| {
                aiondb_plan::TypedExprKind::ArithMul { left, right }
            })
        }
        aiondb_plan::TypedExprKind::ArithDiv { left, right } => {
            rewrite_typed_binary_literal(expr, literal, left, right, |left, right| {
                aiondb_plan::TypedExprKind::ArithDiv { left, right }
            })
        }
        aiondb_plan::TypedExprKind::ArithMod { left, right } => {
            rewrite_typed_binary_literal(expr, literal, left, right, |left, right| {
                aiondb_plan::TypedExprKind::ArithMod { left, right }
            })
        }
        _ => None,
    }
}

fn rewrite_typed_binary_literal(
    expr: &aiondb_plan::TypedExpr,
    literal: &Value,
    left: &aiondb_plan::TypedExpr,
    right: &aiondb_plan::TypedExpr,
    build: impl Fn(
        Box<aiondb_plan::TypedExpr>,
        Box<aiondb_plan::TypedExpr>,
    ) -> aiondb_plan::TypedExprKind,
) -> Option<aiondb_plan::TypedExpr> {
    if let Some(rewritten_left) = rewrite_typed_literal_side(left, literal) {
        return Some(aiondb_plan::TypedExpr {
            kind: build(Box::new(rewritten_left), Box::new(right.clone())),
            data_type: expr.data_type.clone(),
            nullable: expr.nullable,
        });
    }
    if let Some(rewritten_right) = rewrite_typed_literal_side(right, literal) {
        return Some(aiondb_plan::TypedExpr {
            kind: build(Box::new(left.clone()), Box::new(rewritten_right)),
            data_type: expr.data_type.clone(),
            nullable: expr.nullable,
        });
    }
    None
}

fn rewrite_typed_eq_filter_literal(
    filter: &aiondb_plan::TypedExpr,
    literal: &Value,
) -> Option<aiondb_plan::TypedExpr> {
    let aiondb_plan::TypedExprKind::BinaryEq { left, right } = &filter.kind else {
        return None;
    };

    if let Some(rewritten_left) = rewrite_typed_literal_side(left, literal) {
        return Some(aiondb_plan::TypedExpr {
            kind: aiondb_plan::TypedExprKind::BinaryEq {
                left: Box::new(rewritten_left),
                right: right.clone(),
            },
            data_type: filter.data_type.clone(),
            nullable: filter.nullable,
        });
    }
    if let Some(rewritten_right) = rewrite_typed_literal_side(right, literal) {
        return Some(aiondb_plan::TypedExpr {
            kind: aiondb_plan::TypedExprKind::BinaryEq {
                left: left.clone(),
                right: Box::new(rewritten_right),
            },
            data_type: filter.data_type.clone(),
            nullable: filter.nullable,
        });
    }
    None
}

fn rewrite_index_eq_access_path(
    access_path: &aiondb_plan::ScanAccessPath,
    literal: &Value,
) -> Option<aiondb_plan::ScanAccessPath> {
    match access_path {
        aiondb_plan::ScanAccessPath::IndexEq { index_id, .. } => {
            Some(aiondb_plan::ScanAccessPath::IndexEq {
                index_id: *index_id,
                value: literal.clone(),
            })
        }
        aiondb_plan::ScanAccessPath::IndexOnlyScan {
            inner,
            index_column_ids,
        } => Some(aiondb_plan::ScanAccessPath::IndexOnlyScan {
            inner: Box::new(rewrite_index_eq_access_path(inner, literal)?),
            index_column_ids: index_column_ids.clone(),
        }),
        aiondb_plan::ScanAccessPath::SeqScan => Some(aiondb_plan::ScanAccessPath::SeqScan),
        aiondb_plan::ScanAccessPath::IndexEqComposite { .. }
        | aiondb_plan::ScanAccessPath::IndexEqRangeComposite { .. }
        | aiondb_plan::ScanAccessPath::IndexRange { .. }
        | aiondb_plan::ScanAccessPath::GinContainment { .. }
        | aiondb_plan::ScanAccessPath::BitmapOr { .. }
        | aiondb_plan::ScanAccessPath::BitmapAnd { .. } => None,
    }
}

fn rewrite_cached_parameterized_update_table_plan_with_literals(
    plan: &aiondb_plan::PhysicalPlan,
    assignment_literal: &Value,
    filter_literal: &Value,
) -> Option<aiondb_plan::PhysicalPlan> {
    let aiondb_plan::PhysicalPlan::UpdateTable {
        table_id,
        assignments,
        filter,
        returning,
        from_table_ids,
    } = plan
    else {
        return None;
    };
    if !from_table_ids.is_empty() || assignments.len() != 1 {
        return None;
    }

    let rewritten_assignment_expr =
        rewrite_typed_assignment_literal(&assignments[0].expr, assignment_literal)?;
    let rewritten_filter = rewrite_typed_eq_filter_literal(filter.as_ref()?, filter_literal)?;
    let rewritten_assignments = vec![aiondb_plan::UpdateAssignment {
        column_ordinal: assignments[0].column_ordinal,
        data_type: assignments[0].data_type.clone(),
        nullable: assignments[0].nullable,
        expr: rewritten_assignment_expr,
    }];

    Some(aiondb_plan::PhysicalPlan::UpdateTable {
        table_id: *table_id,
        assignments: rewritten_assignments,
        filter: Some(rewritten_filter),
        returning: returning.clone(),
        from_table_ids: from_table_ids.clone(),
    })
}

/// Variant of `rewrite_cached_parameterized_update_table_plan_with_literals`
/// for `UPDATE t SET col = expr WHERE col = ?` shapes where the
/// assignment expression is computed (e.g. `SET counter = counter + 1`)
/// rather than a substitutable literal. Without this, prepared
/// `UPDATE t SET col = col + 1 WHERE id = $1` queries kept the
/// `Param(1)` placeholder in their WHERE filter and missed the
/// index_eq fast path; the assignment expression is left untouched.
fn rewrite_cached_parameterized_update_filter_only_with_literal(
    plan: &aiondb_plan::PhysicalPlan,
    filter_literal: &Value,
) -> Option<aiondb_plan::PhysicalPlan> {
    let aiondb_plan::PhysicalPlan::UpdateTable {
        table_id,
        assignments,
        filter,
        returning,
        from_table_ids,
    } = plan
    else {
        return None;
    };
    if !from_table_ids.is_empty() {
        return None;
    }
    let rewritten_filter = rewrite_typed_eq_filter_literal(filter.as_ref()?, filter_literal)?;
    Some(aiondb_plan::PhysicalPlan::UpdateTable {
        table_id: *table_id,
        assignments: assignments.clone(),
        filter: Some(rewritten_filter),
        returning: returning.clone(),
        from_table_ids: from_table_ids.clone(),
    })
}

fn rewrite_cached_parameterized_insert_values_plan_with_literals(
    plan: &aiondb_plan::PhysicalPlan,
    statement_literals: &[Option<Value>],
) -> Option<aiondb_plan::PhysicalPlan> {
    let aiondb_plan::PhysicalPlan::InsertValues {
        table_id,
        columns,
        rows,
        on_conflict,
        returning,
    } = plan
    else {
        return None;
    };
    if rows.len() != 1 || rows[0].len() != statement_literals.len() {
        return None;
    }

    let mut rewritten_row = Vec::with_capacity(rows[0].len());
    for (expr, literal) in rows[0].iter().zip(statement_literals.iter()) {
        if let Some(literal) = literal.as_ref() {
            rewritten_row.push(rewrite_typed_literal_side(expr, literal)?);
        } else {
            rewritten_row.push(expr.clone());
        }
    }

    Some(aiondb_plan::PhysicalPlan::InsertValues {
        table_id: *table_id,
        columns: columns.clone(),
        rows: vec![rewritten_row],
        on_conflict: on_conflict.clone(),
        returning: returning.clone(),
    })
}

fn rewrite_cached_parameterized_project_table_plan_with_literal(
    plan: &aiondb_plan::PhysicalPlan,
    literal: &Value,
) -> Option<aiondb_plan::PhysicalPlan> {
    match plan {
        aiondb_plan::PhysicalPlan::ProjectTable {
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
            let rewritten_filter = rewrite_typed_eq_filter_literal(filter.as_ref()?, literal)?;
            let rewritten_access_path = rewrite_index_eq_access_path(access_path, literal)?;
            Some(aiondb_plan::PhysicalPlan::ProjectTable {
                table_id: *table_id,
                outputs: outputs.clone(),
                filter: Some(rewritten_filter),
                order_by: order_by.clone(),
                limit: limit.clone(),
                offset: offset.clone(),
                distinct: *distinct,
                distinct_on: distinct_on.clone(),
                access_path: rewritten_access_path,
            })
        }
        aiondb_plan::PhysicalPlan::LockingProjectTable {
            table_id,
            outputs,
            filter,
            order_by,
            limit,
            offset,
            distinct,
            distinct_on,
            access_path,
            row_lock,
        } => {
            let rewritten_filter = rewrite_typed_eq_filter_literal(filter.as_ref()?, literal)?;
            let rewritten_access_path = rewrite_index_eq_access_path(access_path, literal)?;
            Some(aiondb_plan::PhysicalPlan::LockingProjectTable {
                table_id: *table_id,
                outputs: outputs.clone(),
                filter: Some(rewritten_filter),
                order_by: order_by.clone(),
                limit: limit.clone(),
                offset: offset.clone(),
                distinct: *distinct,
                distinct_on: distinct_on.clone(),
                access_path: rewritten_access_path,
                row_lock: *row_lock,
            })
        }
        _ => None,
    }
}

fn rewrite_cached_parameterized_project_table_plan(
    plan: &aiondb_plan::PhysicalPlan,
    statement: &Statement,
) -> Option<aiondb_plan::PhysicalPlan> {
    if let Some((assignment_literal, filter_literal)) = extract_bound_update_literals(statement) {
        if let Some(rewritten) = rewrite_cached_parameterized_update_table_plan_with_literals(
            plan,
            &assignment_literal,
            &filter_literal,
        ) {
            return Some(rewritten);
        }
    }

    if let Some(literal) = extract_bound_statement_eq_literal(statement) {
        if let Some(rewritten) =
            rewrite_cached_parameterized_project_table_plan_with_literal(plan, &literal)
        {
            return Some(rewritten);
        }
        if let Some(rewritten) =
            rewrite_cached_parameterized_aggregate_plan_with_literal(plan, &literal)
        {
            return Some(rewritten);
        }
        if let Some(rewritten) =
            rewrite_cached_parameterized_delete_plan_with_literal(plan, &literal)
        {
            return Some(rewritten);
        }
        // Pure-filter UPDATE param rewrite: handles
        // \`UPDATE t SET col = col + 1 WHERE id = \$1\` shapes where the
        // assignment is computed and only the WHERE clause has a
        // substitutable literal. The full
        // \`update_table_plan_with_literals\` rewrite below handles the
        // both-literal case.
        if let Some(rewritten) =
            rewrite_cached_parameterized_update_filter_only_with_literal(plan, &literal)
        {
            return Some(rewritten);
        }
    }

    if let Some(literals) = extract_bound_insert_values_literals(statement) {
        if let Some(rewritten) =
            rewrite_cached_parameterized_insert_values_plan_with_literals(plan, &literals)
        {
            return Some(rewritten);
        }
    }

    None
}

/// Substitute the bound literal into a cached
/// `PhysicalPlan::DeleteFromTable` with a parameterised equality
/// filter. Without this, prepared `DELETE FROM t WHERE col = $1`
/// queries kept the `Param(1)` placeholder in their cached plan,
/// so the executor's `extract_dml_simple_eq_literal_filter` saw a
/// Param (not a Literal) and missed the index_eq fast path on
/// every Execute.
fn rewrite_cached_parameterized_delete_plan_with_literal(
    plan: &aiondb_plan::PhysicalPlan,
    literal: &Value,
) -> Option<aiondb_plan::PhysicalPlan> {
    let aiondb_plan::PhysicalPlan::DeleteFromTable {
        table_id,
        filter,
        returning,
        using_table_ids,
    } = plan
    else {
        return None;
    };
    if !using_table_ids.is_empty() {
        return None;
    }
    let rewritten_filter = rewrite_typed_eq_filter_literal(filter.as_ref()?, literal)?;
    Some(aiondb_plan::PhysicalPlan::DeleteFromTable {
        table_id: *table_id,
        filter: Some(rewritten_filter),
        returning: returning.clone(),
        using_table_ids: using_table_ids.clone(),
    })
}

/// Substitute the bound literal into a cached `PhysicalPlan::Aggregate`
/// with a parameterised equality filter. Without this, prepared
/// `SELECT COUNT(*) FROM t WHERE col = $1` queries kept the
/// `Param(1)` placeholder in their cached plan and missed the
/// `try_count_simple_eq_filter` index-backed fast path on every
/// Execute.
fn rewrite_cached_parameterized_aggregate_plan_with_literal(
    plan: &aiondb_plan::PhysicalPlan,
    literal: &Value,
) -> Option<aiondb_plan::PhysicalPlan> {
    let aiondb_plan::PhysicalPlan::Aggregate {
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
    } = plan
    else {
        return None;
    };
    let rewritten_filter = rewrite_typed_eq_filter_literal(filter.as_ref()?, literal)?;
    let rewritten_access_path = rewrite_index_eq_access_path(access_path, literal)?;
    Some(aiondb_plan::PhysicalPlan::Aggregate {
        table_id: *table_id,
        group_by: group_by.clone(),
        grouping_sets: grouping_sets.clone(),
        aggregates: aggregates.clone(),
        having: having.clone(),
        filter: Some(rewritten_filter),
        order_by: order_by.clone(),
        limit: limit.clone(),
        offset: offset.clone(),
        distinct: *distinct,
        distinct_on: distinct_on.clone(),
        access_path: rewritten_access_path,
    })
}

impl Engine {
    fn portal_statement_uses_execute_statement(statement: &Statement) -> bool {
        matches!(
            statement,
            Statement::Begin { .. }
                | Statement::Commit { .. }
                | Statement::Rollback { .. }
                | Statement::Savepoint { .. }
                | Statement::RollbackToSavepoint { .. }
                | Statement::ReleaseSavepoint { .. }
                | Statement::SetTransaction(_)
                | Statement::SetSessionCharacteristics(_)
                | Statement::Explain { .. }
                | Statement::Backup { .. }
                | Statement::Restore { .. }
                | Statement::Checkpoint { .. }
                | Statement::CreateTenant { .. }
                | Statement::DropTenant { .. }
                | Statement::SetTenant { .. }
                | Statement::CreateFunction(_)
                | Statement::DropFunction(_)
                | Statement::CreateTrigger(_)
                | Statement::DropTrigger(_)
                | Statement::AlterTriggerRename(_)
                | Statement::CreateExtension(_)
                | Statement::DropExtension(_)
                | Statement::SetVariable(_)
                | Statement::ShowVariable(_)
                | Statement::ResetVariable(_)
                | Statement::Cypher(_)
        )
    }

    fn collapse_portal_compat_results(
        &self,
        session: &SessionHandle,
        results: Vec<StatementResult>,
        context: &str,
    ) -> DbResult<StatementResult> {
        let mut notices = Vec::new();
        let mut final_result = None;
        for result in results {
            match result {
                StatementResult::Notice { message } => notices.push(message),
                other => {
                    if final_result.replace(other).is_some() {
                        return Err(DbError::feature_not_supported(format!(
                            "{context} cannot return multiple non-notice results",
                        )));
                    }
                }
            }
        }

        if !notices.is_empty() {
            self.with_session_mut(session, |record| {
                record.extend_notices(notices);
                Ok(())
            })?;
        }

        final_result.ok_or_else(|| {
            DbError::internal(format!(
                "{context} returned no terminal statement result for portal execution",
            ))
        })
    }

    fn portal_statement_compat_command_name(statement: &Statement) -> Option<&str> {
        match statement {
            Statement::DeclareStmt { .. } => Some("DECLARE"),
            Statement::FetchStmt { .. } => Some("FETCH"),
            Statement::MoveStmt { .. } => Some("MOVE"),
            Statement::CloseStmt { .. } => Some("CLOSE"),
            statement => statement.compat_tag(),
        }
    }

    fn execute_portal_compat_command(
        &self,
        session: &SessionHandle,
        statement_sql: Option<&str>,
        statement: &Statement,
    ) -> DbResult<StatementResult> {
        let Some(statement_sql) = statement_sql else {
            let physical_plan = self.prepare_physical_plan_for_execution(session, statement)?;
            let (result, _) =
                self.execute_physical_plan(session, physical_plan.as_ref(), None, 0)?;
            return Ok(result);
        };
        if let Some(compat_results) =
            self.execute_compat_cursor_command(session, statement_sql, statement)?
        {
            self.collapse_portal_compat_results(
                session,
                compat_results,
                "prepared compat cursor command",
            )
        } else {
            Err(super::compat::unsupported_compat_command(
                Self::portal_statement_compat_command_name(statement).unwrap_or("COMPAT"),
            ))
        }
    }

    fn portal_query_requires_single_execution_cache(statement: &Statement) -> bool {
        match statement {
            Statement::Insert(insert) => !insert.returning.is_empty(),
            Statement::Update(update) => !update.returning.is_empty(),
            Statement::Delete(delete) => !delete.returning.is_empty(),
            _ => false,
        }
    }

    fn cache_portal_query_rows(
        &self,
        session: &SessionHandle,
        portal_name: &str,
        columns: &[ResultColumn],
        rows: &[Row],
    ) -> DbResult<()> {
        self.with_session_mut(session, |record| {
            if let Some(portal) = record.portals.get_mut(portal_name) {
                portal.cached_columns = Some(columns.to_vec());
                portal.cached_rows = Some(rows.to_vec());
            }
            Ok(())
        })
    }

    pub(super) fn execute_cached_portal_query(
        &self,
        session: &SessionHandle,
        portal_name: &str,
        statement: &Statement,
        max_rows: usize,
    ) -> DbResult<PortalBatch> {
        let requested_rows = if max_rows == 0 { usize::MAX } else { max_rows };
        let (columns, rows, end, exhausted) = self.with_session(session, |record| {
            let portal = record.portals.get(portal_name).ok_or_else(|| {
                DbError::parse_error(aiondb_core::SqlState::UndefinedObject, "unknown portal")
            })?;
            let columns = portal.cached_columns.as_ref().ok_or_else(|| {
                DbError::internal("portal cached result columns missing during cached execute")
            })?;
            let cached_rows = portal.cached_rows.as_ref().ok_or_else(|| {
                DbError::internal("portal cached result rows missing during cached execute")
            })?;
            let total_rows = cached_rows.len();
            let start = portal.position.min(total_rows);
            let end = total_rows.min(start.saturating_add(requested_rows));
            let exhausted = end >= total_rows;
            Ok((
                columns.clone(),
                cached_rows[start..end].to_vec(),
                end,
                exhausted,
            ))
        })?;

        self.update_portal_progress(session, portal_name, end, exhausted)?;

        let batch = PortalBatch {
            columns,
            rows,
            tag: if exhausted {
                query_completion_tag(statement, end)
            } else {
                "SELECT".to_owned()
            },
            rows_affected: 0,
            exhausted,
        };

        if batch.exhausted {
            self.mark_portal_exhausted(session, portal_name)?;
        }
        Ok(batch)
    }

    fn execute_portal_statement_core(
        &self,
        session: &SessionHandle,
        statement: &Statement,
        statement_sql: Option<&str>,
        precomputed_plan_fingerprint: Option<crate::session::StatementFingerprint>,
        contains_recursive_cte: bool,
        disable_plan_cache: bool,
        parameterized_plan_literal_rewrite: bool,
        parameterized_eq_literal_override: Option<&Value>,
        parameterized_insert_values_literals_override: Option<&[Option<Value>]>,
        position: usize,
        max_rows: usize,
        executor_applied_portal_offset: &mut bool,
    ) -> DbResult<StatementResult> {
        match statement {
            Statement::Begin { .. }
            | Statement::Commit { .. }
            | Statement::Rollback { .. }
            | Statement::PrepareTransaction { .. }
            | Statement::CommitPrepared { .. }
            | Statement::RollbackPrepared { .. }
            | Statement::PrepareStmt { .. }
            | Statement::ExecuteStmt { .. }
            | Statement::DeallocateStmt { .. }
            | Statement::Checkpoint { .. }
            | Statement::SecurityLabel(_)
            | Statement::Comment(_)
            | Statement::AlterSystem(_)
            | Statement::Savepoint { .. }
            | Statement::RollbackToSavepoint { .. }
            | Statement::ReleaseSavepoint { .. }
            | Statement::CreateDatabase(_)
            | Statement::AlterDatabase(_)
            | Statement::DropDatabase(_)
            | Statement::CreateType(_)
            | Statement::AlterType(_)
            | Statement::DropType(_)
            | Statement::CreateDomain(_)
            | Statement::AlterDomain(_)
            | Statement::DropDomain(_)
            | Statement::CreateCast(_)
            | Statement::DropCast(_)
            | Statement::CreateRule(_)
            | Statement::AlterRule(_)
            | Statement::DropRule(_)
            | Statement::CreateOrReplaceCompat(_)
            | Statement::CreatePolicy(_)
            | Statement::AlterPolicy(_)
            | Statement::DropPolicy(_)
            | Statement::CreatePublication(_)
            | Statement::AlterPublication(_)
            | Statement::DropPublication(_)
            | Statement::CreateSubscription(_)
            | Statement::AlterSubscription(_)
            | Statement::DropSubscription(_)
            | Statement::CreateServer(_)
            | Statement::AlterServer(_)
            | Statement::DropServer(_)
            | Statement::CreateUserMapping(_)
            | Statement::AlterUserMapping(_)
            | Statement::DropUserMapping(_)
            | Statement::CreateForeignTable(_)
            | Statement::AlterForeignTable(_)
            | Statement::DropForeignTable(_)
            | Statement::CreateForeignDataWrapper(_)
            | Statement::AlterForeignDataWrapper(_)
            | Statement::DropForeignDataWrapper(_)
            | Statement::CreateCollation(_)
            | Statement::AlterCollation(_)
            | Statement::DropCollation(_)
            | Statement::CreateStatistics(_)
            | Statement::CreateTablespace(_)
            | Statement::DropStatistics(_)
            | Statement::AlterStatistics(_)
            | Statement::DropTablespace(_)
            | Statement::AlterTablespace(_)
            | Statement::CreateAggregate(_)
            | Statement::DropAggregate(_)
            | Statement::CreateProcedure(_)
            | Statement::DropProcedure(_)
            | Statement::DropRoutine(_)
            | Statement::AlterTriggerCompat(_)
            | Statement::CreateOperator(_)
            | Statement::DropOperator(_)
            | Statement::CompatTagged(_)
            | Statement::CompatTaggedNotice(_)
            | Statement::PgCompatUtility(_) => {
                self.execute_statement_prechecked(session, statement)
            }
            Statement::Explain { .. }
            | Statement::Backup { .. }
            | Statement::Restore { .. }
            | Statement::CreateTenant { .. }
            | Statement::DropTenant { .. }
            | Statement::SetTenant { .. }
            | Statement::CreateFunction(_)
            | Statement::DropFunction(_)
            | Statement::CreateTrigger(_)
            | Statement::DropTrigger(_)
            | Statement::AlterTriggerRename(_)
            | Statement::CreateExtension(_)
            | Statement::DropExtension(_)
            | Statement::Listen { .. }
            | Statement::Unlisten { .. }
            | Statement::Notify { .. }
            | Statement::Lock(_)
            | Statement::Discard(_)
            | Statement::DoStmt { .. }
            | Statement::DropOwned(_)
            | Statement::ReassignOwned(_)
            | Statement::Load { .. }
            | Statement::SetTransaction(_)
            | Statement::SetSessionCharacteristics(_)
            | Statement::SetConstraints(_)
            | Statement::SetVariable(_)
            | Statement::ShowVariable(_)
            | Statement::ResetVariable(_) => self.execute_statement_prechecked(session, statement),
            // Cypher goes through the planned-statement path (with fallback
            // inside execute_statement if the direct pipeline fails). Preserve
            // the prepared statement fingerprint here so repeated graph
            // portals avoid recomputing the plan-cache key on the hot path.
            Statement::Cypher(_) => {
                if let Some(fingerprint) = precomputed_plan_fingerprint {
                    self.execute_statement_prechecked_with_fingerprint(
                        session,
                        statement,
                        fingerprint,
                    )
                } else {
                    self.execute_statement_prechecked(session, statement)
                }
            }
            Statement::Analyze { .. }
            | Statement::Vacuum { .. }
            | Statement::CreateTable(_)
            | Statement::CreateTableAs(_)
            | Statement::CreateSequence(_)
            | Statement::CreateIndex(_)
            | Statement::TruncateTable(_)
            | Statement::DropTable(_)
            | Statement::DropIndex(_)
            | Statement::DropSequence(_)
            | Statement::AlterTable(_)
            | Statement::Delete(_)
            | Statement::Insert(_)
            | Statement::Select(_)
            | Statement::SetOperation(_)
            | Statement::Update(_)
            | Statement::Copy(_)
            | Statement::CreateView(_)
            | Statement::DropView(_)
            | Statement::CreateNodeLabel(_)
            | Statement::CreateEdgeLabel(_)
            | Statement::DropNodeLabel(_)
            | Statement::DropEdgeLabel(_)
            | Statement::CreateRole(_)
            | Statement::DropRole(_)
            | Statement::AlterRole(_)
            | Statement::AlterRoleRename(_)
            | Statement::Grant(_)
            | Statement::Revoke(_)
            | Statement::CreateSchema(_)
            | Statement::DropSchema(_)
            | Statement::Merge(_) => self.with_compat_eval_session(session, || {
                let statement = if contains_recursive_cte {
                    recursive_cte::maybe_rewrite_for_execution(self, session, statement)?
                } else {
                    std::borrow::Cow::Borrowed(statement)
                };
                let storage_fast_path_enabled = std::cell::Cell::new(false);
                let mut execute = || {
                    let physical_plan = if disable_plan_cache {
                        self.prepare_physical_plan_for_execution_uncached(session, &statement)?
                    } else if let Some(fingerprint) = precomputed_plan_fingerprint {
                        let hits_before = self
                            .plan_cache_hits
                            .load(std::sync::atomic::Ordering::Relaxed);
                        let cached_or_compiled = self
                            .prepare_physical_plan_for_execution_with_fingerprint(
                                session,
                                &statement,
                                fingerprint,
                            )?;
                        let hits_after = self
                            .plan_cache_hits
                            .load(std::sync::atomic::Ordering::Relaxed);
                        let from_cache = hits_after > hits_before;
                        if parameterized_plan_literal_rewrite && from_cache {
                            let rewritten = if let Some(literal) = parameterized_eq_literal_override
                            {
                                rewrite_cached_parameterized_project_table_plan_with_literal(
                                    cached_or_compiled.as_ref(),
                                    literal,
                                )
                            } else if let Some(literals) =
                                parameterized_insert_values_literals_override
                            {
                                rewrite_cached_parameterized_insert_values_plan_with_literals(
                                    cached_or_compiled.as_ref(),
                                    literals,
                                )
                            } else {
                                rewrite_cached_parameterized_project_table_plan(
                                    cached_or_compiled.as_ref(),
                                    &statement,
                                )
                            };
                            if let Some(rewritten) = rewritten {
                                Arc::new(rewritten)
                            } else {
                                self.prepare_physical_plan_for_execution_uncached(
                                    session, &statement,
                                )?
                            }
                        } else {
                            cached_or_compiled
                        }
                    } else {
                        self.prepare_physical_plan_for_execution(session, &statement)?
                    };
                    let (row_limit, row_offset) = if matches!(
                        physical_plan.as_ref(),
                        aiondb_plan::PhysicalPlan::ProjectTable { .. }
                            | aiondb_plan::PhysicalPlan::LockingProjectTable { .. }
                            | aiondb_plan::PhysicalPlan::ProjectValues { .. }
                    ) {
                        *executor_applied_portal_offset = true;
                        (
                            if max_rows == 0 {
                                None
                            } else {
                                Some(
                                    u64::try_from(max_rows)
                                        .unwrap_or(u64::MAX)
                                        .saturating_add(1),
                                )
                            },
                            u64::try_from(position).unwrap_or(u64::MAX),
                        )
                    } else {
                        (
                            if max_rows == 0 {
                                None
                            } else {
                                Some(
                                    u64::try_from(
                                        position.saturating_add(max_rows).saturating_add(1),
                                    )
                                    .unwrap_or(u64::MAX),
                                )
                            },
                            0,
                        )
                    };
                    let (result, _) = if storage_fast_path_enabled.get() {
                        self.execute_physical_plan_with_storage_autocommit_fast_path(
                            session,
                            physical_plan.as_ref(),
                            row_limit,
                            row_offset,
                        )?
                    } else {
                        self.execute_physical_plan(
                            session,
                            physical_plan.as_ref(),
                            row_limit,
                            row_offset,
                        )?
                    };
                    Ok(result)
                };
                if statement_requires_implicit_transaction(&statement) {
                    let active_txn =
                        self.with_session(session, |record| Ok(record.active_txn.is_some()))?;
                    let storage_autocommit_candidate =
                        !active_txn && insert_values_storage_autocommit_candidate(&statement);
                    storage_fast_path_enabled.set(storage_autocommit_candidate);
                    let include_catalog = active_txn
                        || (!storage_autocommit_candidate
                            && !super::statement_policy::statement_can_skip_catalog_txn_participant(
                                &statement,
                            ));
                    let include_storage = active_txn
                        || (!super::statement_policy::statement_can_skip_storage_txn_participant(
                            &statement,
                        ) && !storage_autocommit_candidate);
                    let mut result = self.execute_with_implicit_transaction_options(
                        session,
                        include_catalog,
                        include_storage,
                        &mut execute,
                    );
                    if storage_autocommit_candidate
                        && result
                            .as_ref()
                            .is_err_and(is_transaction_not_active_in_storage_error)
                    {
                        storage_fast_path_enabled.set(false);
                        result = self.execute_with_implicit_transaction_options(
                            session,
                            include_catalog,
                            true,
                            execute,
                        );
                    }
                    result
                } else {
                    execute()
                }
            }),
            Statement::DeclareStmt { .. }
            | Statement::FetchStmt { .. }
            | Statement::MoveStmt { .. }
            | Statement::CloseStmt { .. } => {
                self.execute_portal_compat_command(session, statement_sql, statement)
            }
            statement if statement.compat_tag().is_some() => {
                self.execute_portal_compat_command(session, statement_sql, statement)
            }
            _ => Err(DbError::internal(
                "unhandled statement reached portal execution dispatch",
            )),
        }
    }

    #[allow(clippy::fn_params_excessive_bools)]
    pub(super) fn execute_portal_statement(
        &self,
        session: &SessionHandle,
        portal_name: &str,
        track_portal_state: bool,
        failed_txn_active_prechecked: Option<bool>,
        statement: &Statement,
        completion_statement: &Statement,
        statement_sql: Option<&str>,
        compat_hints: Option<PortalCompatHints>,
        precomputed_plan_fingerprint: Option<crate::session::StatementFingerprint>,
        contains_recursive_cte: bool,
        disable_plan_cache: bool,
        parameterized_plan_literal_rewrite: bool,
        parameterized_eq_literal_override: Option<Value>,
        parameterized_insert_values_literals_override: Option<Vec<Option<Value>>>,
        position: usize,
        max_rows: usize,
    ) -> DbResult<PortalBatch> {
        let requested_rows = if max_rows == 0 { usize::MAX } else { max_rows };
        let mut executor_applied_portal_offset = false;
        if let Some(statement_sql) = statement_sql {
            if super::compat::statement_tracks_compat_types(statement)
                && !super::compat::statement_has_post_statement_compat_effects(statement)
            {
                if let Err(error) = self.with_session_mut(session, |record| {
                    super::compat::track_compat_types(record, statement_sql, statement);
                    Ok(())
                }) {
                    tracing::warn!(
                        error = %error,
                        "failed to track compatibility statement metadata in portal execution"
                    );
                }
            }
        }
        let failed_txn_active = if let Some(active) = failed_txn_active_prechecked {
            active
        } else {
            self.with_session(session, |record| {
                Ok(record.transaction_failed
                    && record.active_txn.is_some()
                    && !record.implicit_txn_active)
            })?
        };
        if failed_txn_active
            && !super::query_api_wire::statement_allowed_in_failed_transaction(
                self,
                session,
                statement_sql.unwrap_or_default(),
                statement,
            )
        {
            return Err(super::query_api::failed_transaction_error());
        }
        self.authorize_statement(session, completion_statement)?;
        let mut compat_effects_applied = false;
        let result: DbResult<StatementResult> = (|| {
            let uses_compat_command_hooks = if let Some(hints) = compat_hints {
                hints.uses_command_hooks
            } else if let Some(statement_sql) = statement_sql {
                super::compat::statement_uses_compat_command_hooks_with_sql(
                    statement,
                    statement_sql,
                )
            } else {
                super::compat::statement_uses_compat_command_hooks(statement)
            };
            let uses_compat_rule_dml = compat_hints
                .map(|hints| hints.uses_rule_dml)
                .unwrap_or_else(|| super::compat::statement_uses_compat_rule_dml(statement));
            let pg_object_command_is_planner_owned =
                super::compat::statement_is_planner_pg_object_command(statement);
            let may_use_drop_if_exists_notice = compat_hints
                .map(|hints| hints.may_use_drop_if_exists_notice)
                .unwrap_or_else(|| {
                    statement_sql.is_some()
                        && super::compat::statement_may_use_drop_if_exists_notice(statement)
                });

            if !uses_compat_command_hooks
                && !uses_compat_rule_dml
                && !may_use_drop_if_exists_notice
                && !pg_object_command_is_planner_owned
            {
                return self.execute_portal_statement_core(
                    session,
                    statement,
                    statement_sql,
                    precomputed_plan_fingerprint,
                    contains_recursive_cte,
                    disable_plan_cache,
                    parameterized_plan_literal_rewrite,
                    parameterized_eq_literal_override.as_ref(),
                    parameterized_insert_values_literals_override.as_deref(),
                    position,
                    max_rows,
                    &mut executor_applied_portal_offset,
                );
            }

            let compat_prepared_result = if uses_compat_command_hooks {
                if let Some(statement_sql) = statement_sql {
                    if let Some(compat_results) =
                        self.execute_compat_prepared_command(session, statement_sql, statement)?
                    {
                        Some(self.collapse_portal_compat_results(
                            session,
                            compat_results,
                            "prepared compatibility command",
                        )?)
                    } else {
                        None
                    }
                } else {
                    None
                }
            } else {
                None
            };
            compat_effects_applied |= compat_prepared_result.is_some();
            let compat_do_result = if uses_compat_command_hooks {
                if let Some(statement_sql) = statement_sql {
                    if let Some(compat_results) =
                        self.execute_compat_do_block(session, statement_sql)?
                    {
                        Some(self.collapse_portal_compat_results(
                            session,
                            compat_results,
                            "DO compatibility block",
                        )?)
                    } else {
                        None
                    }
                } else {
                    None
                }
            } else {
                None
            };
            let compat_revoke_role_result = if uses_compat_command_hooks {
                if matches!(
                    statement,
                    Statement::Revoke(revoke)
                        if matches!(revoke.target, aiondb_parser::GrantTarget::Role(_))
                ) && statement_sql
                    .is_some_and(|sql| sql.to_ascii_lowercase().contains("option for"))
                {
                    if let Some(statement_sql) = statement_sql {
                        if let Some(compat_results) = self.handle_compat_revoke_role_option_for(
                            session,
                            statement_sql,
                            statement,
                        )? {
                            Some(self.collapse_portal_compat_results(
                                session,
                                compat_results,
                                "REVOKE ROLE compatibility command",
                            )?)
                        } else {
                            None
                        }
                    } else {
                        None
                    }
                } else {
                    None
                }
            } else {
                None
            };
            // CREATE/DROP CAST is handled by the direct-command +
            // drop-if-exists-notice paths.
            let compat_cast_result: Option<StatementResult> = None;
            let _ = (statement, uses_compat_command_hooks);
            let compat_type_drop_if_exists_result = if uses_compat_command_hooks {
                if let Some(statement_sql) = statement_sql {
                    if let Some(compat_results) =
                        self.compat_type_drop_if_exists_results(session, statement_sql, statement)?
                    {
                        Some(self.collapse_portal_compat_results(
                            session,
                            compat_results,
                            "DROP TYPE/DOMAIN IF EXISTS compatibility command",
                        )?)
                    } else {
                        None
                    }
                } else {
                    None
                }
            } else {
                None
            };
            let compat_tracked_type_result =
                if super::compat::statement_tracks_compat_types(statement)
                    && matches!(
                        super::compat::statement_compat_tag(statement),
                        Some("CREATE TYPE" | "CREATE DOMAIN" | "DROP DOMAIN" | "ALTER DOMAIN")
                    )
                {
                    if let Some(statement_sql) = statement_sql {
                        if let Some(compat_results) = self
                            .compat_role_membership_dependency_results(
                                session,
                                statement_sql,
                                statement,
                            )?
                        {
                            compat_effects_applied = true;
                            Some(self.collapse_portal_compat_results(
                                session,
                                compat_results,
                                "tracked type compatibility command",
                            )?)
                        } else {
                            self.with_session_mut(session, |record| {
                                super::compat::track_compat_types(record, statement_sql, statement);
                                Ok(())
                            })?;
                            compat_effects_applied = true;
                            Some(super::support::command_ok(
                                super::compat::statement_compat_tag(statement).unwrap_or("COMPAT"),
                            ))
                        }
                    } else {
                        None
                    }
                } else {
                    None
                };
            let compat_drop_if_exists_notice_result = if may_use_drop_if_exists_notice {
                if let Some(statement_sql) = statement_sql {
                    if let Some(compat_results) = self.compat_drop_if_exists_notice_results(
                        session,
                        statement_sql,
                        statement,
                    )? {
                        Some(self.collapse_portal_compat_results(
                            session,
                            compat_results,
                            "DROP IF EXISTS notice compatibility command",
                        )?)
                    } else {
                        None
                    }
                } else {
                    None
                }
            } else {
                None
            };
            let compat_rule_command_result = if uses_compat_command_hooks {
                if let Some(statement_sql) = statement_sql {
                    if let Some(compat_results) =
                        self.execute_compat_rule_command(session, statement_sql, statement)?
                    {
                        Some(self.collapse_portal_compat_results(
                            session,
                            compat_results,
                            "rule compatibility command",
                        )?)
                    } else {
                        None
                    }
                } else {
                    None
                }
            } else {
                None
            };
            let compat_rule_dml_result = if uses_compat_rule_dml {
                if let Some(compat_results) = self.execute_compat_rule_dml(session, statement)? {
                    Some(self.collapse_portal_compat_results(
                        session,
                        compat_results,
                        "rule compatibility DML",
                    )?)
                } else {
                    None
                }
            } else {
                None
            };
            let compat_router_result = if compat_prepared_result.is_none()
                && compat_do_result.is_none()
                && compat_revoke_role_result.is_none()
                && compat_cast_result.is_none()
                && compat_type_drop_if_exists_result.is_none()
                && compat_tracked_type_result.is_none()
                && compat_drop_if_exists_notice_result.is_none()
                && compat_rule_command_result.is_none()
                && compat_rule_dml_result.is_none()
                && (uses_compat_command_hooks
                    || uses_compat_rule_dml
                    || pg_object_command_is_planner_owned)
            {
                if let Some(statement_sql) = statement_sql {
                    match self.run_compat_router(
                        session,
                        statement_sql,
                        statement_sql,
                        statement,
                        uses_compat_command_hooks,
                        aiondb_pg_compat::disposition::classify(statement),
                    )? {
                        CompatHandlerPlan::Handled(compat_results) => {
                            Some(self.collapse_portal_compat_results(
                                session,
                                compat_results,
                                "portal compatibility router",
                            )?)
                        }
                        CompatHandlerPlan::Unhandled => None,
                    }
                } else {
                    None
                }
            } else {
                None
            };
            compat_effects_applied |= compat_router_result.is_some();
            let result = if let Some(result) = compat_prepared_result {
                result
            } else if let Some(result) = compat_do_result {
                result
            } else if let Some(result) = compat_revoke_role_result {
                result
            } else if let Some(result) = compat_cast_result {
                result
            } else if let Some(result) = compat_type_drop_if_exists_result {
                result
            } else if let Some(result) = compat_tracked_type_result {
                result
            } else if let Some(result) = compat_drop_if_exists_notice_result {
                result
            } else if let Some(result) = compat_rule_command_result {
                result
            } else if let Some(result) = compat_rule_dml_result {
                result
            } else if let Some(result) = compat_router_result {
                result
            } else {
                self.execute_portal_statement_core(
                    session,
                    statement,
                    statement_sql,
                    precomputed_plan_fingerprint,
                    contains_recursive_cte,
                    disable_plan_cache,
                    parameterized_plan_literal_rewrite,
                    parameterized_eq_literal_override.as_ref(),
                    parameterized_insert_values_literals_override.as_deref(),
                    position,
                    max_rows,
                    &mut executor_applied_portal_offset,
                )?
            };
            Ok(result)
        })();
        let mut result = match result {
            Ok(result) => result,
            Err(error) => {
                if !Self::portal_statement_uses_execute_statement(statement)
                    && !super::support::preserves_outer_transaction(&error)
                {
                    let _ = self.mark_transaction_failed_if_active(session);
                }
                return Err(error);
            }
        };
        if let Some(statement_sql) = statement_sql {
            if let Statement::CreateTableAs(create_table_as) = statement {
                if super::compat::extract_matview_source(statement_sql).is_some() {
                    if let Err(error) = self.persist_materialized_view_sidecar(
                        session,
                        statement_sql,
                        create_table_as,
                    ) {
                        let _ = self.mark_transaction_failed_if_active(session);
                        return Err(error);
                    }
                    if let StatementResult::Command { tag, .. } = &mut result {
                        *tag = "CREATE MATERIALIZED VIEW".to_owned();
                    }
                }
            }
            if matches!(statement, Statement::DropTable(_))
                && super::compat::is_drop_materialized_view_statement(statement_sql)
            {
                if let StatementResult::Command { tag, .. } = &mut result {
                    *tag = "DROP MATERIALIZED VIEW".to_owned();
                }
            }
        }
        if !compat_effects_applied
            && super::compat::statement_has_post_statement_compat_effects(statement)
        {
            if let Some(statement_sql) = statement_sql {
                if let Err(error) =
                    self.apply_post_statement_compat_effects(session, statement_sql, statement)
                {
                    let _ = self.mark_transaction_failed_if_active(session);
                    return Err(error);
                }
            }
        }
        let batch = match result {
            StatementResult::CopyIn { table_id, columns } => PortalBatch {
                columns: Vec::new(),
                rows: Vec::new(),
                tag: portal_batch_copy_in_tag(table_id, columns.len()),
                rows_affected: 0,
                exhausted: true,
            },
            StatementResult::CopyOut { data, column_count } => PortalBatch {
                columns: (0..column_count)
                    .map(|_| ResultColumn {
                        name: String::new(),
                        data_type: aiondb_core::DataType::Text,
                        text_type_modifier: None,
                        nullable: true,
                    })
                    .collect(),
                rows: vec![aiondb_core::Row::new(vec![aiondb_core::Value::Text(data)])],
                tag: PORTAL_BATCH_COPY_OUT_TAG.to_owned(),
                rows_affected: 0,
                exhausted: true,
            },
            StatementResult::Notice { message } => PortalBatch {
                columns: Vec::new(),
                rows: Vec::new(),
                tag: format!("NOTICE: {message}"),
                rows_affected: 0,
                exhausted: true,
            },
            StatementResult::Command { tag, rows_affected } => PortalBatch {
                columns: Vec::new(),
                rows: Vec::new(),
                tag,
                rows_affected,
                exhausted: true,
            },
            StatementResult::Query { columns, mut rows } => {
                if Self::portal_query_requires_single_execution_cache(statement) {
                    self.cache_portal_query_rows(session, portal_name, &columns, &rows)?;
                }
                let (next_position, exhausted) = if executor_applied_portal_offset {
                    let exhausted = max_rows == 0 || rows.len() <= requested_rows;
                    rows.truncate(requested_rows.min(rows.len()));
                    (position.saturating_add(rows.len()), exhausted)
                } else {
                    let total_rows = rows.len();
                    let start = position.min(total_rows);
                    let end = total_rows.min(start.saturating_add(requested_rows));
                    let exhausted = end >= total_rows;
                    rows = rows.split_off(start);
                    rows.truncate(requested_rows.min(rows.len()));
                    (end, exhausted)
                };

                if track_portal_state {
                    self.update_portal_progress(session, portal_name, next_position, exhausted)?;
                }

                PortalBatch {
                    columns,
                    rows,
                    tag: if exhausted {
                        query_completion_tag(completion_statement, next_position)
                    } else {
                        "SELECT".to_owned()
                    },
                    rows_affected: 0,
                    exhausted,
                }
            }
        };

        if track_portal_state && batch.exhausted {
            self.mark_portal_exhausted(session, portal_name)?;
        }
        Ok(batch)
    }

    fn mark_portal_exhausted(&self, session: &SessionHandle, portal_name: &str) -> DbResult<()> {
        self.with_session_mut(session, |record| {
            if let Some(portal) = record.portals.get_mut(portal_name) {
                portal.exhausted = true;
            }
            Ok(())
        })
    }

    fn update_portal_progress(
        &self,
        session: &SessionHandle,
        portal_name: &str,
        position: usize,
        exhausted: bool,
    ) -> DbResult<()> {
        self.with_session_mut(session, |record| {
            if let Some(portal) = record.portals.get_mut(portal_name) {
                portal.position = position;
                portal.exhausted = exhausted;
            }
            Ok(())
        })
    }
}

pub(super) fn query_completion_tag(statement: &Statement, row_count: usize) -> String {
    match statement {
        Statement::Explain { .. } => "EXPLAIN".to_owned(),
        Statement::ShowVariable(_) => "SHOW".to_owned(),
        Statement::CreateTableAs(_) => "CREATE TABLE".to_owned(),
        Statement::FetchStmt { .. } => format!("FETCH {row_count}"),
        Statement::Insert(insert) if !insert.returning.is_empty() => {
            format!("INSERT 0 {row_count}")
        }
        Statement::Update(update) if !update.returning.is_empty() => format!("UPDATE {row_count}"),
        Statement::Delete(delete) if !delete.returning.is_empty() => format!("DELETE {row_count}"),
        _ => format!("SELECT {row_count}"),
    }
}
