//! Pure AST-level analysis of data-modifying CTEs and relation references
//! used by the compat WITH-DML validator. These helpers walk the parser
//! AST and report whether a statement/expression references a given
//! relation - the engine uses the results to decide whether to emit
//! PostgreSQL-compatible errors before running the query.

use std::cell::Cell;

use aiondb_parser::{CteDefinition, Expr, ObjectName, SelectStatement, Span, Statement};

use crate::rewrite::object_name_matches_relation_name;

const MAX_DML_VALIDATION_AST_DEPTH: usize = 512;

thread_local! {
    static DML_VALIDATION_AST_DEPTH: Cell<usize> = const { Cell::new(0) };
}

struct DmlValidationAstDepthGuard;

impl DmlValidationAstDepthGuard {
    fn enter() -> Option<Self> {
        DML_VALIDATION_AST_DEPTH.with(|depth| {
            let current = depth.get();
            if current >= MAX_DML_VALIDATION_AST_DEPTH {
                None
            } else {
                depth.set(current + 1);
                Some(Self)
            }
        })
    }
}

impl Drop for DmlValidationAstDepthGuard {
    fn drop(&mut self) {
        DML_VALIDATION_AST_DEPTH.with(|depth| depth.set(depth.get().saturating_sub(1)));
    }
}

pub fn statement_contains_data_modifying_cte(statement: &Statement) -> bool {
    let Some(_guard) = DmlValidationAstDepthGuard::enter() else {
        return false;
    };
    match statement {
        Statement::Select(select) => select_contains_data_modifying_cte(select),
        Statement::SetOperation(set_op) => {
            statement_contains_data_modifying_cte(set_op.left.as_ref())
                || statement_contains_data_modifying_cte(set_op.right.as_ref())
        }
        Statement::Insert(insert) => insert
            .query
            .as_ref()
            .is_some_and(select_contains_data_modifying_cte),
        Statement::CreateTableAs(create_table_as) => {
            select_contains_data_modifying_cte(&create_table_as.query)
        }
        Statement::CreateView(create_view) => {
            select_contains_data_modifying_cte(&create_view.query)
        }
        Statement::Explain { statement, .. } => {
            statement_contains_data_modifying_cte(statement.as_ref())
        }
        _ => false,
    }
}

pub fn select_contains_data_modifying_cte(select: &SelectStatement) -> bool {
    let Some(_guard) = DmlValidationAstDepthGuard::enter() else {
        return false;
    };
    select.ctes.iter().any(|cte| {
        matches!(
            cte.query.as_ref(),
            Statement::Insert(_) | Statement::Update(_) | Statement::Delete(_)
        ) || statement_contains_data_modifying_cte(cte.query.as_ref())
            || cte
                .recursive_term
                .as_ref()
                .is_some_and(|term| select_contains_data_modifying_cte(term))
    })
}

pub fn cte_data_modifying_info(cte: &CteDefinition) -> Option<(&'static str, &ObjectName, bool)> {
    match cte.query.as_ref() {
        Statement::Insert(insert) => Some(("INSERT", &insert.table, !insert.returning.is_empty())),
        Statement::Update(update) => Some(("UPDATE", &update.table, !update.returning.is_empty())),
        Statement::Delete(delete) => Some(("DELETE", &delete.table, !delete.returning.is_empty())),
        _ => None,
    }
}

pub fn find_relation_reference_in_statement(
    statement: &Statement,
    relation_name: &str,
) -> Option<Span> {
    let _guard = DmlValidationAstDepthGuard::enter()?;
    match statement {
        Statement::Select(select) => find_relation_reference_in_select(select, relation_name),
        Statement::SetOperation(set_op) => {
            find_relation_reference_in_statement(set_op.left.as_ref(), relation_name)
                .or_else(|| {
                    find_relation_reference_in_statement(set_op.right.as_ref(), relation_name)
                })
                .or_else(|| {
                    set_op
                        .order_by
                        .iter()
                        .find_map(|item| find_relation_reference_in_expr(&item.expr, relation_name))
                })
                .or_else(|| {
                    set_op
                        .limit
                        .as_ref()
                        .and_then(|expr| find_relation_reference_in_expr(expr, relation_name))
                })
                .or_else(|| {
                    set_op
                        .offset
                        .as_ref()
                        .and_then(|expr| find_relation_reference_in_expr(expr, relation_name))
                })
        }
        Statement::Insert(insert) => {
            for row in &insert.rows {
                for expr in row {
                    if let Some(span) = find_relation_reference_in_expr(expr, relation_name) {
                        return Some(span);
                    }
                }
            }
            insert
                .query
                .as_ref()
                .and_then(|query| find_relation_reference_in_select(query, relation_name))
        }
        Statement::Update(update) => {
            if let Some(span) = update.from_tables.iter().find_map(|(table, _)| {
                object_name_matches_relation_name(table, relation_name).then_some(table.span)
            }) {
                return Some(span);
            }
            if let Some(span) = update.assignments.iter().find_map(|assignment| {
                find_relation_reference_in_expr(&assignment.expr, relation_name)
            }) {
                return Some(span);
            }
            update
                .selection
                .as_ref()
                .and_then(|expr| find_relation_reference_in_expr(expr, relation_name))
        }
        Statement::Delete(delete) => {
            if let Some(span) = delete.using_tables.iter().find_map(|(table, _)| {
                object_name_matches_relation_name(table, relation_name).then_some(table.span)
            }) {
                return Some(span);
            }
            delete
                .selection
                .as_ref()
                .and_then(|expr| find_relation_reference_in_expr(expr, relation_name))
        }
        _ => None,
    }
}

pub fn find_relation_reference_in_select(
    select: &SelectStatement,
    relation_name: &str,
) -> Option<Span> {
    let _guard = DmlValidationAstDepthGuard::enter()?;
    for cte in &select.ctes {
        if let Some(span) = find_relation_reference_in_statement(cte.query.as_ref(), relation_name)
        {
            return Some(span);
        }
    }
    find_relation_reference_in_select_body(select, relation_name)
}

pub fn find_relation_reference_in_select_body(
    select: &SelectStatement,
    relation_name: &str,
) -> Option<Span> {
    let _guard = DmlValidationAstDepthGuard::enter()?;
    if let Some(from) = &select.from {
        if object_name_matches_relation_name(from, relation_name) {
            return Some(from.span);
        }
    }
    if let Some(span) = select.joins.iter().find_map(|join| {
        object_name_matches_relation_name(&join.table, relation_name).then_some(join.table.span)
    }) {
        return Some(span);
    }
    for item in &select.items {
        if let Some(span) = find_relation_reference_in_expr(&item.expr, relation_name) {
            return Some(span);
        }
    }
    if let Some(selection) = &select.selection {
        if let Some(span) = find_relation_reference_in_expr(selection, relation_name) {
            return Some(span);
        }
    }
    for expr in &select.group_by {
        if let Some(span) = find_relation_reference_in_expr(expr, relation_name) {
            return Some(span);
        }
    }
    if let Some(having) = &select.having {
        if let Some(span) = find_relation_reference_in_expr(having, relation_name) {
            return Some(span);
        }
    }
    for window in &select.window_definitions {
        for expr in &window.partition_by {
            if let Some(span) = find_relation_reference_in_expr(expr, relation_name) {
                return Some(span);
            }
        }
        for item in &window.order_by {
            if let Some(span) = find_relation_reference_in_expr(&item.expr, relation_name) {
                return Some(span);
            }
        }
    }
    for item in &select.order_by {
        if let Some(span) = find_relation_reference_in_expr(&item.expr, relation_name) {
            return Some(span);
        }
    }
    if let Some(limit) = &select.limit {
        if let Some(span) = find_relation_reference_in_expr(limit, relation_name) {
            return Some(span);
        }
    }
    if let Some(offset) = &select.offset {
        if let Some(span) = find_relation_reference_in_expr(offset, relation_name) {
            return Some(span);
        }
    }
    None
}

pub fn find_relation_reference_in_expr(expr: &Expr, relation_name: &str) -> Option<Span> {
    let _guard = DmlValidationAstDepthGuard::enter()?;
    match expr {
        Expr::ArraySubquery { query, .. }
        | Expr::Subquery { query, .. }
        | Expr::Exists { query, .. } => find_relation_reference_in_select(query, relation_name),
        Expr::InSubquery { expr, query, .. } => {
            find_relation_reference_in_expr(expr, relation_name)
                .or_else(|| find_relation_reference_in_select(query, relation_name))
        }
        Expr::FunctionCall { args, filter, .. } => args
            .iter()
            .find_map(|arg| find_relation_reference_in_expr(arg, relation_name))
            .or_else(|| {
                filter
                    .as_ref()
                    .and_then(|expr| find_relation_reference_in_expr(expr, relation_name))
            }),
        Expr::UnaryOp { expr, .. } | Expr::IsNull { expr, .. } | Expr::Cast { expr, .. } => {
            find_relation_reference_in_expr(expr, relation_name)
        }
        Expr::BinaryOp { left, right, .. } | Expr::IsDistinctFrom { left, right, .. } => {
            find_relation_reference_in_expr(left, relation_name)
                .or_else(|| find_relation_reference_in_expr(right, relation_name))
        }
        Expr::Like { expr, pattern, .. } => find_relation_reference_in_expr(expr, relation_name)
            .or_else(|| find_relation_reference_in_expr(pattern, relation_name)),
        Expr::InList { expr, list, .. } => find_relation_reference_in_expr(expr, relation_name)
            .or_else(|| {
                list.iter()
                    .find_map(|item| find_relation_reference_in_expr(item, relation_name))
            }),
        Expr::Between {
            expr, low, high, ..
        } => find_relation_reference_in_expr(expr, relation_name)
            .or_else(|| find_relation_reference_in_expr(low, relation_name))
            .or_else(|| find_relation_reference_in_expr(high, relation_name)),
        Expr::CaseWhen {
            operand,
            conditions,
            results,
            else_result,
            ..
        } => operand
            .as_ref()
            .and_then(|expr| find_relation_reference_in_expr(expr, relation_name))
            .or_else(|| {
                conditions
                    .iter()
                    .find_map(|condition| find_relation_reference_in_expr(condition, relation_name))
            })
            .or_else(|| {
                results
                    .iter()
                    .find_map(|result| find_relation_reference_in_expr(result, relation_name))
            })
            .or_else(|| {
                else_result
                    .as_ref()
                    .and_then(|expr| find_relation_reference_in_expr(expr, relation_name))
            }),
        Expr::Array { elements, .. } => elements
            .iter()
            .find_map(|element| find_relation_reference_in_expr(element, relation_name)),
        Expr::WindowFunction {
            function,
            partition_by,
            order_by,
            ..
        } => find_relation_reference_in_expr(function, relation_name)
            .or_else(|| {
                partition_by
                    .iter()
                    .find_map(|expr| find_relation_reference_in_expr(expr, relation_name))
            })
            .or_else(|| {
                order_by
                    .iter()
                    .find_map(|item| find_relation_reference_in_expr(&item.expr, relation_name))
            }),
        Expr::Literal(..)
        | Expr::Identifier(..)
        | Expr::Default { .. }
        | Expr::Parameter { .. }
        | Expr::CypherExists { .. }
        | Expr::CypherPatternComprehension { .. } => None,
    }
}
