//! Iterative evaluation of `WITH RECURSIVE` CTEs.
//!
//! The strategy is:
//!   1. For each recursive CTE in the statement, compute all rows via a
//!      worktable fixpoint loop.
//!   2. Replace the recursive CTE definition with a plain (non-recursive)
//!      CTE whose body is `SELECT col1, col2, ... FROM (VALUES (...), ...)`
//!      containing the precomputed rows.
//!   3. Execute the rewritten statement through the normal planner pipeline.

#![allow(
    clippy::items_after_statements,
    clippy::match_same_arms,
    clippy::needless_pass_by_value,
    clippy::redundant_closure_for_method_calls,
    clippy::semicolon_if_nothing_returned,
    clippy::too_many_lines
)]

use aiondb_core::{
    convert::usize_to_u64_saturating as usize_to_u64, DataType, DbError, DbResult, Row, Value,
};
use aiondb_parser::{
    CteDefinition, DistinctKind, Expr, Literal, MergeAction, MergeSource, ObjectName, SelectItem,
    SelectStatement, SetOperationStatement, SetOperationType, Span, Statement,
};
use std::time::Instant;

use crate::prepared::{ResultColumn, StatementResult};

use super::{Engine, SessionHandle};
#[path = "recursive_cte_rewrite.rs"]
mod recursive_cte_rewrite;
use recursive_cte_rewrite::rewrite_statement_in_place;

// Default limits for recursive CTEs are defined in
// `aiondb_config::runtime::{DEFAULT_LIMITS_MAX_RECURSIVE_ITERATIONS, DEFAULT_LIMITS_MAX_RECURSIVE_ROWS}`.

const RECURSIVE_ROW_OVERHEAD_BYTES: u64 = 64;
const RECURSIVE_SEEN_KEY_OVERHEAD_BYTES: u64 = 32;
const RECURSIVE_SYNTHETIC_VALUES_ESTIMATED_ROW_BYTES: u64 = 256;
const RECURSIVE_SYNTHETIC_VALUES_MAX_ROWS: u64 = 250_000;
const RECURSIVE_SYNTHETIC_UNION_MAX_DEPTH: usize = 64;
const RECURSIVE_VALUE_ESTIMATE_MAX_DEPTH: usize = 256;
const RECURSIVE_DEEP_VALUE_ESTIMATED_BYTES: u64 = 1 << 20;

/// Rewrite any recursive CTEs reachable from `statement` into non-recursive,
/// materialized CTEs before normal planning/execution.
///
/// Returns `Some(rewritten_statement)` only when at least one recursive CTE
/// was found and replaced somewhere in the statement tree.
pub(super) fn rewrite_statement_for_execution(
    engine: &Engine,
    session: &SessionHandle,
    statement: &Statement,
) -> DbResult<Option<Statement>> {
    // Fast path: most statements do not include recursive CTEs.
    // Avoid cloning the full AST unless a recursive subtree is present.
    if !statement_contains_recursive_cte(statement) {
        return Ok(None);
    }

    let mut rewritten = statement.clone();
    if rewrite_statement_in_place(engine, session, &mut rewritten)? {
        Ok(Some(rewritten))
    } else {
        Ok(None)
    }
}

/// Convenience wrapper: returns a `Cow` that borrows the original statement
/// when no rewrite was needed, or owns the rewritten copy otherwise.
pub(super) fn maybe_rewrite_for_execution<'a>(
    engine: &Engine,
    session: &SessionHandle,
    statement: &'a Statement,
) -> DbResult<std::borrow::Cow<'a, Statement>> {
    match rewrite_statement_for_execution(engine, session, statement)? {
        Some(rewritten) => Ok(std::borrow::Cow::Owned(rewritten)),
        None => Ok(std::borrow::Cow::Borrowed(statement)),
    }
}

enum RecursiveCteWork<'a> {
    Statement(&'a Statement),
    Select(&'a SelectStatement),
    Expr(&'a Expr),
}

pub(super) fn statement_contains_recursive_cte(statement: &Statement) -> bool {
    contains_recursive_cte_work([RecursiveCteWork::Statement(statement)])
}

fn contains_recursive_cte_work<'a>(
    initial: impl IntoIterator<Item = RecursiveCteWork<'a>>,
) -> bool {
    let mut stack: Vec<RecursiveCteWork<'a>> = initial.into_iter().collect();
    while let Some(work) = stack.pop() {
        match work {
            RecursiveCteWork::Statement(statement) => {
                push_statement_recursive_cte_work(statement, &mut stack);
            }
            RecursiveCteWork::Select(select) => {
                if select.ctes.iter().any(|cte| cte.recursive_term.is_some()) {
                    return true;
                }
                push_select_recursive_cte_work(select, &mut stack);
            }
            RecursiveCteWork::Expr(expr) => push_expr_recursive_cte_work(expr, &mut stack),
        }
    }
    false
}

fn push_statement_recursive_cte_work<'a>(
    statement: &'a Statement,
    stack: &mut Vec<RecursiveCteWork<'a>>,
) {
    match statement {
        Statement::Select(select) => stack.push(RecursiveCteWork::Select(select)),
        Statement::SetOperation(set_op) => {
            stack.push(RecursiveCteWork::Statement(set_op.right.as_ref()));
            stack.push(RecursiveCteWork::Statement(set_op.left.as_ref()));
            for item in &set_op.order_by {
                stack.push(RecursiveCteWork::Expr(&item.expr));
            }
            if let Some(limit) = &set_op.limit {
                stack.push(RecursiveCteWork::Expr(limit));
            }
            if let Some(offset) = &set_op.offset {
                stack.push(RecursiveCteWork::Expr(offset));
            }
        }
        Statement::CreateTableAs(create_table_as) => {
            stack.push(RecursiveCteWork::Select(&create_table_as.query));
        }
        Statement::CreateView(create_view) => {
            stack.push(RecursiveCteWork::Select(&create_view.query))
        }
        Statement::Insert(insert) => {
            for expr in insert.rows.iter().flatten() {
                stack.push(RecursiveCteWork::Expr(expr));
            }
            if let Some(query) = &insert.query {
                stack.push(RecursiveCteWork::Select(query));
            }
            for item in &insert.returning {
                stack.push(RecursiveCteWork::Expr(&item.expr));
            }
        }
        Statement::Delete(delete) => {
            if let Some(selection) = &delete.selection {
                stack.push(RecursiveCteWork::Expr(selection));
            }
            for item in &delete.returning {
                stack.push(RecursiveCteWork::Expr(&item.expr));
            }
        }
        Statement::Update(update) => {
            for assignment in &update.assignments {
                stack.push(RecursiveCteWork::Expr(&assignment.expr));
            }
            if let Some(selection) = &update.selection {
                stack.push(RecursiveCteWork::Expr(selection));
            }
            for item in &update.returning {
                stack.push(RecursiveCteWork::Expr(&item.expr));
            }
        }
        Statement::Merge(merge) => {
            if let MergeSource::Subquery(query) = &merge.source {
                stack.push(RecursiveCteWork::Select(query));
            }
            stack.push(RecursiveCteWork::Expr(&merge.on_condition));
            for when in &merge.when_clauses {
                if let Some(condition) = &when.condition {
                    stack.push(RecursiveCteWork::Expr(condition));
                }
                match &when.action {
                    MergeAction::Update { assignments } => {
                        for assignment in assignments {
                            stack.push(RecursiveCteWork::Expr(&assignment.expr));
                        }
                    }
                    MergeAction::Insert { values, .. } => {
                        stack.extend(values.iter().map(RecursiveCteWork::Expr));
                    }
                    _ => {}
                }
            }
        }
        Statement::Explain { statement, .. } => stack.push(RecursiveCteWork::Statement(statement)),
        _ => {}
    }
}

fn push_select_recursive_cte_work<'a>(
    select: &'a SelectStatement,
    stack: &mut Vec<RecursiveCteWork<'a>>,
) {
    for cte in &select.ctes {
        stack.push(RecursiveCteWork::Statement(cte.query.as_ref()));
        if let Some(recursive_term) = &cte.recursive_term {
            stack.push(RecursiveCteWork::Select(recursive_term));
        }
    }
    for item in &select.items {
        stack.push(RecursiveCteWork::Expr(&item.expr));
    }
    for join in &select.joins {
        if let Some(condition) = &join.condition {
            stack.push(RecursiveCteWork::Expr(condition));
        }
    }
    if let Some(selection) = &select.selection {
        stack.push(RecursiveCteWork::Expr(selection));
    }
    stack.extend(select.group_by.iter().map(RecursiveCteWork::Expr));
    if let Some(having) = &select.having {
        stack.push(RecursiveCteWork::Expr(having));
    }
    for window in &select.window_definitions {
        stack.extend(window.partition_by.iter().map(RecursiveCteWork::Expr));
        for item in &window.order_by {
            stack.push(RecursiveCteWork::Expr(&item.expr));
        }
    }
    for item in &select.order_by {
        stack.push(RecursiveCteWork::Expr(&item.expr));
    }
    if let Some(limit) = &select.limit {
        stack.push(RecursiveCteWork::Expr(limit));
    }
    if let Some(offset) = &select.offset {
        stack.push(RecursiveCteWork::Expr(offset));
    }
}

fn push_expr_recursive_cte_work<'a>(expr: &'a Expr, stack: &mut Vec<RecursiveCteWork<'a>>) {
    match expr {
        Expr::Literal(_, _)
        | Expr::Identifier(_)
        | Expr::Parameter { .. }
        | Expr::Default { .. }
        | Expr::CypherExists { .. }
        | Expr::CypherPatternComprehension { .. } => {}
        Expr::FunctionCall { args, filter, .. } => {
            stack.extend(args.iter().map(RecursiveCteWork::Expr));
            if let Some(filter) = filter {
                stack.push(RecursiveCteWork::Expr(filter));
            }
        }
        Expr::UnaryOp { expr, .. } | Expr::IsNull { expr, .. } | Expr::Cast { expr, .. } => {
            stack.push(RecursiveCteWork::Expr(expr));
        }
        Expr::BinaryOp { left, right, .. }
        | Expr::IsDistinctFrom { left, right, .. }
        | Expr::Like {
            expr: left,
            pattern: right,
            ..
        } => {
            stack.push(RecursiveCteWork::Expr(right));
            stack.push(RecursiveCteWork::Expr(left));
        }
        Expr::InList { expr, list, .. } => {
            stack.extend(list.iter().map(RecursiveCteWork::Expr));
            stack.push(RecursiveCteWork::Expr(expr));
        }
        Expr::Between {
            expr, low, high, ..
        } => {
            stack.push(RecursiveCteWork::Expr(high));
            stack.push(RecursiveCteWork::Expr(low));
            stack.push(RecursiveCteWork::Expr(expr));
        }
        Expr::CaseWhen {
            operand,
            conditions,
            results,
            else_result,
            ..
        } => {
            if let Some(operand) = operand {
                stack.push(RecursiveCteWork::Expr(operand));
            }
            stack.extend(conditions.iter().map(RecursiveCteWork::Expr));
            stack.extend(results.iter().map(RecursiveCteWork::Expr));
            if let Some(else_result) = else_result {
                stack.push(RecursiveCteWork::Expr(else_result));
            }
        }
        Expr::Array { elements, .. } => {
            stack.extend(elements.iter().map(RecursiveCteWork::Expr));
        }
        Expr::ArraySubquery { query, .. } | Expr::Subquery { query, .. } => {
            stack.push(RecursiveCteWork::Select(query));
        }
        Expr::InSubquery { expr, query, .. } => {
            stack.push(RecursiveCteWork::Select(query));
            stack.push(RecursiveCteWork::Expr(expr));
        }
        Expr::Exists { query, .. } => stack.push(RecursiveCteWork::Select(query)),
        Expr::WindowFunction {
            function,
            partition_by,
            order_by,
            ..
        } => {
            stack.push(RecursiveCteWork::Expr(function));
            stack.extend(partition_by.iter().map(RecursiveCteWork::Expr));
            for item in order_by {
                stack.push(RecursiveCteWork::Expr(&item.expr));
            }
        }
    }
}

/// Evaluate all recursive CTEs in `select` and replace their definitions
/// with non-recursive versions containing the precomputed rows.
///
/// This is also used by the portal path which needs to rewrite the
/// statement in place before entering the standard plan-execute pipeline.
pub(super) fn materialise_recursive_ctes(
    engine: &Engine,
    session: &SessionHandle,
    select: &mut SelectStatement,
) -> DbResult<()> {
    for i in 0..select.ctes.len() {
        if select.ctes[i].recursive_term.is_some() {
            let cte = select.ctes[i].clone();
            if !cte_references_self(&cte) {
                select.ctes[i] = fold_non_self_recursive_cte(cte);
                continue;
            }
            let (rows, col_names, col_types) =
                evaluate_recursive_cte(engine, session, &cte, select)?;
            let max_synthetic_values_rows = recursive_synthetic_values_row_cap(
                engine.session_info(session)?.limits.max_memory_bytes,
            );
            select.ctes[i] =
                materialise_cte(&cte, rows, col_names, col_types, max_synthetic_values_rows)?;
        }
    }
    Ok(())
}

fn fold_non_self_recursive_cte(cte: CteDefinition) -> CteDefinition {
    let CteDefinition {
        name,
        column_aliases,
        recursive,
        query,
        recursive_term,
        union_all,
        span,
    } = cte;
    let Some(recursive_term) = recursive_term else {
        return CteDefinition {
            name,
            column_aliases,
            recursive,
            query,
            recursive_term: None,
            union_all,
            span,
        };
    };

    CteDefinition {
        name,
        column_aliases,
        recursive,
        query: Box::new(Statement::SetOperation(SetOperationStatement {
            op: SetOperationType::Union,
            all: union_all,
            left: query,
            right: Box::new(Statement::Select(*recursive_term)),
            order_by: Vec::new(),
            order_by_span: None,
            limit: None,
            limit_span: None,
            offset: None,
            offset_span: None,
            span,
        })),
        recursive_term: None,
        union_all: false,
        span,
    }
}

fn cte_references_self(cte: &CteDefinition) -> bool {
    cte.recursive_term
        .as_ref()
        .is_some_and(|term| select_references_cte_name(term, &cte.name))
}

fn select_references_cte_name(select: &SelectStatement, cte_name: &str) -> bool {
    if select
        .ctes
        .iter()
        .any(|cte| cte.name.eq_ignore_ascii_case(cte_name))
    {
        return false;
    }

    select
        .from
        .as_ref()
        .is_some_and(|from| object_name_matches_recursive_cte(from, cte_name))
        || select
            .joins
            .iter()
            .any(|join| object_name_matches_recursive_cte(&join.table, cte_name))
        || select.ctes.iter().any(|cte| {
            statement_references_cte_name(cte.query.as_ref(), cte_name)
                || cte
                    .recursive_term
                    .as_ref()
                    .is_some_and(|term| select_references_cte_name(term, cte_name))
        })
        || select
            .items
            .iter()
            .any(|item| expr_references_cte_name(&item.expr, cte_name))
        || select.joins.iter().any(|join| {
            join.condition
                .as_ref()
                .is_some_and(|expr| expr_references_cte_name(expr, cte_name))
        })
        || select
            .selection
            .as_ref()
            .is_some_and(|expr| expr_references_cte_name(expr, cte_name))
        || select
            .group_by
            .iter()
            .any(|expr| expr_references_cte_name(expr, cte_name))
        || select
            .having
            .as_ref()
            .is_some_and(|expr| expr_references_cte_name(expr, cte_name))
        || select.window_definitions.iter().any(|window| {
            window
                .partition_by
                .iter()
                .any(|expr| expr_references_cte_name(expr, cte_name))
                || window
                    .order_by
                    .iter()
                    .any(|item| expr_references_cte_name(&item.expr, cte_name))
        })
        || select
            .order_by
            .iter()
            .any(|item| expr_references_cte_name(&item.expr, cte_name))
        || select
            .limit
            .as_ref()
            .is_some_and(|expr| expr_references_cte_name(expr, cte_name))
        || select
            .offset
            .as_ref()
            .is_some_and(|expr| expr_references_cte_name(expr, cte_name))
}

fn statement_references_cte_name(statement: &Statement, cte_name: &str) -> bool {
    match statement {
        Statement::Select(select) => select_references_cte_name(select, cte_name),
        Statement::SetOperation(set_op) => {
            statement_references_cte_name(set_op.left.as_ref(), cte_name)
                || statement_references_cte_name(set_op.right.as_ref(), cte_name)
                || set_op
                    .order_by
                    .iter()
                    .any(|item| expr_references_cte_name(&item.expr, cte_name))
                || set_op
                    .limit
                    .as_ref()
                    .is_some_and(|expr| expr_references_cte_name(expr, cte_name))
                || set_op
                    .offset
                    .as_ref()
                    .is_some_and(|expr| expr_references_cte_name(expr, cte_name))
        }
        Statement::Explain { statement, .. } => {
            statement_references_cte_name(statement.as_ref(), cte_name)
        }
        Statement::CreateTableAs(create_table_as) => {
            select_references_cte_name(&create_table_as.query, cte_name)
        }
        Statement::CreateView(create_view) => {
            select_references_cte_name(&create_view.query, cte_name)
        }
        Statement::Insert(insert) => {
            insert
                .rows
                .iter()
                .flatten()
                .any(|expr| expr_references_cte_name(expr, cte_name))
                || insert
                    .query
                    .as_ref()
                    .is_some_and(|query| select_references_cte_name(query, cte_name))
                || insert
                    .returning
                    .iter()
                    .any(|item| expr_references_cte_name(&item.expr, cte_name))
        }
        Statement::Delete(delete) => {
            delete
                .selection
                .as_ref()
                .is_some_and(|expr| expr_references_cte_name(expr, cte_name))
                || delete
                    .returning
                    .iter()
                    .any(|item| expr_references_cte_name(&item.expr, cte_name))
        }
        Statement::Update(update) => {
            update
                .assignments
                .iter()
                .any(|assignment| expr_references_cte_name(&assignment.expr, cte_name))
                || update
                    .selection
                    .as_ref()
                    .is_some_and(|expr| expr_references_cte_name(expr, cte_name))
                || update
                    .returning
                    .iter()
                    .any(|item| expr_references_cte_name(&item.expr, cte_name))
        }
        Statement::Merge(merge) => {
            expr_references_cte_name(&merge.on_condition, cte_name)
                || merge.when_clauses.iter().any(|when| {
                    when.condition
                        .as_ref()
                        .is_some_and(|condition| expr_references_cte_name(condition, cte_name))
                        || match &when.action {
                            MergeAction::Update { assignments } => {
                                assignments.iter().any(|assignment| {
                                    expr_references_cte_name(&assignment.expr, cte_name)
                                })
                            }
                            MergeAction::Insert { values, .. } => values
                                .iter()
                                .any(|value| expr_references_cte_name(value, cte_name)),
                            _ => false,
                        }
                })
        }
        _ => false,
    }
}

fn expr_references_cte_name(expr: &Expr, cte_name: &str) -> bool {
    match expr {
        Expr::Literal(_, _)
        | Expr::Identifier(_)
        | Expr::Parameter { .. }
        | Expr::Default { .. }
        | Expr::CypherExists { .. }
        | Expr::CypherPatternComprehension { .. } => false,
        Expr::FunctionCall { args, filter, .. } => {
            args.iter()
                .any(|arg| expr_references_cte_name(arg, cte_name))
                || filter
                    .as_ref()
                    .is_some_and(|f| expr_references_cte_name(f, cte_name))
        }
        Expr::UnaryOp { expr, .. } | Expr::IsNull { expr, .. } | Expr::Cast { expr, .. } => {
            expr_references_cte_name(expr, cte_name)
        }
        Expr::BinaryOp { left, right, .. }
        | Expr::IsDistinctFrom { left, right, .. }
        | Expr::Like {
            expr: left,
            pattern: right,
            ..
        } => expr_references_cte_name(left, cte_name) || expr_references_cte_name(right, cte_name),
        Expr::InList { expr, list, .. } => {
            expr_references_cte_name(expr, cte_name)
                || list
                    .iter()
                    .any(|item| expr_references_cte_name(item, cte_name))
        }
        Expr::Between {
            expr, low, high, ..
        } => {
            expr_references_cte_name(expr, cte_name)
                || expr_references_cte_name(low, cte_name)
                || expr_references_cte_name(high, cte_name)
        }
        Expr::CaseWhen {
            operand,
            conditions,
            results,
            else_result,
            ..
        } => {
            operand
                .as_ref()
                .is_some_and(|operand| expr_references_cte_name(operand, cte_name))
                || conditions
                    .iter()
                    .any(|condition| expr_references_cte_name(condition, cte_name))
                || results
                    .iter()
                    .any(|result| expr_references_cte_name(result, cte_name))
                || else_result
                    .as_ref()
                    .is_some_and(|result| expr_references_cte_name(result, cte_name))
        }
        Expr::Array { elements, .. } => elements
            .iter()
            .any(|element| expr_references_cte_name(element, cte_name)),
        Expr::ArraySubquery { query, .. } | Expr::Subquery { query, .. } => {
            select_references_cte_name(query, cte_name)
        }
        Expr::InSubquery { expr, query, .. } => {
            expr_references_cte_name(expr, cte_name) || select_references_cte_name(query, cte_name)
        }
        Expr::Exists { query, .. } => select_references_cte_name(query, cte_name),
        Expr::WindowFunction {
            function,
            partition_by,
            order_by,
            ..
        } => {
            expr_references_cte_name(function, cte_name)
                || partition_by
                    .iter()
                    .any(|expr| expr_references_cte_name(expr, cte_name))
                || order_by
                    .iter()
                    .any(|item| expr_references_cte_name(&item.expr, cte_name))
        }
    }
}

#[derive(Clone, Copy)]
struct RecursiveFromRef {
    position: usize,
    in_except: bool,
    in_outer_join: bool,
}

fn collect_recursive_from_refs_in_statement(
    statement: &Statement,
    cte_name: &str,
    in_except: bool,
    refs: &mut Vec<RecursiveFromRef>,
) {
    match statement {
        Statement::Select(select) => {
            collect_recursive_from_refs_in_select(select, cte_name, in_except, refs);
        }
        Statement::SetOperation(set_op) => {
            let next_in_except = in_except || matches!(set_op.op, SetOperationType::Except);
            collect_recursive_from_refs_in_statement(
                set_op.left.as_ref(),
                cte_name,
                next_in_except,
                refs,
            );
            collect_recursive_from_refs_in_statement(
                set_op.right.as_ref(),
                cte_name,
                next_in_except,
                refs,
            );
        }
        _ => {}
    }
}

fn collect_recursive_from_refs_in_select(
    select: &SelectStatement,
    cte_name: &str,
    in_except: bool,
    refs: &mut Vec<RecursiveFromRef>,
) {
    if select
        .ctes
        .iter()
        .any(|cte| cte.name.eq_ignore_ascii_case(cte_name))
    {
        return;
    }

    let left_side_outer = select.from.as_ref().is_some()
        && select.joins.iter().any(|join| {
            matches!(
                join.join_type,
                aiondb_parser::AstJoinType::Right | aiondb_parser::AstJoinType::Full
            )
        });

    if select
        .from
        .as_ref()
        .is_some_and(|from| object_name_matches_recursive_cte(from, cte_name))
    {
        if let Some(from) = &select.from {
            refs.push(RecursiveFromRef {
                position: from.span.start + 1,
                in_except,
                in_outer_join: left_side_outer,
            });
        }
    }
    for join in &select.joins {
        if object_name_matches_recursive_cte(&join.table, cte_name) {
            refs.push(RecursiveFromRef {
                position: join.table.span.start + 1,
                in_except,
                in_outer_join: matches!(
                    join.join_type,
                    aiondb_parser::AstJoinType::Left
                        | aiondb_parser::AstJoinType::Right
                        | aiondb_parser::AstJoinType::Full
                ),
            });
        }
    }
    for local_cte in &select.ctes {
        collect_recursive_from_refs_in_statement(
            local_cte.query.as_ref(),
            cte_name,
            in_except,
            refs,
        );
        if let Some(term) = &local_cte.recursive_term {
            collect_recursive_from_refs_in_select(term, cte_name, in_except, refs);
        }
    }
}

fn collect_recursive_from_refs_in_recursive_term(
    select: &SelectStatement,
    cte_name: &str,
    refs: &mut Vec<RecursiveFromRef>,
) {
    if let Some(wrapped_statement) = parser_wrapped_recursive_term_statement(select) {
        collect_recursive_from_refs_in_statement(wrapped_statement, cte_name, false, refs);
    } else {
        collect_recursive_from_refs_in_select(select, cte_name, false, refs);
    }
}

fn recursive_term_contains_set_operation(select: &SelectStatement) -> bool {
    let mut stack = vec![select];
    while let Some(select) = stack.pop() {
        for cte in &select.ctes {
            match cte.query.as_ref() {
                Statement::SetOperation(_) => return true,
                Statement::Select(select) => stack.push(select),
                _ => {}
            }
        }
    }
    false
}

fn parser_wrapped_recursive_term_statement(select: &SelectStatement) -> Option<&Statement> {
    let from = select.from.as_ref()?;
    if from.parts.len() != 1 || select.ctes.len() != 1 {
        return None;
    }
    let local_cte = &select.ctes[0];
    if !local_cte.name.eq_ignore_ascii_case(&from.parts[0]) {
        return None;
    }
    if local_cte.name != "__aiondb_recursive_term" {
        return None;
    }
    Some(local_cte.query.as_ref())
}

fn find_recursive_ref_in_recursive_term_subquery(
    select: &SelectStatement,
    cte_name: &str,
) -> Option<usize> {
    for item in &select.items {
        if let Some(pos) = find_recursive_ref_in_subquery_expr(&item.expr, cte_name) {
            return Some(pos);
        }
    }
    if let Some(expr) = &select.selection {
        if let Some(pos) = find_recursive_ref_in_subquery_expr(expr, cte_name) {
            return Some(pos);
        }
    }
    for expr in &select.group_by {
        if let Some(pos) = find_recursive_ref_in_subquery_expr(expr, cte_name) {
            return Some(pos);
        }
    }
    if let Some(expr) = &select.having {
        if let Some(pos) = find_recursive_ref_in_subquery_expr(expr, cte_name) {
            return Some(pos);
        }
    }
    for item in &select.order_by {
        if let Some(pos) = find_recursive_ref_in_subquery_expr(&item.expr, cte_name) {
            return Some(pos);
        }
    }
    for window in &select.window_definitions {
        for expr in &window.partition_by {
            if let Some(pos) = find_recursive_ref_in_subquery_expr(expr, cte_name) {
                return Some(pos);
            }
        }
        for item in &window.order_by {
            if let Some(pos) = find_recursive_ref_in_subquery_expr(&item.expr, cte_name) {
                return Some(pos);
            }
        }
    }
    if let Some(expr) = &select.limit {
        if let Some(pos) = find_recursive_ref_in_subquery_expr(expr, cte_name) {
            return Some(pos);
        }
    }
    if let Some(expr) = &select.offset {
        if let Some(pos) = find_recursive_ref_in_subquery_expr(expr, cte_name) {
            return Some(pos);
        }
    }
    for join in &select.joins {
        if let Some(expr) = &join.condition {
            if let Some(pos) = find_recursive_ref_in_subquery_expr(expr, cte_name) {
                return Some(pos);
            }
        }
    }
    None
}

fn find_recursive_ref_in_immediate_local_ctes_in_statement(
    statement: &Statement,
    cte_name: &str,
) -> Option<usize> {
    match statement {
        Statement::Select(select) => {
            find_recursive_ref_in_immediate_local_ctes_in_select(select, cte_name)
        }
        Statement::SetOperation(set_op) => {
            find_recursive_ref_in_immediate_local_ctes_in_statement(&set_op.left, cte_name).or_else(
                || find_recursive_ref_in_immediate_local_ctes_in_statement(&set_op.right, cte_name),
            )
        }
        _ => None,
    }
}

fn find_recursive_ref_in_immediate_local_ctes_in_select(
    select: &SelectStatement,
    cte_name: &str,
) -> Option<usize> {
    for local_cte in &select.ctes {
        if let Some(pos) = find_recursive_ref_in_statement(local_cte.query.as_ref(), cte_name) {
            return Some(pos);
        }
        if let Some(term) = &local_cte.recursive_term {
            if let Some(pos) = find_recursive_ref_in_select(term, cte_name) {
                return Some(pos);
            }
        }
    }
    None
}

fn find_recursive_ref_in_statement(statement: &Statement, cte_name: &str) -> Option<usize> {
    match statement {
        Statement::Select(select) => find_recursive_ref_in_select(select, cte_name),
        Statement::SetOperation(set_op) => find_recursive_ref_in_statement(&set_op.left, cte_name)
            .or_else(|| find_recursive_ref_in_statement(&set_op.right, cte_name)),
        _ => None,
    }
}

fn find_recursive_ref_in_subquery_expr(expr: &Expr, cte_name: &str) -> Option<usize> {
    match expr {
        Expr::ArraySubquery { query, .. }
        | Expr::Subquery { query, .. }
        | Expr::Exists { query, .. } => find_recursive_ref_in_select(query, cte_name),
        Expr::InSubquery { expr, query, .. } => find_recursive_ref_in_subquery_expr(expr, cte_name)
            .or_else(|| find_recursive_ref_in_select(query, cte_name)),
        Expr::UnaryOp { expr, .. } | Expr::IsNull { expr, .. } | Expr::Cast { expr, .. } => {
            find_recursive_ref_in_subquery_expr(expr, cte_name)
        }
        Expr::BinaryOp { left, right, .. } | Expr::IsDistinctFrom { left, right, .. } => {
            find_recursive_ref_in_subquery_expr(left, cte_name)
                .or_else(|| find_recursive_ref_in_subquery_expr(right, cte_name))
        }
        Expr::Like { expr, pattern, .. } => find_recursive_ref_in_subquery_expr(expr, cte_name)
            .or_else(|| find_recursive_ref_in_subquery_expr(pattern, cte_name)),
        Expr::InList { expr, list, .. } => find_recursive_ref_in_subquery_expr(expr, cte_name)
            .or_else(|| {
                list.iter()
                    .find_map(|item| find_recursive_ref_in_subquery_expr(item, cte_name))
            }),
        Expr::Between {
            expr, low, high, ..
        } => find_recursive_ref_in_subquery_expr(expr, cte_name)
            .or_else(|| find_recursive_ref_in_subquery_expr(low, cte_name))
            .or_else(|| find_recursive_ref_in_subquery_expr(high, cte_name)),
        Expr::CaseWhen {
            operand,
            conditions,
            results,
            else_result,
            ..
        } => operand
            .as_deref()
            .and_then(|expr| find_recursive_ref_in_subquery_expr(expr, cte_name))
            .or_else(|| {
                conditions
                    .iter()
                    .find_map(|expr| find_recursive_ref_in_subquery_expr(expr, cte_name))
            })
            .or_else(|| {
                results
                    .iter()
                    .find_map(|expr| find_recursive_ref_in_subquery_expr(expr, cte_name))
            })
            .or_else(|| {
                else_result
                    .as_deref()
                    .and_then(|expr| find_recursive_ref_in_subquery_expr(expr, cte_name))
            }),
        Expr::Array { elements, .. } => elements
            .iter()
            .find_map(|expr| find_recursive_ref_in_subquery_expr(expr, cte_name)),
        Expr::FunctionCall { args, filter, .. } => args
            .iter()
            .find_map(|expr| find_recursive_ref_in_subquery_expr(expr, cte_name))
            .or_else(|| {
                filter
                    .as_deref()
                    .and_then(|expr| find_recursive_ref_in_subquery_expr(expr, cte_name))
            }),
        Expr::WindowFunction {
            function,
            partition_by,
            order_by,
            ..
        } => find_recursive_ref_in_subquery_expr(function, cte_name)
            .or_else(|| {
                partition_by
                    .iter()
                    .find_map(|expr| find_recursive_ref_in_subquery_expr(expr, cte_name))
            })
            .or_else(|| {
                order_by
                    .iter()
                    .find_map(|item| find_recursive_ref_in_subquery_expr(&item.expr, cte_name))
            }),
        Expr::Literal(_, _)
        | Expr::Identifier(_)
        | Expr::Parameter { .. }
        | Expr::Default { .. }
        | Expr::CypherExists { .. }
        | Expr::CypherPatternComprehension { .. } => None,
    }
}

fn find_recursive_ref_in_select(select: &SelectStatement, cte_name: &str) -> Option<usize> {
    if select
        .ctes
        .iter()
        .any(|cte| cte.name.eq_ignore_ascii_case(cte_name))
    {
        return None;
    }

    if select
        .from
        .as_ref()
        .is_some_and(|from| object_name_matches_recursive_cte(from, cte_name))
    {
        return select.from.as_ref().map(|from| from.span.start + 1);
    }
    if let Some(join) = select
        .joins
        .iter()
        .find(|join| object_name_matches_recursive_cte(&join.table, cte_name))
    {
        return Some(join.table.span.start + 1);
    }
    find_recursive_ref_in_recursive_term_subquery(select, cte_name)
}

fn find_first_aggregate_in_select(select: &SelectStatement) -> Option<usize> {
    for item in &select.items {
        if let Some(pos) = find_first_aggregate_in_expr(&item.expr) {
            return Some(pos);
        }
    }
    if let Some(expr) = &select.selection {
        if let Some(pos) = find_first_aggregate_in_expr(expr) {
            return Some(pos);
        }
    }
    if let Some(expr) = &select.having {
        if let Some(pos) = find_first_aggregate_in_expr(expr) {
            return Some(pos);
        }
    }
    for expr in &select.group_by {
        if let Some(pos) = find_first_aggregate_in_expr(expr) {
            return Some(pos);
        }
    }
    for item in &select.order_by {
        if let Some(pos) = find_first_aggregate_in_expr(&item.expr) {
            return Some(pos);
        }
    }
    None
}

fn find_first_aggregate_in_expr(expr: &Expr) -> Option<usize> {
    match expr {
        Expr::FunctionCall {
            name,
            args,
            filter,
            span,
            ..
        } => {
            let function_name = name.parts.last().map_or("", String::as_str);
            if is_aggregate_function_name(function_name) {
                return Some(span.start + 1);
            }
            for arg in args {
                if let Some(pos) = find_first_aggregate_in_expr(arg) {
                    return Some(pos);
                }
            }
            filter.as_deref().and_then(find_first_aggregate_in_expr)
        }
        Expr::UnaryOp { expr, .. } | Expr::IsNull { expr, .. } | Expr::Cast { expr, .. } => {
            find_first_aggregate_in_expr(expr)
        }
        Expr::BinaryOp { left, right, .. } | Expr::IsDistinctFrom { left, right, .. } => {
            find_first_aggregate_in_expr(left).or_else(|| find_first_aggregate_in_expr(right))
        }
        Expr::Like { expr, pattern, .. } => {
            find_first_aggregate_in_expr(expr).or_else(|| find_first_aggregate_in_expr(pattern))
        }
        Expr::InList { expr, list, .. } => find_first_aggregate_in_expr(expr)
            .or_else(|| list.iter().find_map(find_first_aggregate_in_expr)),
        Expr::Between {
            expr, low, high, ..
        } => find_first_aggregate_in_expr(expr)
            .or_else(|| find_first_aggregate_in_expr(low))
            .or_else(|| find_first_aggregate_in_expr(high)),
        Expr::CaseWhen {
            operand,
            conditions,
            results,
            else_result,
            ..
        } => operand
            .as_deref()
            .and_then(find_first_aggregate_in_expr)
            .or_else(|| conditions.iter().find_map(find_first_aggregate_in_expr))
            .or_else(|| results.iter().find_map(find_first_aggregate_in_expr))
            .or_else(|| {
                else_result
                    .as_deref()
                    .and_then(find_first_aggregate_in_expr)
            }),
        Expr::Array { elements, .. } => elements.iter().find_map(find_first_aggregate_in_expr),
        Expr::WindowFunction {
            function,
            partition_by,
            order_by,
            ..
        } => find_first_aggregate_in_expr(function)
            .or_else(|| partition_by.iter().find_map(find_first_aggregate_in_expr))
            .or_else(|| {
                order_by
                    .iter()
                    .find_map(|item| find_first_aggregate_in_expr(&item.expr))
            }),
        Expr::ArraySubquery { .. }
        | Expr::Subquery { .. }
        | Expr::InSubquery { .. }
        | Expr::Exists { .. }
        | Expr::CypherExists { .. }
        | Expr::CypherPatternComprehension { .. }
        | Expr::Literal(_, _)
        | Expr::Identifier(_)
        | Expr::Parameter { .. }
        | Expr::Default { .. } => None,
    }
}

fn is_aggregate_function_name(name: &str) -> bool {
    matches!(
        name.to_ascii_lowercase().as_str(),
        "count"
            | "sum"
            | "avg"
            | "min"
            | "max"
            | "bool_and"
            | "bool_or"
            | "every"
            | "string_agg"
            | "array_agg"
            | "json_agg"
            | "jsonb_agg"
            | "json_object_agg"
            | "jsonb_object_agg"
    )
}

/// Run the fixpoint iteration for a single recursive CTE.
///
/// Returns (all accumulated rows, column names derived from the base query).
fn evaluate_recursive_cte(
    engine: &Engine,
    session: &SessionHandle,
    cte: &CteDefinition,
    outer_select: &SelectStatement,
) -> DbResult<(Vec<Row>, Vec<String>, Vec<DataType>)> {
    let Some(recursive_term) = cte.recursive_term.as_ref() else {
        return Err(DbError::internal(
            "recursive CTE evaluation requires a recursive term",
        ));
    };

    if let Some(pos) =
        find_recursive_ref_in_immediate_local_ctes_in_statement(&cte.query, &cte.name)
    {
        return Err(DbError::bind_error(
            aiondb_core::SqlState::SyntaxError,
            format!(
                "recursive reference to query \"{}\" must not appear within a subquery",
                cte.name
            ),
        )
        .with_position(pos));
    }

    let mut recursive_refs = Vec::new();
    collect_recursive_from_refs_in_recursive_term(recursive_term, &cte.name, &mut recursive_refs);
    if let Some(r#ref) = recursive_refs.iter().find(|r| r.in_outer_join).copied() {
        return Err(DbError::bind_error(
            aiondb_core::SqlState::SyntaxError,
            format!(
                "recursive reference to query \"{}\" must not appear within an outer join",
                cte.name
            ),
        )
        .with_position(r#ref.position));
    }
    if recursive_refs.len() > 1 {
        if let Some(r#ref) = recursive_refs.iter().find(|r| r.in_except).copied() {
            return Err(DbError::bind_error(
                aiondb_core::SqlState::FeatureNotSupported,
                format!(
                    "recursive reference to query \"{}\" must not appear within EXCEPT",
                    cte.name
                ),
            )
            .with_position(r#ref.position));
        }
        if !recursive_term_contains_set_operation(recursive_term) {
            return Err(DbError::bind_error(
                aiondb_core::SqlState::FeatureNotSupported,
                format!(
                    "recursive reference to query \"{}\" must not appear more than once",
                    cte.name
                ),
            )
            .with_position(recursive_refs[1].position));
        }
    }

    if let Some(pos) = find_recursive_ref_in_recursive_term_subquery(recursive_term, &cte.name) {
        return Err(DbError::bind_error(
            aiondb_core::SqlState::SyntaxError,
            format!(
                "recursive reference to query \"{}\" must not appear within a subquery",
                cte.name
            ),
        )
        .with_position(pos));
    }
    if let Some(pos) = find_first_aggregate_in_select(recursive_term) {
        return Err(DbError::bind_error(
            aiondb_core::SqlState::SyntaxError,
            "aggregate functions are not allowed in a recursive query's recursive term",
        )
        .with_position(pos));
    }
    if let Some(order_by) = recursive_term.order_by.first() {
        return Err(DbError::bind_error(
            aiondb_core::SqlState::FeatureNotSupported,
            "ORDER BY in a recursive query is not implemented",
        )
        .with_position(order_by.expr.span().start + 1));
    }
    if let Some(offset) = &recursive_term.offset {
        return Err(DbError::bind_error(
            aiondb_core::SqlState::FeatureNotSupported,
            "OFFSET in a recursive query is not implemented",
        )
        .with_position(offset.span().start + 1));
    }

    // Use the live session limits so recursive CTE execution cannot exceed
    // the runtime budget configured for this session.
    let session_limits = engine.session_info(session)?.limits;
    let statement_deadline = if session_limits.statement_timeout.is_zero() {
        None
    } else {
        Instant::now().checked_add(session_limits.statement_timeout)
    };
    check_recursive_execution_interrupts(engine, session, statement_deadline)?;

    // ---------------------------------------------------------------
    // 1. Execute the base (non-recursive) query to seed the worktable
    // ---------------------------------------------------------------
    let base_stmt = build_base_statement(cte, outer_select);
    let base_result = execute_query_rows(engine, session, &base_stmt)?;
    check_recursive_execution_interrupts(engine, session, statement_deadline)?;

    let base_col_names: Vec<String> = base_result
        .columns
        .iter()
        .map(|column| column.name.clone())
        .collect();

    let n_cols = base_result.columns.len();
    ensure_recursive_row_width(&base_result.rows, n_cols)?;

    // The column names exposed by the CTE (either explicit aliases or derived
    // from the base query).
    let cte_col_names: Vec<String> = if let Some(aliases) = &cte.column_aliases {
        aliases
            .iter()
            .enumerate()
            .map(|(i, a)| {
                if a.is_empty() {
                    base_col_names
                        .get(i)
                        .cloned()
                        .unwrap_or_else(|| format!("col{i}"))
                } else {
                    a.clone()
                }
            })
            .collect()
    } else {
        base_col_names
    };

    let max_iterations = session_limits.max_recursive_iterations;
    let hard_max_rows = session_limits.max_recursive_rows;
    let max_result_bytes = session_limits.max_result_bytes;
    let max_memory_bytes = session_limits.max_memory_bytes;
    let max_synthetic_values_rows = recursive_synthetic_values_row_cap(max_memory_bytes);
    let materialization_row_goal =
        recursive_materialization_row_goal(outer_select, &cte.name, hard_max_rows);
    let target_max_rows = materialization_row_goal.unwrap_or(hard_max_rows);

    let mut target_types: Vec<DataType> = base_result
        .columns
        .iter()
        .map(|column| column.data_type.clone())
        .collect();
    let base_unknown_columns = recursive_unknown_columns(cte.query.as_ref());
    let recursive_unknown_columns =
        recursive_unknown_columns(&Statement::Select(recursive_term.as_ref().clone()));

    let iter_stmt = build_recursive_iteration_statement(
        recursive_term,
        cte,
        &cte_col_names,
        &target_types,
        &base_result.rows,
        outer_select,
        max_synthetic_values_rows,
    )?;

    let first_iter_result = execute_query_rows(engine, session, &iter_stmt)?;
    check_recursive_execution_interrupts(engine, session, statement_deadline)?;
    target_types = resolve_recursive_target_types(
        &base_result.columns,
        &first_iter_result.columns,
        base_unknown_columns.as_deref(),
        recursive_unknown_columns.as_deref(),
        &base_result.rows,
        &first_iter_result.rows,
    )?;
    ensure_recursive_non_recursive_term_matches_overall_type(
        cte,
        &base_result.columns,
        &target_types,
        base_unknown_columns.as_deref(),
        recursive_declared_type_hints(cte.query.as_ref()).as_deref(),
    )?;
    ensure_recursive_row_width(&first_iter_result.rows, n_cols)?;

    let mut cached_first_iteration = Some(coerce_recursive_rows(
        first_iter_result.rows,
        &target_types,
    )?);
    let mut working_table = coerce_recursive_rows(base_result.rows, &target_types)?;

    if working_table.is_empty() {
        return Ok((Vec::new(), cte_col_names, target_types));
    }

    let union_all = cte.union_all;
    let mut all_rows: Vec<Row> = Vec::new();
    let mut result_bytes_used = 0u64;
    let mut all_rows_memory_bytes = 0u64;
    let mut working_table_memory_bytes = rows_memory_bytes(&working_table)?;
    let mut seen_memory_bytes = 0u64;

    check_recursive_memory_budget(
        all_rows_memory_bytes,
        working_table_memory_bytes,
        seen_memory_bytes,
        max_memory_bytes,
    )?;

    // For UNION (distinct) mode, track seen rows via their debug representation.
    // This is a simple but correct approach: rows are compared by their
    // serialised form which is deterministic for equal values.
    let mut seen = if union_all {
        None
    } else {
        let mut set = std::collections::HashSet::<String>::with_capacity(working_table.len());
        let mut unique = Vec::with_capacity(working_table.len());
        let mut unique_memory_bytes = 0u64;
        for row in working_table.drain(..) {
            check_recursive_execution_interrupts(engine, session, statement_deadline)?;
            let key = row_key(&row);
            // Compute memory cost *before* the move into the set so we
            // don't need to clone the key string.
            let key_mem = hash_key_memory_bytes(&key);
            if set.insert(key) {
                let row_memory_bytes = recursive_row_memory_bytes(&row);
                unique_memory_bytes = checked_add_u64(
                    unique_memory_bytes,
                    row_memory_bytes,
                    "recursive query memory accounting overflowed",
                )?;
                seen_memory_bytes = checked_add_u64(
                    seen_memory_bytes,
                    key_mem,
                    "recursive query memory accounting overflowed",
                )?;
                unique.push(row);
                check_recursive_memory_budget(
                    all_rows_memory_bytes,
                    unique_memory_bytes,
                    seen_memory_bytes,
                    max_memory_bytes,
                )?;
            }
        }
        working_table = unique;
        working_table_memory_bytes = unique_memory_bytes;
        Some(set)
    };

    if working_table.len() > hard_max_rows {
        return Err(DbError::program_limit(
            "recursive query exceeded maximum number of rows",
        ));
    }
    if working_table.len() > target_max_rows {
        working_table.truncate(target_max_rows);
        working_table_memory_bytes = rows_memory_bytes(&working_table)?;
    }
    append_rows_to_recursive_result(
        engine,
        session,
        statement_deadline,
        &working_table,
        &mut all_rows,
        &mut result_bytes_used,
        &mut all_rows_memory_bytes,
        working_table_memory_bytes,
        seen_memory_bytes,
        max_result_bytes,
        max_memory_bytes,
    )?;
    if all_rows.len() >= target_max_rows {
        return Ok((all_rows, cte_col_names, target_types));
    }

    // ---------------------------------------------------------------
    // 2. Iterate: execute recursive term against current working table
    // ---------------------------------------------------------------
    let mut terminated_by_iteration_cap = true;
    for iteration in 0..max_iterations {
        check_recursive_execution_interrupts(engine, session, statement_deadline)?;
        if working_table.is_empty() {
            terminated_by_iteration_cap = false;
            break;
        }
        if all_rows.len() >= target_max_rows {
            terminated_by_iteration_cap = false;
            break;
        }

        let new_rows = if iteration == 0 {
            cached_first_iteration
                .take()
                .ok_or_else(|| DbError::internal("first recursive iteration should be cached"))?
        } else {
            // Build a statement that replaces references to the CTE name in the
            // recursive term's FROM clause with the current working-table rows
            // inlined as a VALUES subquery.
            let iter_stmt = build_recursive_iteration_statement(
                recursive_term,
                cte,
                &cte_col_names,
                &target_types,
                &working_table,
                outer_select,
                max_synthetic_values_rows,
            )?;

            let iter_result = execute_query_rows(engine, session, &iter_stmt)?;
            check_recursive_execution_interrupts(engine, session, statement_deadline)?;
            ensure_recursive_iteration_types(&target_types, &iter_result.columns)?;
            ensure_recursive_row_width(&iter_result.rows, n_cols)?;
            coerce_recursive_rows(iter_result.rows, &target_types)?
        };

        if new_rows.is_empty() {
            terminated_by_iteration_cap = false;
            break;
        }

        let previous_working_table_memory_bytes = working_table_memory_bytes;

        // For UNION DISTINCT, filter out duplicates.
        let (mut new_rows, mut new_rows_memory_bytes, next_seen_memory_bytes) =
            if let Some(ref mut seen_set) = seen {
                let mut unique = Vec::with_capacity(new_rows.len());
                let mut unique_memory_bytes = 0u64;
                let mut next_seen_memory_bytes = seen_memory_bytes;
                for row in new_rows {
                    check_recursive_execution_interrupts(engine, session, statement_deadline)?;
                    let key = row_key(&row);
                    let key_mem = hash_key_memory_bytes(&key);
                    if seen_set.insert(key) {
                        next_seen_memory_bytes = checked_add_u64(
                            next_seen_memory_bytes,
                            key_mem,
                            "recursive query memory accounting overflowed",
                        )?;
                        let row_memory_bytes = recursive_row_memory_bytes(&row);
                        unique_memory_bytes = checked_add_u64(
                            unique_memory_bytes,
                            row_memory_bytes,
                            "recursive query memory accounting overflowed",
                        )?;
                        check_recursive_memory_budget_with_pending(
                            all_rows_memory_bytes,
                            previous_working_table_memory_bytes,
                            unique_memory_bytes,
                            next_seen_memory_bytes,
                            max_memory_bytes,
                        )?;
                        unique.push(row);
                    }
                }
                (unique, unique_memory_bytes, next_seen_memory_bytes)
            } else {
                let new_rows_memory_bytes = rows_memory_bytes(&new_rows)?;
                (new_rows, new_rows_memory_bytes, seen_memory_bytes)
            };

        if new_rows.is_empty() {
            // Converged after UNION DISTINCT duplicate elimination.
            terminated_by_iteration_cap = false;
            break;
        }

        let Some(total_rows_after_append) = all_rows.len().checked_add(new_rows.len()) else {
            return Err(DbError::program_limit(
                "recursive query exceeded maximum number of rows",
            ));
        };

        if total_rows_after_append > hard_max_rows {
            return Err(DbError::program_limit(
                "recursive query exceeded maximum number of rows",
            ));
        }
        if total_rows_after_append > target_max_rows {
            let remaining_rows = target_max_rows.saturating_sub(all_rows.len());
            new_rows.truncate(remaining_rows);
            new_rows_memory_bytes = rows_memory_bytes(&new_rows)?;
            if new_rows.is_empty() {
                terminated_by_iteration_cap = false;
                break;
            }
        }

        check_recursive_memory_budget_with_pending(
            all_rows_memory_bytes,
            previous_working_table_memory_bytes,
            new_rows_memory_bytes,
            next_seen_memory_bytes,
            max_memory_bytes,
        )?;

        seen_memory_bytes = next_seen_memory_bytes;
        working_table_memory_bytes = new_rows_memory_bytes;
        working_table = new_rows;

        append_rows_to_recursive_result(
            engine,
            session,
            statement_deadline,
            &working_table,
            &mut all_rows,
            &mut result_bytes_used,
            &mut all_rows_memory_bytes,
            working_table_memory_bytes,
            seen_memory_bytes,
            max_result_bytes,
            max_memory_bytes,
        )?;
        if all_rows.len() >= target_max_rows {
            terminated_by_iteration_cap = false;
            break;
        }
    }

    if terminated_by_iteration_cap && !working_table.is_empty() && all_rows.len() < target_max_rows
    {
        return Err(DbError::program_limit(
            "recursive query exceeded maximum number of iterations",
        ));
    }

    Ok((all_rows, cte_col_names, target_types))
}

struct QueryRows {
    rows: Vec<Row>,
    columns: Vec<ResultColumn>,
}

fn ensure_recursive_row_width(rows: &[Row], expected_cols: usize) -> DbResult<()> {
    for row in rows {
        if row.values.len() != expected_cols {
            return Err(DbError::bind_error(
                aiondb_core::SqlState::SyntaxError,
                format!(
                    "each UNION query must have the same number of columns (expected {expected_cols}, got {})",
                    row.values.len()
                ),
            ));
        }
    }
    Ok(())
}

fn resolve_recursive_target_types(
    base_columns: &[ResultColumn],
    recursive_columns: &[ResultColumn],
    base_unknown_columns: Option<&[bool]>,
    recursive_unknown_columns: Option<&[bool]>,
    base_rows: &[Row],
    recursive_rows: &[Row],
) -> DbResult<Vec<DataType>> {
    ensure_recursive_column_count(base_columns.len(), recursive_columns.len())?;
    base_columns
        .iter()
        .enumerate()
        .zip(recursive_columns.iter())
        .map(|((index, base), recursive)| {
            let base_unknown = base_unknown_columns
                .and_then(|flags| flags.get(index))
                .copied()
                .unwrap_or(false);
            let recursive_unknown = recursive_unknown_columns
                .and_then(|flags| flags.get(index))
                .copied()
                .unwrap_or(false);
            resolve_recursive_column_type(
                &base.data_type,
                &recursive.data_type,
                base_unknown,
                recursive_unknown,
                base_rows,
                recursive_rows,
                index,
            )
        })
        .collect()
}

fn ensure_recursive_iteration_types(
    target_types: &[DataType],
    actual_columns: &[ResultColumn],
) -> DbResult<()> {
    ensure_recursive_column_count(target_types.len(), actual_columns.len())?;
    for (target_type, actual_column) in target_types.iter().zip(actual_columns.iter()) {
        let resolved = resolve_recursive_column_type(
            target_type,
            &actual_column.data_type,
            false,
            false,
            &[],
            &[],
            0,
        )?;
        if &resolved != target_type {
            return Err(DbError::bind_error(
                aiondb_core::SqlState::SyntaxError,
                format!(
                    "recursive query column type changed from {} to {}",
                    target_type.pg_type_name(),
                    actual_column.data_type.pg_type_name()
                ),
            ));
        }
    }
    Ok(())
}

fn ensure_recursive_column_count(expected: usize, actual: usize) -> DbResult<()> {
    if expected == actual {
        Ok(())
    } else {
        Err(DbError::bind_error(
            aiondb_core::SqlState::SyntaxError,
            format!(
                "each UNION query must have the same number of columns (expected {expected}, got {actual})",
            ),
        ))
    }
}

fn ensure_recursive_non_recursive_term_matches_overall_type(
    cte: &CteDefinition,
    base_columns: &[ResultColumn],
    overall_types: &[DataType],
    base_unknown_columns: Option<&[bool]>,
    declared_type_hints: Option<&[Option<String>]>,
) -> DbResult<()> {
    ensure_recursive_column_count(base_columns.len(), overall_types.len())?;
    for (index, (base_column, overall_type)) in base_columns.iter().zip(overall_types).enumerate() {
        if base_unknown_columns
            .and_then(|flags| flags.get(index))
            .copied()
            .unwrap_or(false)
        {
            continue;
        }

        let declared_type_hint = declared_type_hints
            .and_then(|hints| hints.get(index))
            .and_then(|hint| hint.as_ref())
            .map(|hint| hint.to_ascii_lowercase());
        let base_has_numeric_typmod = declared_type_hint
            .as_deref()
            .is_some_and(|hint| hint.starts_with("numeric(") || hint.starts_with("decimal("));
        let base_type_name =
            declared_type_hint.unwrap_or_else(|| base_column.data_type.pg_type_name().to_owned());
        let overall_type_name = overall_type.pg_type_name().to_owned();

        if (!recursive_base_type_can_coerce_to_overall(&base_column.data_type, overall_type)
            && base_column.data_type != *overall_type)
            || (base_has_numeric_typmod && matches!(overall_type, DataType::Numeric))
        {
            return Err(DbError::bind_error(
                aiondb_core::SqlState::DatatypeMismatch,
                format!(
                    "recursive query \"{}\" column {} has type {} in non-recursive term but type {} overall",
                    cte.name,
                    index.saturating_add(1),
                    base_type_name,
                    overall_type_name,
                ),
            )
            .with_position(cte.query.span().start + 1)
            .with_client_hint("Cast the output of the non-recursive term to the correct type."));
        }
    }

    Ok(())
}

fn recursive_base_type_can_coerce_to_overall(
    base_type: &DataType,
    overall_type: &DataType,
) -> bool {
    matches!((base_type, overall_type), (DataType::Int, DataType::BigInt))
}

fn coerce_recursive_rows(rows: Vec<Row>, target_types: &[DataType]) -> DbResult<Vec<Row>> {
    rows.into_iter()
        .map(|row| coerce_recursive_row(row, target_types))
        .collect()
}

fn coerce_recursive_row(row: Row, target_types: &[DataType]) -> DbResult<Row> {
    ensure_recursive_row_width(std::slice::from_ref(&row), target_types.len())?;
    let values = row
        .values
        .into_iter()
        .zip(target_types.iter())
        .map(|(value, target_type)| aiondb_eval::coercions::coerce_value(value, target_type))
        .collect::<DbResult<Vec<_>>>()?;
    Ok(Row { values })
}

fn resolve_recursive_column_type(
    left: &DataType,
    right: &DataType,
    left_unknown: bool,
    right_unknown: bool,
    left_rows: &[Row],
    right_rows: &[Row],
    column_index: usize,
) -> DbResult<DataType> {
    if left == right {
        return Ok(left.clone());
    }

    if left_unknown && !right_unknown {
        return Ok(right.clone());
    }
    if right_unknown && !left_unknown {
        return Ok(left.clone());
    }

    if matches!(right, DataType::Text)
        && !matches!(left, DataType::Text)
        && column_rows_coercible_to_type(right_rows, column_index, left)
    {
        return Ok(left.clone());
    }
    if matches!(left, DataType::Text)
        && !matches!(right, DataType::Text)
        && column_rows_coercible_to_type(left_rows, column_index, right)
    {
        return Ok(right.clone());
    }

    if is_recursive_numeric(left) && is_recursive_numeric(right) {
        return resolve_recursive_arithmetic_type(left, right);
    }

    match (left, right) {
        (DataType::Text, DataType::Text) => Ok(DataType::Text),
        (DataType::Date, DataType::Timestamp) | (DataType::Timestamp, DataType::Date) => {
            Ok(DataType::Timestamp)
        }
        (DataType::Date | DataType::Timestamp, DataType::TimestampTz)
        | (DataType::TimestampTz, DataType::Date | DataType::Timestamp)
        | (DataType::Time, DataType::TimeTz)
        | (DataType::TimeTz, DataType::Time) => Ok(DataType::TimestampTz),
        (DataType::Array(left_elem), DataType::Array(right_elem)) => Ok(DataType::Array(Box::new(
            resolve_recursive_column_type(left_elem, right_elem, false, false, &[], &[], 0)?,
        ))),
        (
            DataType::Vector {
                dims: left_dims, ..
            },
            DataType::Vector {
                dims: right_dims, ..
            },
        ) if left_dims == right_dims => Ok(left.clone()),
        (DataType::Jsonb, DataType::Jsonb) => Ok(DataType::Jsonb),
        _ => Err(DbError::bind_error(
            aiondb_core::SqlState::SyntaxError,
            format!(
                "set operation types {} and {} cannot be matched",
                left.pg_type_name(),
                right.pg_type_name()
            ),
        )),
    }
}

fn column_rows_coercible_to_type(
    rows: &[Row],
    column_index: usize,
    target_type: &DataType,
) -> bool {
    !rows.is_empty()
        && rows.iter().all(|row| {
            row.values.get(column_index).cloned().is_some_and(|value| {
                aiondb_eval::coercions::coerce_value(value, target_type).is_ok()
            })
        })
}

fn recursive_unknown_columns(statement: &Statement) -> Option<Vec<bool>> {
    match statement {
        Statement::Select(select) => Some(
            select
                .items
                .iter()
                .map(|item| expr_is_recursive_unknown_literal(&item.expr))
                .collect(),
        ),
        Statement::SetOperation(set_op) if set_op.op == SetOperationType::Union => {
            let left = recursive_unknown_columns(set_op.left.as_ref())?;
            let right = recursive_unknown_columns(set_op.right.as_ref())?;
            if left.len() != right.len() {
                return None;
            }
            Some(
                left.into_iter()
                    .zip(right)
                    .map(|(lhs, rhs)| lhs && rhs)
                    .collect(),
            )
        }
        _ => None,
    }
}

fn recursive_declared_type_hints(statement: &Statement) -> Option<Vec<Option<String>>> {
    match statement {
        Statement::Select(select) => Some(
            select
                .items
                .iter()
                .map(|item| recursive_declared_type_hint_for_expr(&item.expr))
                .collect(),
        ),
        Statement::SetOperation(set_op) if set_op.op == SetOperationType::Union => {
            let left = recursive_declared_type_hints(set_op.left.as_ref())?;
            let right = recursive_declared_type_hints(set_op.right.as_ref())?;
            if left.len() != right.len() {
                return None;
            }
            Some(
                left.into_iter()
                    .zip(right)
                    .map(|(lhs, rhs)| lhs.or(rhs))
                    .collect(),
            )
        }
        _ => None,
    }
}

fn recursive_declared_type_hint_for_expr(expr: &Expr) -> Option<String> {
    match expr {
        Expr::FunctionCall { name, args, .. }
            if name
                .parts
                .last()
                .is_some_and(|part| part.eq_ignore_ascii_case("__aiondb_type_hint")) =>
        {
            if let Some(Expr::Literal(Literal::String(type_name), _)) = args.get(1) {
                Some(type_name.to_ascii_lowercase())
            } else {
                args.first().and_then(recursive_declared_type_hint_for_expr)
            }
        }
        Expr::Cast { data_type, .. } => Some(data_type.pg_type_name().to_owned()),
        Expr::Literal(Literal::Integer(_), _) => Some("integer".to_owned()),
        Expr::Literal(Literal::NumericLit(_), _) => Some("numeric".to_owned()),
        Expr::Literal(Literal::String(_), _) => Some("text".to_owned()),
        Expr::Literal(Literal::Boolean(_), _) => Some("boolean".to_owned()),
        _ => None,
    }
}

fn expr_is_recursive_unknown_literal(expr: &Expr) -> bool {
    matches!(expr, Expr::Literal(Literal::Null | Literal::String(_), _))
}

fn is_recursive_numeric(data_type: &DataType) -> bool {
    matches!(
        data_type,
        DataType::Int
            | DataType::BigInt
            | DataType::Real
            | DataType::Double
            | DataType::Numeric
            | DataType::Money
    )
}

fn resolve_recursive_arithmetic_type(left: &DataType, right: &DataType) -> DbResult<DataType> {
    match (left, right) {
        (DataType::Int, DataType::Int) => Ok(DataType::Int),
        (DataType::Int, DataType::BigInt) | (DataType::BigInt, DataType::Int) => {
            Ok(DataType::BigInt)
        }
        (DataType::BigInt, DataType::BigInt) => Ok(DataType::BigInt),
        (DataType::Real, DataType::Real) => Ok(DataType::Real),
        (DataType::Double, DataType::Double) => Ok(DataType::Double),
        (DataType::Int, DataType::Real) | (DataType::Real, DataType::Int) => Ok(DataType::Real),
        (DataType::Int, DataType::Double) | (DataType::Double, DataType::Int) => {
            Ok(DataType::Double)
        }
        (DataType::BigInt, DataType::Double) | (DataType::Double, DataType::BigInt) => {
            Ok(DataType::Double)
        }
        (DataType::BigInt, DataType::Real) | (DataType::Real, DataType::BigInt) => {
            Ok(DataType::Double)
        }
        (DataType::Real, DataType::Double) | (DataType::Double, DataType::Real) => {
            Ok(DataType::Double)
        }
        (DataType::Numeric, DataType::Numeric) => Ok(DataType::Numeric),
        (DataType::Int | DataType::BigInt, DataType::Numeric)
        | (DataType::Numeric, DataType::Int | DataType::BigInt) => Ok(DataType::Numeric),
        (DataType::Real | DataType::Double, DataType::Numeric)
        | (DataType::Numeric, DataType::Real | DataType::Double) => Ok(DataType::Double),
        (DataType::Money, DataType::Money) => Ok(DataType::Money),
        (
            DataType::Money,
            DataType::Int | DataType::BigInt | DataType::Real | DataType::Double | DataType::Text,
        )
        | (
            DataType::Int | DataType::BigInt | DataType::Real | DataType::Double | DataType::Text,
            DataType::Money,
        ) => Ok(DataType::Money),
        (DataType::PgLsn, DataType::PgLsn) => Ok(DataType::Numeric),
        (DataType::PgLsn, DataType::Int | DataType::BigInt | DataType::Numeric)
        | (DataType::Int | DataType::BigInt | DataType::Numeric, DataType::PgLsn) => {
            Ok(DataType::PgLsn)
        }
        _ => Err(DbError::bind_error(
            aiondb_core::SqlState::SyntaxError,
            format!(
                "set operation types {} and {} cannot be matched",
                left.pg_type_name(),
                right.pg_type_name()
            ),
        )),
    }
}

/// Execute a statement and return (rows, `column_names`).
fn execute_query_rows(
    engine: &Engine,
    session: &SessionHandle,
    stmt: &Statement,
) -> DbResult<QueryRows> {
    let result = engine.execute_planned_statement(session, stmt)?;
    match result {
        StatementResult::Query { columns, rows } => Ok(QueryRows { rows, columns }),
        _ => Err(DbError::internal(
            "recursive CTE subquery did not return rows",
        )),
    }
}

fn recursive_materialization_row_goal(
    outer_select: &SelectStatement,
    cte_name: &str,
    hard_max_rows: usize,
) -> Option<usize> {
    // Safe limit pushdown: only for direct passthrough reads from the recursive
    // CTE (`SELECT <identifiers> FROM <cte> LIMIT <n>` with no joins/filters/order).
    // This prevents unbounded materialization for recursive UNION ALL queries where
    // the outer query only needs the first N rows.
    if outer_select.distinct != DistinctKind::All
        || !outer_select.joins.is_empty()
        || outer_select.selection.is_some()
        || !outer_select.group_by.is_empty()
        || outer_select.having.is_some()
        || !outer_select.window_definitions.is_empty()
        || !outer_select.order_by.is_empty()
    {
        return None;
    }
    if !recursive_projection_is_passthrough(outer_select) {
        return None;
    }
    let from = outer_select.from.as_ref()?;
    if !object_name_matches_recursive_cte(from, cte_name) {
        return None;
    }
    if !recursive_offset_is_zero(outer_select.offset.as_ref()) {
        return None;
    }
    let limit = recursive_integer_literal(outer_select.limit.as_ref()?)?;
    Some(limit.min(hard_max_rows))
}

fn recursive_projection_is_passthrough(select: &SelectStatement) -> bool {
    !select.items.is_empty()
        && select
            .items
            .iter()
            .all(|item| matches!(item.expr, Expr::Identifier(_)))
}

fn object_name_matches_recursive_cte(name: &ObjectName, cte_name: &str) -> bool {
    name.parts.len() == 1 && name.parts[0].eq_ignore_ascii_case(cte_name)
}

fn recursive_offset_is_zero(offset: Option<&Expr>) -> bool {
    offset
        .and_then(recursive_integer_literal)
        .map_or(true, |value| value == 0)
}

fn recursive_integer_literal(expr: &Expr) -> Option<usize> {
    match expr {
        Expr::Literal(Literal::Integer(value), _) => usize::try_from(*value).ok(),
        Expr::Literal(Literal::NumericLit(value), _) => value.parse::<usize>().ok(),
        _ => None,
    }
}
include!("recursive_cte_materialization.rs");

/// Produce a deterministic key for a row suitable for deduplication.
/// Uses the debug representation of each value which is deterministic for
/// equal runtime values.
fn row_key(row: &Row) -> String {
    use std::fmt::Write;
    let mut key = String::new();
    for (i, v) in row.values.iter().enumerate() {
        if i > 0 {
            key.push('\x00');
        }
        let _ = write!(key, "{v:?}");
    }
    key
}

fn recursive_synthetic_values_row_cap(max_memory_bytes: u64) -> usize {
    let by_budget = max_memory_bytes
        .checked_div(RECURSIVE_SYNTHETIC_VALUES_ESTIMATED_ROW_BYTES)
        .unwrap_or(0)
        .clamp(1, RECURSIVE_SYNTHETIC_VALUES_MAX_ROWS);
    usize::try_from(by_budget).unwrap_or(usize::MAX)
}

fn ensure_synthetic_union_depth(row_count: usize) -> DbResult<()> {
    if row_count <= 1 {
        return Ok(());
    }
    let mut depth = 0usize;
    let mut nodes = row_count;
    while nodes > 1 {
        nodes = nodes.div_ceil(2);
        depth = depth.saturating_add(1);
    }
    if depth > RECURSIVE_SYNTHETIC_UNION_MAX_DEPTH {
        return Err(DbError::program_limit(format!(
            "recursive query synthetic UNION ALL depth exceeded (depth={depth}, max={RECURSIVE_SYNTHETIC_UNION_MAX_DEPTH})"
        )));
    }
    Ok(())
}

fn append_rows_to_recursive_result(
    engine: &Engine,
    session: &SessionHandle,
    statement_deadline: Option<Instant>,
    source_rows: &[Row],
    all_rows: &mut Vec<Row>,
    result_bytes_used: &mut u64,
    all_rows_memory_bytes: &mut u64,
    working_table_memory_bytes: u64,
    seen_memory_bytes: u64,
    max_result_bytes: u64,
    max_memory_bytes: u64,
) -> DbResult<()> {
    for row in source_rows {
        check_recursive_execution_interrupts(engine, session, statement_deadline)?;
        let next_result_bytes = checked_add_u64(
            *result_bytes_used,
            estimate_row_bytes(row),
            "recursive query result byte accounting overflowed",
        )?;
        if next_result_bytes > max_result_bytes {
            return Err(DbError::program_limit(
                "maximum number of result bytes reached",
            ));
        }

        let next_all_rows_memory_bytes = checked_add_u64(
            *all_rows_memory_bytes,
            recursive_row_memory_bytes(row),
            "recursive query memory accounting overflowed",
        )?;
        check_recursive_memory_budget(
            next_all_rows_memory_bytes,
            working_table_memory_bytes,
            seen_memory_bytes,
            max_memory_bytes,
        )?;

        all_rows.push(row.clone());
        *result_bytes_used = next_result_bytes;
        *all_rows_memory_bytes = next_all_rows_memory_bytes;
    }
    Ok(())
}

fn check_recursive_execution_interrupts(
    engine: &Engine,
    session: &SessionHandle,
    statement_deadline: Option<Instant>,
) -> DbResult<()> {
    if let Some(deadline) = statement_deadline {
        if Instant::now() >= deadline {
            return Err(DbError::query_canceled("statement timeout exceeded"));
        }
    }
    engine.take_cancellation_if_needed(session)
}

fn check_recursive_memory_budget(
    all_rows_memory_bytes: u64,
    working_table_memory_bytes: u64,
    seen_memory_bytes: u64,
    max_memory_bytes: u64,
) -> DbResult<()> {
    check_recursive_memory_budget_with_pending(
        all_rows_memory_bytes,
        working_table_memory_bytes,
        0,
        seen_memory_bytes,
        max_memory_bytes,
    )
}

fn check_recursive_memory_budget_with_pending(
    all_rows_memory_bytes: u64,
    working_table_memory_bytes: u64,
    pending_memory_bytes: u64,
    seen_memory_bytes: u64,
    max_memory_bytes: u64,
) -> DbResult<()> {
    let total_memory_bytes = checked_add_u64(
        checked_add_u64(
            checked_add_u64(
                all_rows_memory_bytes,
                working_table_memory_bytes,
                "recursive query memory accounting overflowed",
            )?,
            pending_memory_bytes,
            "recursive query memory accounting overflowed",
        )?,
        seen_memory_bytes,
        "recursive query memory accounting overflowed",
    )?;
    if total_memory_bytes > max_memory_bytes {
        return Err(DbError::program_limit(
            "maximum memory budget exceeded for this statement",
        ));
    }
    Ok(())
}

fn rows_memory_bytes(rows: &[Row]) -> DbResult<u64> {
    let mut total = 0u64;
    for row in rows {
        total = checked_add_u64(
            total,
            recursive_row_memory_bytes(row),
            "recursive query memory accounting overflowed",
        )?;
    }
    Ok(total)
}

#[inline]
fn recursive_row_memory_bytes(row: &Row) -> u64 {
    estimate_row_bytes(row).saturating_add(RECURSIVE_ROW_OVERHEAD_BYTES)
}

#[inline]
fn hash_key_memory_bytes(key: &str) -> u64 {
    usize_to_u64(key.len()).saturating_add(RECURSIVE_SEEN_KEY_OVERHEAD_BYTES)
}

#[inline]
fn checked_add_u64(lhs: u64, rhs: u64, overflow_message: &'static str) -> DbResult<u64> {
    lhs.checked_add(rhs)
        .ok_or_else(|| DbError::program_limit(overflow_message))
}

#[inline]
fn estimate_row_bytes(row: &Row) -> u64 {
    estimate_values_bytes(&row.values)
}

#[inline]
fn estimate_values_bytes(values: &[Value]) -> u64 {
    let mut total = 0u64;
    for value in values {
        total = total.saturating_add(estimate_value_bytes(value));
    }
    total
}

#[inline]
fn estimate_value_bytes(value: &Value) -> u64 {
    estimate_value_bytes_at_depth(value, 0)
}

fn estimate_value_bytes_at_depth(value: &Value, depth: usize) -> u64 {
    if depth >= RECURSIVE_VALUE_ESTIMATE_MAX_DEPTH {
        return RECURSIVE_DEEP_VALUE_ESTIMATED_BYTES;
    }
    match value {
        Value::Null => 1,
        Value::Int(_) => 4,
        Value::BigInt(_) => 8,
        Value::Real(_) => 4,
        Value::Double(_) => 8,
        Value::Numeric(_) => 20,
        Value::Money(_) => 8,
        Value::Text(text) => usize_to_u64(text.len()),
        Value::Boolean(_) => 1,
        Value::Blob(bytes) => usize_to_u64(bytes.len()),
        Value::Timestamp(_) => 16,
        Value::Date(_) => 8,
        Value::LargeDate(_) => 12,
        Value::Time(_) => 8,
        Value::TimeTz(_, _) => 12,
        Value::Interval(_) => 16,
        Value::Tid(_) => 8,
        Value::MacAddr(_) => 6,
        Value::MacAddr8(_) => 8,
        Value::PgLsn(_) => 8,
        Value::Uuid(_) => 16,
        Value::TimestampTz(_) => 16,
        Value::Jsonb(v) => estimate_jsonb_bytes_at_depth(v, 0),
        Value::Vector(vector) => {
            4u64.saturating_add(usize_to_u64(vector.values.len()).saturating_mul(4))
        }
        Value::Array(elements) => {
            let mut total = 8u64;
            for element in elements {
                total = total.saturating_add(estimate_value_bytes_at_depth(element, depth + 1));
            }
            total
        }
    }
}

fn estimate_jsonb_bytes_at_depth(value: &serde_json::Value, depth: usize) -> u64 {
    if depth >= RECURSIVE_VALUE_ESTIMATE_MAX_DEPTH {
        return RECURSIVE_DEEP_VALUE_ESTIMATED_BYTES;
    }
    match value {
        serde_json::Value::Null => 4,
        serde_json::Value::Bool(b) => {
            if *b {
                4
            } else {
                5
            }
        }
        serde_json::Value::Number(number) => {
            if let Some(value) = number.as_u64() {
                return decimal_digits_u64(value);
            }
            if let Some(value) = number.as_i64() {
                if value < 0 {
                    return 1u64.saturating_add(decimal_digits_u64(value.unsigned_abs()));
                }
                return decimal_digits_u64(value.cast_unsigned());
            }
            usize_to_u64(number.to_string().len())
        }
        serde_json::Value::String(text) => 2u64.saturating_add(usize_to_u64(text.len())),
        serde_json::Value::Array(items) => {
            let mut total = 2u64;
            let mut iter = items.iter();
            if let Some(first) = iter.next() {
                total = total.saturating_add(estimate_jsonb_bytes_at_depth(first, depth + 1));
                for item in iter {
                    total = total.saturating_add(2);
                    total = total.saturating_add(estimate_jsonb_bytes_at_depth(item, depth + 1));
                }
            }
            total
        }
        serde_json::Value::Object(map) => {
            let mut total = 2u64;
            let mut iter = map.iter();
            if let Some((key, value)) = iter.next() {
                total = total.saturating_add(2u64.saturating_add(usize_to_u64(key.len())));
                total = total.saturating_add(1);
                total = total.saturating_add(estimate_jsonb_bytes_at_depth(value, depth + 1));
                for (key, value) in iter {
                    total = total.saturating_add(2);
                    total = total.saturating_add(2u64.saturating_add(usize_to_u64(key.len())));
                    total = total.saturating_add(1);
                    total = total.saturating_add(estimate_jsonb_bytes_at_depth(value, depth + 1));
                }
            }
            total
        }
    }
}

#[inline]
fn decimal_digits_u64(mut value: u64) -> u64 {
    let mut digits = 1u64;
    while value >= 10 {
        value /= 10;
        digits = digits.saturating_add(1);
    }
    digits
}
