use aiondb_core::DbResult;
use aiondb_parser::{Expr, MergeAction, SelectStatement, Statement};

use super::{materialise_recursive_ctes, Engine, SessionHandle};

pub(super) fn rewrite_statement_in_place(
    engine: &Engine,
    session: &SessionHandle,
    statement: &mut Statement,
) -> DbResult<bool> {
    match statement {
        Statement::Select(select) => rewrite_select_in_place(engine, session, select),
        Statement::SetOperation(set_op) => {
            let mut changed = rewrite_statement_in_place(engine, session, set_op.left.as_mut())?;
            changed |= rewrite_statement_in_place(engine, session, set_op.right.as_mut())?;
            for item in &mut set_op.order_by {
                changed |= rewrite_expr_in_place(engine, session, &mut item.expr)?;
            }
            if let Some(limit) = &mut set_op.limit {
                changed |= rewrite_expr_in_place(engine, session, limit)?;
            }
            if let Some(offset) = &mut set_op.offset {
                changed |= rewrite_expr_in_place(engine, session, offset)?;
            }
            Ok(changed)
        }
        Statement::CreateTableAs(create_table_as) => {
            rewrite_select_in_place(engine, session, &mut create_table_as.query)
        }
        Statement::CreateView(create_view) => {
            rewrite_select_in_place(engine, session, &mut create_view.query)
        }
        Statement::Insert(insert) => {
            let mut changed = false;
            for row in &mut insert.rows {
                for expr in row {
                    changed |= rewrite_expr_in_place(engine, session, expr)?;
                }
            }
            if let Some(query) = &mut insert.query {
                changed |= rewrite_select_in_place(engine, session, query)?;
            }
            for item in &mut insert.returning {
                changed |= rewrite_expr_in_place(engine, session, &mut item.expr)?;
            }
            Ok(changed)
        }
        Statement::Delete(delete) => {
            let mut changed = false;
            if let Some(selection) = &mut delete.selection {
                changed |= rewrite_expr_in_place(engine, session, selection)?;
            }
            for item in &mut delete.returning {
                changed |= rewrite_expr_in_place(engine, session, &mut item.expr)?;
            }
            Ok(changed)
        }
        Statement::Update(update) => {
            let mut changed = false;
            for assignment in &mut update.assignments {
                changed |= rewrite_expr_in_place(engine, session, &mut assignment.expr)?;
            }
            if let Some(selection) = &mut update.selection {
                changed |= rewrite_expr_in_place(engine, session, selection)?;
            }
            for item in &mut update.returning {
                changed |= rewrite_expr_in_place(engine, session, &mut item.expr)?;
            }
            Ok(changed)
        }
        Statement::Merge(merge) => {
            let mut changed = rewrite_expr_in_place(engine, session, &mut merge.on_condition)?;
            for when in &mut merge.when_clauses {
                if let Some(condition) = &mut when.condition {
                    changed |= rewrite_expr_in_place(engine, session, condition)?;
                }
                if let MergeAction::Update { assignments } = &mut when.action {
                    for assignment in assignments {
                        changed |= rewrite_expr_in_place(engine, session, &mut assignment.expr)?;
                    }
                } else if let MergeAction::Insert { values, .. } = &mut when.action {
                    for value in values {
                        changed |= rewrite_expr_in_place(engine, session, value)?;
                    }
                }
            }
            Ok(changed)
        }
        Statement::Explain { statement, .. } => {
            rewrite_statement_in_place(engine, session, statement.as_mut())
        }
        _ => Ok(false),
    }
}

fn rewrite_select_in_place(
    engine: &Engine,
    session: &SessionHandle,
    select: &mut SelectStatement,
) -> DbResult<bool> {
    let mut changed = false;

    for cte in &mut select.ctes {
        changed |= rewrite_statement_in_place(engine, session, cte.query.as_mut())?;
        if let Some(recursive_term) = &mut cte.recursive_term {
            changed |= rewrite_select_in_place(engine, session, recursive_term.as_mut())?;
        }
    }

    for item in &mut select.items {
        changed |= rewrite_expr_in_place(engine, session, &mut item.expr)?;
    }
    for join in &mut select.joins {
        if let Some(condition) = &mut join.condition {
            changed |= rewrite_expr_in_place(engine, session, condition)?;
        }
    }
    if let Some(selection) = &mut select.selection {
        changed |= rewrite_expr_in_place(engine, session, selection)?;
    }
    for expr in &mut select.group_by {
        changed |= rewrite_expr_in_place(engine, session, expr)?;
    }
    if let Some(having) = &mut select.having {
        changed |= rewrite_expr_in_place(engine, session, having)?;
    }
    for window in &mut select.window_definitions {
        for expr in &mut window.partition_by {
            changed |= rewrite_expr_in_place(engine, session, expr)?;
        }
        for item in &mut window.order_by {
            changed |= rewrite_expr_in_place(engine, session, &mut item.expr)?;
        }
    }
    for item in &mut select.order_by {
        changed |= rewrite_expr_in_place(engine, session, &mut item.expr)?;
    }
    if let Some(limit) = &mut select.limit {
        changed |= rewrite_expr_in_place(engine, session, limit)?;
    }
    if let Some(offset) = &mut select.offset {
        changed |= rewrite_expr_in_place(engine, session, offset)?;
    }

    if select.ctes.iter().any(|cte| cte.recursive_term.is_some()) {
        materialise_recursive_ctes(engine, session, select)?;
        changed = true;
    }

    Ok(changed)
}

fn rewrite_expr_in_place(
    engine: &Engine,
    session: &SessionHandle,
    expr: &mut Expr,
) -> DbResult<bool> {
    match expr {
        Expr::Literal(_, _)
        | Expr::Identifier(_)
        | Expr::Parameter { .. }
        | Expr::Default { .. }
        | Expr::CypherExists { .. }
        | Expr::CypherPatternComprehension { .. } => Ok(false),
        Expr::FunctionCall { args, filter, .. } => {
            let mut changed = false;
            for arg in args {
                changed |= rewrite_expr_in_place(engine, session, arg)?;
            }
            if let Some(filter) = filter {
                changed |= rewrite_expr_in_place(engine, session, filter.as_mut())?;
            }
            Ok(changed)
        }
        Expr::UnaryOp { expr, .. } | Expr::IsNull { expr, .. } | Expr::Cast { expr, .. } => {
            rewrite_expr_in_place(engine, session, expr.as_mut())
        }
        Expr::BinaryOp { left, right, .. }
        | Expr::IsDistinctFrom { left, right, .. }
        | Expr::Like {
            expr: left,
            pattern: right,
            ..
        } => {
            let mut changed = rewrite_expr_in_place(engine, session, left.as_mut())?;
            changed |= rewrite_expr_in_place(engine, session, right.as_mut())?;
            Ok(changed)
        }
        Expr::InList { expr, list, .. } => {
            let mut changed = rewrite_expr_in_place(engine, session, expr.as_mut())?;
            for item in list {
                changed |= rewrite_expr_in_place(engine, session, item)?;
            }
            Ok(changed)
        }
        Expr::Between {
            expr, low, high, ..
        } => {
            let mut changed = rewrite_expr_in_place(engine, session, expr.as_mut())?;
            changed |= rewrite_expr_in_place(engine, session, low.as_mut())?;
            changed |= rewrite_expr_in_place(engine, session, high.as_mut())?;
            Ok(changed)
        }
        Expr::CaseWhen {
            operand,
            conditions,
            results,
            else_result,
            ..
        } => {
            let mut changed = false;
            if let Some(operand) = operand {
                changed |= rewrite_expr_in_place(engine, session, operand.as_mut())?;
            }
            for condition in conditions {
                changed |= rewrite_expr_in_place(engine, session, condition)?;
            }
            for result in results {
                changed |= rewrite_expr_in_place(engine, session, result)?;
            }
            if let Some(else_result) = else_result {
                changed |= rewrite_expr_in_place(engine, session, else_result.as_mut())?;
            }
            Ok(changed)
        }
        Expr::Array { elements, .. } => {
            let mut changed = false;
            for element in elements {
                changed |= rewrite_expr_in_place(engine, session, element)?;
            }
            Ok(changed)
        }
        Expr::ArraySubquery { query, .. } | Expr::Subquery { query, .. } => {
            rewrite_select_in_place(engine, session, query.as_mut())
        }
        Expr::InSubquery { expr, query, .. } => {
            let mut changed = rewrite_expr_in_place(engine, session, expr.as_mut())?;
            changed |= rewrite_select_in_place(engine, session, query.as_mut())?;
            Ok(changed)
        }
        Expr::Exists { query, .. } => rewrite_select_in_place(engine, session, query.as_mut()),
        Expr::WindowFunction {
            function,
            partition_by,
            order_by,
            ..
        } => {
            let mut changed = rewrite_expr_in_place(engine, session, function.as_mut())?;
            for expr in partition_by {
                changed |= rewrite_expr_in_place(engine, session, expr)?;
            }
            for item in order_by {
                changed |= rewrite_expr_in_place(engine, session, &mut item.expr)?;
            }
            Ok(changed)
        }
    }
}
