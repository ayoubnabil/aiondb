#![allow(clippy::pedantic)]

use std::cell::Cell;
use std::thread::LocalKey;

use aiondb_core::{DataType, DbError, DbResult, SqlState};
use aiondb_parser::ast::WindowDefinition;
use aiondb_parser::{
    Expr, InsertStatement, JoinClause, Literal, MergeAction, MergeSource, MergeStatement,
    MergeWhenClause, OnConflict, OnConflictAction, OrderByItem, SelectItem, SelectStatement,
    SetOperationStatement, Statement, UpdateAssignment,
};

use aiondb_core::Value;

#[path = "params_detect.rs"]
mod detect;

pub(crate) use self::detect::statement_contains_parameters;

const MAX_PARAM_VALUE_EXPR_DEPTH: usize = 256;
const MAX_PARAM_BIND_STATEMENT_DEPTH: usize = 512;
const MAX_PARAM_BIND_SELECT_DEPTH: usize = 512;
const MAX_PARAM_BIND_EXPR_DEPTH: usize = 1024;
const MAX_PARAM_BIND_CYPHER_DEPTH: usize = 512;

thread_local! {
    static PARAM_BIND_STATEMENT_DEPTH: Cell<usize> = const { Cell::new(0) };
    static PARAM_BIND_SELECT_DEPTH: Cell<usize> = const { Cell::new(0) };
    static PARAM_BIND_EXPR_DEPTH: Cell<usize> = const { Cell::new(0) };
    static PARAM_BIND_CYPHER_DEPTH: Cell<usize> = const { Cell::new(0) };
}

struct ParamBindDepthGuard {
    key: &'static LocalKey<Cell<usize>>,
}

impl Drop for ParamBindDepthGuard {
    fn drop(&mut self) {
        self.key
            .with(|depth| depth.set(depth.get().saturating_sub(1)));
    }
}

fn enter_param_bind_depth(
    key: &'static LocalKey<Cell<usize>>,
    limit: usize,
    label: &str,
) -> DbResult<ParamBindDepthGuard> {
    key.with(|depth| {
        let current = depth.get();
        if current >= limit {
            return Err(DbError::program_limit(format!(
                "parameter binding {label} depth exceeds limit {limit}"
            )));
        }
        depth.set(current + 1);
        Ok(ParamBindDepthGuard { key })
    })
}

pub(crate) fn ensure_supported_portal_params(params: &[Value]) -> DbResult<()> {
    // PostgreSQL caps parameter count at i16 (65535). Mirror that here so an
    // FFI/SDK caller bypassing pgwire's `MAX_STATEMENT_PARAMS = 10_000` cannot
    // submit an arbitrarily-long parameter vector (audit query_api F-3).
    const MAX_PORTAL_PARAMS: usize = 65535;
    if params.len() > MAX_PORTAL_PARAMS {
        return Err(DbError::program_limit(format!(
            "portal parameter count {} exceeds limit {MAX_PORTAL_PARAMS}",
            params.len()
        )));
    }
    for value in params {
        value_to_expr(value, aiondb_parser::Span::default())?;
    }
    Ok(())
}

pub(crate) fn ensure_portal_param_types_compatible(
    expected_types: &[DataType],
    params: &[Value],
) -> DbResult<()> {
    for (index, (expected, value)) in expected_types.iter().zip(params.iter()).enumerate() {
        if matches!(value, Value::Null) {
            continue;
        }

        if !value_matches_expected_type(value, expected) {
            let received_type = value
                .data_type()
                .map_or_else(|| "NULL".to_owned(), |data_type| data_type.to_string());
            return Err(DbError::bind_error(
                SqlState::DatatypeMismatch,
                format!(
                    "parameter ${} expects {expected}, received {received_type}",
                    index + 1
                ),
            ));
        }
    }

    Ok(())
}

pub(crate) fn bind_statement_params(
    statement: &Statement,
    params: &[Value],
    expected_types: &[DataType],
) -> DbResult<Statement> {
    let _guard = enter_param_bind_depth(
        &PARAM_BIND_STATEMENT_DEPTH,
        MAX_PARAM_BIND_STATEMENT_DEPTH,
        "statement",
    )?;
    if params.is_empty() {
        return Ok(statement.clone());
    }
    if let Some(bound) = try_bind_simple_select_single_eq_param(statement, params, expected_types)?
    {
        return Ok(bound);
    }
    match statement {
        Statement::Begin {
            mode,
            read_only,
            deferrable,
            span,
        } => Ok(Statement::Begin {
            mode: *mode,
            read_only: *read_only,
            deferrable: *deferrable,
            span: *span,
        }),
        Statement::Commit { span } => Ok(Statement::Commit { span: *span }),
        Statement::Rollback { span } => Ok(Statement::Rollback { span: *span }),
        Statement::Checkpoint { span } => Ok(Statement::Checkpoint { span: *span }),
        Statement::PrepareTransaction { gid, span } => Ok(Statement::PrepareTransaction {
            gid: gid.clone(),
            span: *span,
        }),
        Statement::PrepareStmt { span } => Ok(Statement::PrepareStmt { span: *span }),
        Statement::ExecuteStmt { span } => Ok(Statement::ExecuteStmt { span: *span }),
        Statement::DeallocateStmt { span } => Ok(Statement::DeallocateStmt { span: *span }),
        Statement::DeclareStmt { span } => Ok(Statement::DeclareStmt { span: *span }),
        Statement::FetchStmt { span } => Ok(Statement::FetchStmt { span: *span }),
        Statement::MoveStmt { span } => Ok(Statement::MoveStmt { span: *span }),
        Statement::CloseStmt { span } => Ok(Statement::CloseStmt { span: *span }),
        Statement::CommitPrepared { gid, span } => Ok(Statement::CommitPrepared {
            gid: gid.clone(),
            span: *span,
        }),
        Statement::RollbackPrepared { gid, span } => Ok(Statement::RollbackPrepared {
            gid: gid.clone(),
            span: *span,
        }),
        Statement::Savepoint { name, span } => Ok(Statement::Savepoint {
            name: name.clone(),
            span: *span,
        }),
        Statement::RollbackToSavepoint { name, span } => Ok(Statement::RollbackToSavepoint {
            name: name.clone(),
            span: *span,
        }),
        Statement::ReleaseSavepoint { name, span } => Ok(Statement::ReleaseSavepoint {
            name: name.clone(),
            span: *span,
        }),
        Statement::CreateTable(create_table) => Ok(Statement::CreateTable(create_table.clone())),
        Statement::CreateSequence(create_sequence) => {
            Ok(Statement::CreateSequence(create_sequence.clone()))
        }
        Statement::CreateIndex(create_index) => Ok(Statement::CreateIndex(create_index.clone())),
        Statement::TruncateTable(truncate_table) => {
            Ok(Statement::TruncateTable(truncate_table.clone()))
        }
        Statement::DropTable(drop_table) => Ok(Statement::DropTable(drop_table.clone())),
        Statement::DropIndex(drop_index) => Ok(Statement::DropIndex(drop_index.clone())),
        Statement::DropSequence(drop_sequence) => {
            Ok(Statement::DropSequence(drop_sequence.clone()))
        }
        Statement::AlterTable(alter_table) => Ok(Statement::AlterTable(alter_table.clone())),
        Statement::Copy(copy) => Ok(Statement::Copy(aiondb_parser::CopyStatement {
            table: copy.table.clone(),
            columns: copy.columns.clone(),
            query: copy
                .query
                .as_ref()
                .map(|query| bind_statement_params(query, params, expected_types))
                .transpose()?
                .map(Box::new),
            direction: copy.direction,
            span: copy.span,
        })),
        Statement::CreateView(create_view) => Ok(Statement::CreateView(create_view.clone())),
        Statement::DropView(drop_view) => Ok(Statement::DropView(drop_view.clone())),
        Statement::CreateNodeLabel(s) => Ok(Statement::CreateNodeLabel(s.clone())),
        Statement::CreateEdgeLabel(s) => Ok(Statement::CreateEdgeLabel(s.clone())),
        Statement::DropNodeLabel(s) => Ok(Statement::DropNodeLabel(s.clone())),
        Statement::DropEdgeLabel(s) => Ok(Statement::DropEdgeLabel(s.clone())),
        Statement::CreateRole(s) => Ok(Statement::CreateRole(s.clone())),
        Statement::DropRole(s) => Ok(Statement::DropRole(s.clone())),
        Statement::AlterRole(s) => Ok(Statement::AlterRole(s.clone())),
        Statement::AlterRoleRename(s) => Ok(Statement::AlterRoleRename(s.clone())),
        Statement::Grant(s) => Ok(Statement::Grant(s.clone())),
        Statement::Revoke(s) => Ok(Statement::Revoke(s.clone())),
        Statement::CreateSchema(s) => Ok(Statement::CreateSchema(
            aiondb_parser::CreateSchemaStatement {
                name: s.name.clone(),
                if_not_exists: s.if_not_exists,
                body: s
                    .body
                    .iter()
                    .map(|statement| bind_statement_params(statement, params, expected_types))
                    .collect::<DbResult<Vec<_>>>()?,
                span: s.span,
            },
        )),
        Statement::DropSchema(s) => Ok(Statement::DropSchema(s.clone())),
        Statement::Analyze { table, span } => Ok(Statement::Analyze {
            table: table.clone(),
            span: *span,
        }),
        Statement::Vacuum { table, span } => Ok(Statement::Vacuum {
            table: table.clone(),
            span: *span,
        }),
        Statement::Backup { path, span } => Ok(Statement::Backup {
            path: path.clone(),
            span: *span,
        }),
        Statement::Restore { path, span } => Ok(Statement::Restore {
            path: path.clone(),
            span: *span,
        }),
        Statement::Load { file, span } => Ok(Statement::Load {
            file: file.clone(),
            span: *span,
        }),
        Statement::AlterSystem(s) => Ok(Statement::AlterSystem(s.clone())),
        Statement::SetTransaction(s) => Ok(Statement::SetTransaction(s.clone())),
        Statement::SetSessionCharacteristics(s) => {
            Ok(Statement::SetSessionCharacteristics(s.clone()))
        }
        Statement::SetConstraints(s) => Ok(Statement::SetConstraints(s.clone())),
        Statement::CreateFunction(s) => Ok(Statement::CreateFunction(s.clone())),
        Statement::DropFunction(s) => Ok(Statement::DropFunction(s.clone())),
        Statement::CreateTrigger(s) => Ok(Statement::CreateTrigger(s.clone())),
        Statement::DropTrigger(s) => Ok(Statement::DropTrigger(s.clone())),
        Statement::CreateTableAs(s) => Ok(Statement::CreateTableAs(s.clone())),
        Statement::CreateTenant { name, span } => Ok(Statement::CreateTenant {
            name: name.clone(),
            span: *span,
        }),
        Statement::DropTenant { name, span } => Ok(Statement::DropTenant {
            name: name.clone(),
            span: *span,
        }),
        Statement::SetTenant { name, span } => Ok(Statement::SetTenant {
            name: name.clone(),
            span: *span,
        }),
        Statement::SetVariable(s) => Ok(Statement::SetVariable(s.clone())),
        Statement::ShowVariable(s) => Ok(Statement::ShowVariable(s.clone())),
        Statement::ResetVariable(s) => Ok(Statement::ResetVariable(s.clone())),
        Statement::Merge(merge) => Ok(Statement::Merge(bind_merge_params(
            merge,
            params,
            expected_types,
        )?)),
        Statement::AlterTriggerRename(s) => Ok(Statement::AlterTriggerRename(s.clone())),
        Statement::CreateExtension(s) => Ok(Statement::CreateExtension(s.clone())),
        Statement::DropExtension(s) => Ok(Statement::DropExtension(s.clone())),
        Statement::SecurityLabel(s) => Ok(Statement::SecurityLabel(s.clone())),
        Statement::Comment(s) => Ok(Statement::Comment(s.clone())),
        Statement::Cypher(s) => Ok(Statement::Cypher(bind_cypher_statement_params(
            s,
            params,
            expected_types,
        )?)),
        Statement::Listen { channel, span } => Ok(Statement::Listen {
            channel: channel.clone(),
            span: *span,
        }),
        Statement::Unlisten { channel, span } => Ok(Statement::Unlisten {
            channel: channel.clone(),
            span: *span,
        }),
        Statement::Notify {
            channel,
            payload,
            span,
        } => Ok(Statement::Notify {
            channel: channel.clone(),
            payload: payload.clone(),
            span: *span,
        }),
        Statement::Discard(s) => Ok(Statement::Discard(s.clone())),
        Statement::CreateDatabase(s) => Ok(Statement::CreateDatabase(s.clone())),
        Statement::AlterDatabase(s) => Ok(Statement::AlterDatabase(s.clone())),
        Statement::DropDatabase(s) => Ok(Statement::DropDatabase(s.clone())),
        Statement::CreateType(s) => Ok(Statement::CreateType(s.clone())),
        Statement::AlterType(s) => Ok(Statement::AlterType(s.clone())),
        Statement::DropType(s) => Ok(Statement::DropType(s.clone())),
        Statement::CreateDomain(s) => Ok(Statement::CreateDomain(s.clone())),
        Statement::AlterDomain(s) => Ok(Statement::AlterDomain(s.clone())),
        Statement::DropDomain(s) => Ok(Statement::DropDomain(s.clone())),
        Statement::CreateCast(s) => Ok(Statement::CreateCast(s.clone())),
        Statement::DropCast(s) => Ok(Statement::DropCast(s.clone())),
        Statement::CreateRule(s) => Ok(Statement::CreateRule(s.clone())),
        Statement::AlterRule(s) => Ok(Statement::AlterRule(s.clone())),
        Statement::DropRule(s) => Ok(Statement::DropRule(s.clone())),
        Statement::CreateOrReplaceCompat(s) => Ok(Statement::CreateOrReplaceCompat(s.clone())),
        Statement::CreatePolicy(s) => Ok(Statement::CreatePolicy(s.clone())),
        Statement::AlterPolicy(s) => Ok(Statement::AlterPolicy(s.clone())),
        Statement::DropPolicy(s) => Ok(Statement::DropPolicy(s.clone())),
        Statement::CreatePublication(s) => Ok(Statement::CreatePublication(s.clone())),
        Statement::AlterPublication(s) => Ok(Statement::AlterPublication(s.clone())),
        Statement::DropPublication(s) => Ok(Statement::DropPublication(s.clone())),
        Statement::CreateSubscription(s) => Ok(Statement::CreateSubscription(s.clone())),
        Statement::AlterSubscription(s) => Ok(Statement::AlterSubscription(s.clone())),
        Statement::DropSubscription(s) => Ok(Statement::DropSubscription(s.clone())),
        Statement::CreateServer(s) => Ok(Statement::CreateServer(s.clone())),
        Statement::AlterServer(s) => Ok(Statement::AlterServer(s.clone())),
        Statement::DropServer(s) => Ok(Statement::DropServer(s.clone())),
        Statement::CreateUserMapping(s) => Ok(Statement::CreateUserMapping(s.clone())),
        Statement::AlterUserMapping(s) => Ok(Statement::AlterUserMapping(s.clone())),
        Statement::DropUserMapping(s) => Ok(Statement::DropUserMapping(s.clone())),
        Statement::CreateForeignTable(s) => Ok(Statement::CreateForeignTable(s.clone())),
        Statement::AlterForeignTable(s) => Ok(Statement::AlterForeignTable(s.clone())),
        Statement::DropForeignTable(s) => Ok(Statement::DropForeignTable(s.clone())),
        Statement::CreateForeignDataWrapper(s) => {
            Ok(Statement::CreateForeignDataWrapper(s.clone()))
        }
        Statement::AlterForeignDataWrapper(s) => Ok(Statement::AlterForeignDataWrapper(s.clone())),
        Statement::DropForeignDataWrapper(s) => Ok(Statement::DropForeignDataWrapper(s.clone())),
        Statement::CreateCollation(s) => Ok(Statement::CreateCollation(s.clone())),
        Statement::AlterCollation(s) => Ok(Statement::AlterCollation(s.clone())),
        Statement::DropCollation(s) => Ok(Statement::DropCollation(s.clone())),
        Statement::CreateStatistics(s) => Ok(Statement::CreateStatistics(s.clone())),
        Statement::CreateTablespace(s) => Ok(Statement::CreateTablespace(s.clone())),
        Statement::DropStatistics(s) => Ok(Statement::DropStatistics(s.clone())),
        Statement::AlterStatistics(s) => Ok(Statement::AlterStatistics(s.clone())),
        Statement::DropTablespace(s) => Ok(Statement::DropTablespace(s.clone())),
        Statement::AlterTablespace(s) => Ok(Statement::AlterTablespace(s.clone())),
        Statement::CreateAggregate(s) => Ok(Statement::CreateAggregate(s.clone())),
        Statement::DropAggregate(s) => Ok(Statement::DropAggregate(s.clone())),
        Statement::CreateProcedure(s) => Ok(Statement::CreateProcedure(s.clone())),
        Statement::DropProcedure(s) => Ok(Statement::DropProcedure(s.clone())),
        Statement::DropRoutine(s) => Ok(Statement::DropRoutine(s.clone())),
        Statement::AlterTriggerCompat(s) => Ok(Statement::AlterTriggerCompat(s.clone())),
        Statement::CreateOperator(s) => Ok(Statement::CreateOperator(s.clone())),
        Statement::DropOperator(s) => Ok(Statement::DropOperator(s.clone())),
        statement if statement.compat_tag().is_some() => {
            let tag = statement.compat_tag().unwrap_or("UNKNOWN");
            Err(DbError::feature_not_supported(format!(
                "parameter binding is not supported for compatibility-tagged statements ({tag})"
            )))
        }
        Statement::DoStmt { span } => Ok(Statement::DoStmt { span: *span }),
        Statement::DropOwned(s) => Ok(Statement::DropOwned(s.clone())),
        Statement::ReassignOwned(s) => Ok(Statement::ReassignOwned(s.clone())),
        Statement::Lock(lock) => Ok(Statement::Lock(lock.clone())),
        Statement::Explain {
            analyze,
            format_json,
            statement: inner,
            span,
        } => Ok(Statement::Explain {
            analyze: *analyze,
            format_json: *format_json,
            statement: Box::new(bind_statement_params(inner, params, expected_types)?),
            span: *span,
        }),
        Statement::SetOperation(set_op) => Ok(Statement::SetOperation(bind_set_operation_params(
            set_op,
            params,
            expected_types,
        )?)),
        Statement::Delete(delete) => Ok(Statement::Delete(aiondb_parser::DeleteStatement {
            table: delete.table.clone(),
            table_alias: delete.table_alias.clone(),
            using_tables: delete.using_tables.clone(),
            selection: delete
                .selection
                .as_ref()
                .map(|expr| bind_expr_params(expr, params, expected_types))
                .transpose()?,
            where_span: delete.where_span,
            returning: bind_select_items_params(&delete.returning, params, expected_types)?,
            span: delete.span,
        })),
        Statement::Insert(insert) => Ok(Statement::Insert(InsertStatement {
            table: insert.table.clone(),
            table_alias: insert.table_alias.clone(),
            columns: insert.columns.clone(),
            rows: insert
                .rows
                .iter()
                .map(|row| {
                    row.iter()
                        .map(|expr| bind_expr_params(expr, params, expected_types))
                        .collect::<DbResult<Vec<_>>>()
                })
                .collect::<DbResult<Vec<_>>>()?,
            query: insert
                .query
                .as_ref()
                .map(|query| bind_select_params(query, params, expected_types))
                .transpose()?,
            on_conflict: bind_on_conflict_params(
                insert.on_conflict.as_ref(),
                params,
                expected_types,
            )?,
            returning: bind_select_items_params(&insert.returning, params, expected_types)?,
            span: insert.span,
        })),
        Statement::Select(select) => Ok(Statement::Select(bind_select_params(
            select,
            params,
            expected_types,
        )?)),
        Statement::Update(update) => Ok(Statement::Update(aiondb_parser::UpdateStatement {
            table: update.table.clone(),
            table_alias: update.table_alias.clone(),
            assignments: update
                .assignments
                .iter()
                .map(|assignment| {
                    Ok(UpdateAssignment {
                        column: assignment.column.clone(),
                        expr: bind_expr_params(&assignment.expr, params, expected_types)?,
                        span: assignment.span,
                    })
                })
                .collect::<DbResult<Vec<_>>>()?,
            from_tables: update.from_tables.clone(),
            selection: update
                .selection
                .as_ref()
                .map(|expr| bind_expr_params(expr, params, expected_types))
                .transpose()?,
            where_span: update.where_span,
            returning: bind_select_items_params(&update.returning, params, expected_types)?,
            ctes: update.ctes.clone(),
            span: update.span,
        })),
        _ => Ok(statement.clone()),
    }
}

fn strip_parser_cast_wrappers(expr: &Expr) -> &Expr {
    let mut current = expr;
    while let Expr::Cast { expr, .. } = current {
        current = expr;
    }
    current
}

fn push_parser_expr_children<'a>(expr: &'a Expr, stack: &mut Vec<&'a Expr>) {
    match expr {
        Expr::FunctionCall { args, filter, .. } => {
            if let Some(expr) = filter {
                stack.push(expr);
            }
            stack.extend(args);
        }
        Expr::UnaryOp { expr, .. } | Expr::IsNull { expr, .. } | Expr::Cast { expr, .. } => {
            stack.push(expr);
        }
        Expr::BinaryOp { left, right, .. }
        | Expr::IsDistinctFrom { left, right, .. }
        | Expr::Like {
            expr: left,
            pattern: right,
            ..
        } => {
            stack.push(right);
            stack.push(left);
        }
        Expr::InList { expr, list, .. } => {
            stack.extend(list);
            stack.push(expr);
        }
        Expr::Between {
            expr, low, high, ..
        } => {
            stack.push(high);
            stack.push(low);
            stack.push(expr);
        }
        Expr::CaseWhen {
            operand,
            conditions,
            results,
            else_result,
            ..
        } => {
            if let Some(expr) = else_result {
                stack.push(expr);
            }
            stack.extend(results);
            stack.extend(conditions);
            if let Some(expr) = operand {
                stack.push(expr);
            }
        }
        Expr::Array { elements, .. } => stack.extend(elements),
        Expr::InSubquery { expr, .. } => stack.push(expr),
        Expr::WindowFunction {
            function,
            partition_by,
            order_by,
            ..
        } => {
            for item in order_by {
                stack.push(&item.expr);
            }
            stack.extend(partition_by);
            stack.push(function);
        }
        Expr::Parameter { .. }
        | Expr::Literal(_, _)
        | Expr::Identifier(_)
        | Expr::Default { .. }
        | Expr::Subquery { .. }
        | Expr::ArraySubquery { .. }
        | Expr::Exists { .. }
        | Expr::CypherExists { .. } => {}
        Expr::CypherPatternComprehension {
            where_clause,
            map_expr,
            ..
        } => {
            stack.push(map_expr);
            if let Some(where_clause) = where_clause {
                stack.push(where_clause);
            }
        }
    }
}

enum ParserParameterWork<'a> {
    Expr(&'a Expr),
    Select(&'a SelectStatement),
}

fn parser_expr_contains_parameter(expr: &Expr) -> bool {
    contains_parser_parameter_work([ParserParameterWork::Expr(expr)])
}

fn contains_parser_parameter_work<'a>(
    initial: impl IntoIterator<Item = ParserParameterWork<'a>>,
) -> bool {
    let mut stack: Vec<ParserParameterWork<'a>> = initial.into_iter().collect();
    while let Some(work) = stack.pop() {
        match work {
            ParserParameterWork::Expr(expr) => match expr {
                Expr::Parameter { .. } => return true,
                Expr::Subquery { query, .. }
                | Expr::ArraySubquery { query, .. }
                | Expr::Exists { query, .. } => stack.push(ParserParameterWork::Select(query)),
                Expr::InSubquery { expr, query, .. } => {
                    stack.push(ParserParameterWork::Select(query));
                    stack.push(ParserParameterWork::Expr(expr));
                }
                _ => push_parser_expr_work(expr, &mut stack),
            },
            ParserParameterWork::Select(statement) => {
                for item in &statement.items {
                    stack.push(ParserParameterWork::Expr(&item.expr));
                }
                if let Some(selection) = &statement.selection {
                    stack.push(ParserParameterWork::Expr(selection));
                }
                stack.extend(statement.group_by.iter().map(ParserParameterWork::Expr));
                if let Some(having) = &statement.having {
                    stack.push(ParserParameterWork::Expr(having));
                }
                for item in &statement.order_by {
                    stack.push(ParserParameterWork::Expr(&item.expr));
                }
                if let Some(limit) = &statement.limit {
                    stack.push(ParserParameterWork::Expr(limit));
                }
                if let Some(offset) = &statement.offset {
                    stack.push(ParserParameterWork::Expr(offset));
                }
            }
        }
    }
    false
}

fn push_parser_expr_work<'a>(expr: &'a Expr, stack: &mut Vec<ParserParameterWork<'a>>) {
    let mut expr_stack = Vec::new();
    push_parser_expr_children(expr, &mut expr_stack);
    stack.extend(expr_stack.into_iter().map(ParserParameterWork::Expr));
}

/// Fast path that rebuilds the AST only along the `WHERE col = $1` spine
/// instead of cloning every field via `bind_statement_params`. The
/// recognized shape is the textbook OLTP point lookup: a flat
/// `SELECT ... WHERE col = $1` with no CTEs, joins, grouping, ordering, or
/// projection-side parameters. Returning `Ok(None)` falls back to the full
/// generic path, so the conditions below are guards, not optimizations:
/// any divergence from the supported shape *must* bail out so the generic
/// binder still observes every parameter site.
fn try_bind_simple_select_single_eq_param(
    statement: &Statement,
    params: &[Value],
    expected_types: &[DataType],
) -> DbResult<Option<Statement>> {
    if params.len() != 1 || expected_types.len() != 1 {
        return Ok(None);
    }
    let Statement::Select(select) = statement else {
        return Ok(None);
    };

    if !select.ctes.is_empty()
        || !matches!(select.distinct, aiondb_parser::DistinctKind::All)
        || select.from.is_none()
        || !select.joins.is_empty()
        || !select.group_by.is_empty()
        || !select.group_by_items.is_empty()
        || select.having.is_some()
        || !select.window_definitions.is_empty()
        || !select.order_by.is_empty()
        || select.limit.is_some()
        || select.offset.is_some()
        || select
            .items
            .iter()
            .any(|item| parser_expr_contains_parameter(&item.expr))
    {
        return Ok(None);
    }

    let Some(selection) = select.selection.as_ref() else {
        return Ok(None);
    };
    let Expr::BinaryOp {
        left,
        op,
        right,
        span,
    } = selection
    else {
        return Ok(None);
    };
    if *op != aiondb_parser::BinaryOperator::Eq {
        return Ok(None);
    }

    let left_base = strip_parser_cast_wrappers(left);
    let right_base = strip_parser_cast_wrappers(right);
    let (param_on_left, param_span) = match (left_base, right_base) {
        (Expr::Parameter { index, span }, Expr::Identifier(_)) if *index == 1 => (true, *span),
        (Expr::Identifier(_), Expr::Parameter { index, span }) if *index == 1 => (false, *span),
        _ => return Ok(None),
    };

    let literal_expr = value_to_expr_with_expected(&params[0], expected_types.first(), param_span)?;
    let rebound_selection = if param_on_left {
        Expr::BinaryOp {
            left: Box::new(literal_expr),
            op: *op,
            right: right.clone(),
            span: *span,
        }
    } else {
        Expr::BinaryOp {
            left: left.clone(),
            op: *op,
            right: Box::new(literal_expr),
            span: *span,
        }
    };

    let mut rebound = select.clone();
    rebound.selection = Some(rebound_selection);
    Ok(Some(Statement::Select(rebound)))
}

fn bind_select_items_params(
    items: &[SelectItem],
    params: &[Value],
    expected_types: &[DataType],
) -> DbResult<Vec<SelectItem>> {
    items
        .iter()
        .map(|item| {
            Ok(SelectItem {
                expr: bind_expr_params(&item.expr, params, expected_types)?,
                alias: item.alias.clone(),
                span: item.span,
            })
        })
        .collect()
}

fn bind_on_conflict_params(
    on_conflict: Option<&OnConflict>,
    params: &[Value],
    expected_types: &[DataType],
) -> DbResult<Option<OnConflict>> {
    on_conflict
        .map(|on_conflict| {
            Ok(OnConflict {
                columns: on_conflict.columns.clone(),
                action: match &on_conflict.action {
                    OnConflictAction::DoNothing => OnConflictAction::DoNothing,
                    OnConflictAction::DoUpdate {
                        assignments,
                        where_clause,
                    } => OnConflictAction::DoUpdate {
                        assignments: assignments
                            .iter()
                            .map(|assignment| {
                                Ok(UpdateAssignment {
                                    column: assignment.column.clone(),
                                    expr: bind_expr_params(
                                        &assignment.expr,
                                        params,
                                        expected_types,
                                    )?,
                                    span: assignment.span,
                                })
                            })
                            .collect::<DbResult<Vec<_>>>()?,
                        where_clause: where_clause
                            .as_ref()
                            .map(|expr| bind_expr_params(expr, params, expected_types))
                            .transpose()?,
                    },
                },
            })
        })
        .transpose()
}

fn bind_merge_params(
    merge: &MergeStatement,
    params: &[Value],
    expected_types: &[DataType],
) -> DbResult<MergeStatement> {
    Ok(MergeStatement {
        target_table: merge.target_table.clone(),
        target_alias: merge.target_alias.clone(),
        source: match &merge.source {
            MergeSource::Table(table) => MergeSource::Table(table.clone()),
            MergeSource::Subquery(query) => {
                MergeSource::Subquery(Box::new(bind_select_params(query, params, expected_types)?))
            }
        },
        source_alias: merge.source_alias.clone(),
        on_condition: bind_expr_params(&merge.on_condition, params, expected_types)?,
        when_clauses: merge
            .when_clauses
            .iter()
            .map(|clause| {
                Ok(MergeWhenClause {
                    matched: clause.matched,
                    condition: clause
                        .condition
                        .as_ref()
                        .map(|expr| bind_expr_params(expr, params, expected_types))
                        .transpose()?,
                    action: match &clause.action {
                        MergeAction::Update { assignments } => MergeAction::Update {
                            assignments: assignments
                                .iter()
                                .map(|assignment| {
                                    Ok(UpdateAssignment {
                                        column: assignment.column.clone(),
                                        expr: bind_expr_params(
                                            &assignment.expr,
                                            params,
                                            expected_types,
                                        )?,
                                        span: assignment.span,
                                    })
                                })
                                .collect::<DbResult<Vec<_>>>()?,
                        },
                        MergeAction::Delete => MergeAction::Delete,
                        MergeAction::Insert { columns, values } => MergeAction::Insert {
                            columns: columns.clone(),
                            values: values
                                .iter()
                                .map(|value| bind_expr_params(value, params, expected_types))
                                .collect::<DbResult<Vec<_>>>()?,
                        },
                        MergeAction::InsertDefaultValues => MergeAction::InsertDefaultValues,
                        MergeAction::DoNothing => MergeAction::DoNothing,
                    },
                    span: clause.span,
                })
            })
            .collect::<DbResult<Vec<_>>>()?,
        span: merge.span,
    })
}

fn bind_select_params(
    select: &SelectStatement,
    params: &[Value],
    expected_types: &[DataType],
) -> DbResult<SelectStatement> {
    let _guard = enter_param_bind_depth(
        &PARAM_BIND_SELECT_DEPTH,
        MAX_PARAM_BIND_SELECT_DEPTH,
        "select",
    )?;
    Ok(SelectStatement {
        row_lock: select.row_lock.clone(),
        ctes: select
            .ctes
            .iter()
            .map(|cte| {
                Ok(aiondb_parser::CteDefinition {
                    name: cte.name.clone(),
                    column_aliases: cte.column_aliases.clone(),
                    recursive: cte.recursive,
                    query: Box::new(bind_statement_params(&cte.query, params, expected_types)?),
                    recursive_term: cte
                        .recursive_term
                        .as_ref()
                        .map(|term| bind_select_params(term, params, expected_types).map(Box::new))
                        .transpose()?,
                    union_all: cte.union_all,
                    span: cte.span,
                })
            })
            .collect::<DbResult<Vec<_>>>()?,
        items: select
            .items
            .iter()
            .map(|item| {
                Ok(SelectItem {
                    expr: bind_expr_params(&item.expr, params, expected_types)?,
                    alias: item.alias.clone(),
                    span: item.span,
                })
            })
            .collect::<DbResult<Vec<_>>>()?,
        from: select.from.clone(),
        from_alias: select.from_alias.clone(),
        from_span: select.from_span,
        joins: select
            .joins
            .iter()
            .map(|join| {
                Ok(JoinClause {
                    join_type: join.join_type,
                    table: join.table.clone(),
                    alias: join.alias.clone(),
                    condition: join
                        .condition
                        .as_ref()
                        .map(|expr| bind_expr_params(expr, params, expected_types))
                        .transpose()?,
                    using_columns: join.using_columns.clone(),
                    using_alias: join.using_alias.clone(),
                    natural: join.natural,
                    span: join.span,
                })
            })
            .collect::<DbResult<Vec<_>>>()?,
        selection: select
            .selection
            .as_ref()
            .map(|expr| bind_expr_params(expr, params, expected_types))
            .transpose()?,
        where_span: select.where_span,
        group_by: select
            .group_by
            .iter()
            .map(|expr| bind_expr_params(expr, params, expected_types))
            .collect::<DbResult<Vec<_>>>()?,
        group_by_items: select.group_by_items.clone(),
        group_by_span: select.group_by_span,
        having: select
            .having
            .as_ref()
            .map(|expr| bind_expr_params(expr, params, expected_types))
            .transpose()?,
        having_span: select.having_span,
        window_definitions: select
            .window_definitions
            .iter()
            .map(|window| {
                Ok(WindowDefinition {
                    name: window.name.clone(),
                    partition_by: window
                        .partition_by
                        .iter()
                        .map(|expr| bind_expr_params(expr, params, expected_types))
                        .collect::<DbResult<Vec<_>>>()?,
                    order_by: window
                        .order_by
                        .iter()
                        .map(|item| {
                            Ok(OrderByItem {
                                expr: bind_expr_params(&item.expr, params, expected_types)?,
                                descending: item.descending,
                                nulls_first: item.nulls_first,
                                span: item.span,
                            })
                        })
                        .collect::<DbResult<Vec<_>>>()?,
                })
            })
            .collect::<DbResult<Vec<_>>>()?,
        order_by: select
            .order_by
            .iter()
            .map(|item| {
                Ok(OrderByItem {
                    expr: bind_expr_params(&item.expr, params, expected_types)?,
                    descending: item.descending,
                    nulls_first: item.nulls_first,
                    span: item.span,
                })
            })
            .collect::<DbResult<Vec<_>>>()?,
        order_by_span: select.order_by_span,
        limit: select
            .limit
            .as_ref()
            .map(|expr| bind_expr_params(expr, params, expected_types))
            .transpose()?,
        limit_span: select.limit_span,
        offset: select
            .offset
            .as_ref()
            .map(|expr| bind_expr_params(expr, params, expected_types))
            .transpose()?,
        offset_span: select.offset_span,
        distinct: match &select.distinct {
            aiondb_parser::DistinctKind::DistinctOn(exprs) => {
                aiondb_parser::DistinctKind::DistinctOn(
                    exprs
                        .iter()
                        .map(|expr| bind_expr_params(expr, params, expected_types))
                        .collect::<DbResult<Vec<_>>>()?,
                )
            }
            other => other.clone(),
        },
        span: select.span,
    })
}

fn bind_cypher_statement_params(
    statement: &aiondb_parser::cypher_ast::CypherStatement,
    params: &[Value],
    expected_types: &[DataType],
) -> DbResult<aiondb_parser::cypher_ast::CypherStatement> {
    let _guard = enter_param_bind_depth(
        &PARAM_BIND_CYPHER_DEPTH,
        MAX_PARAM_BIND_CYPHER_DEPTH,
        "cypher",
    )?;
    Ok(aiondb_parser::cypher_ast::CypherStatement {
        clauses: statement
            .clauses
            .iter()
            .map(|clause| bind_cypher_clause_params(clause, params, expected_types))
            .collect::<DbResult<Vec<_>>>()?,
        union: statement
            .union
            .as_ref()
            .map(|union| {
                Ok::<Box<aiondb_parser::cypher_ast::CypherUnion>, DbError>(Box::new(
                    aiondb_parser::cypher_ast::CypherUnion {
                        all: union.all,
                        right: bind_cypher_statement_params(&union.right, params, expected_types)?,
                        span: union.span,
                    },
                ))
            })
            .transpose()?,
        span: statement.span,
    })
}

fn bind_cypher_clause_params(
    clause: &aiondb_parser::cypher_ast::CypherClause,
    params: &[Value],
    expected_types: &[DataType],
) -> DbResult<aiondb_parser::cypher_ast::CypherClause> {
    let _guard = enter_param_bind_depth(
        &PARAM_BIND_CYPHER_DEPTH,
        MAX_PARAM_BIND_CYPHER_DEPTH,
        "cypher",
    )?;
    use aiondb_parser::cypher_ast::CypherClause;
    Ok(match clause {
        CypherClause::Match(match_clause) => {
            CypherClause::Match(aiondb_parser::cypher_ast::CypherMatchClause {
                optional: match_clause.optional,
                patterns: bind_cypher_patterns_params(
                    &match_clause.patterns,
                    params,
                    expected_types,
                )?,
                where_clause: match_clause
                    .where_clause
                    .as_ref()
                    .map(|expr| bind_expr_params(expr, params, expected_types))
                    .transpose()?,
                span: match_clause.span,
            })
        }
        CypherClause::Create(create) => {
            CypherClause::Create(aiondb_parser::cypher_ast::CypherCreateClause {
                patterns: bind_cypher_patterns_params(&create.patterns, params, expected_types)?,
                span: create.span,
            })
        }
        CypherClause::Merge(merge) => {
            CypherClause::Merge(aiondb_parser::cypher_ast::CypherMergeClause {
                pattern: bind_cypher_pattern_params(&merge.pattern, params, expected_types)?,
                actions: merge
                    .actions
                    .iter()
                    .map(|action| {
                        Ok(aiondb_parser::cypher_ast::CypherMergeAction {
                            on_create: action.on_create,
                            items: action
                                .items
                                .iter()
                                .map(|item| {
                                    bind_cypher_set_item_params(item, params, expected_types)
                                })
                                .collect::<DbResult<Vec<_>>>()?,
                            span: action.span,
                        })
                    })
                    .collect::<DbResult<Vec<_>>>()?,
                span: merge.span,
            })
        }
        CypherClause::Set(set) => CypherClause::Set(aiondb_parser::cypher_ast::CypherSetClause {
            items: set
                .items
                .iter()
                .map(|item| bind_cypher_set_item_params(item, params, expected_types))
                .collect::<DbResult<Vec<_>>>()?,
            span: set.span,
        }),
        CypherClause::Delete(delete) => CypherClause::Delete(delete.clone()),
        CypherClause::Unwind(unwind) => {
            CypherClause::Unwind(aiondb_parser::cypher_ast::CypherUnwindClause {
                expr: bind_expr_params(&unwind.expr, params, expected_types)?,
                variable: unwind.variable.clone(),
                span: unwind.span,
            })
        }
        CypherClause::Remove(remove) => CypherClause::Remove(remove.clone()),
        CypherClause::With(with) => {
            CypherClause::With(aiondb_parser::cypher_ast::CypherWithClause {
                distinct: with.distinct,
                items: with
                    .items
                    .iter()
                    .map(|item| bind_cypher_return_item_params(item, params, expected_types))
                    .collect::<DbResult<Vec<_>>>()?,
                where_clause: with
                    .where_clause
                    .as_ref()
                    .map(|expr| bind_expr_params(expr, params, expected_types))
                    .transpose()?,
                order_by: bind_order_by_items_params(&with.order_by, params, expected_types)?,
                skip: with
                    .skip
                    .as_ref()
                    .map(|expr| bind_expr_params(expr, params, expected_types))
                    .transpose()?,
                limit: with
                    .limit
                    .as_ref()
                    .map(|expr| bind_expr_params(expr, params, expected_types))
                    .transpose()?,
                span: with.span,
            })
        }
        CypherClause::Return(ret) => {
            CypherClause::Return(aiondb_parser::cypher_ast::CypherReturnClause {
                distinct: ret.distinct,
                items: ret
                    .items
                    .iter()
                    .map(|item| bind_cypher_return_item_params(item, params, expected_types))
                    .collect::<DbResult<Vec<_>>>()?,
                order_by: bind_order_by_items_params(&ret.order_by, params, expected_types)?,
                skip: ret
                    .skip
                    .as_ref()
                    .map(|expr| bind_expr_params(expr, params, expected_types))
                    .transpose()?,
                limit: ret
                    .limit
                    .as_ref()
                    .map(|expr| bind_expr_params(expr, params, expected_types))
                    .transpose()?,
                span: ret.span,
            })
        }
        CypherClause::Call(call) => {
            CypherClause::Call(aiondb_parser::cypher_ast::CypherCallClause {
                procedure: call.procedure.clone(),
                args: call
                    .args
                    .iter()
                    .map(|expr| bind_expr_params(expr, params, expected_types))
                    .collect::<DbResult<Vec<_>>>()?,
                yields: call.yields.clone(),
                subquery: call
                    .subquery
                    .as_deref()
                    .map(|query| bind_cypher_statement_params(query, params, expected_types))
                    .transpose()?
                    .map(Box::new),
                span: call.span,
            })
        }
        CypherClause::Foreach(foreach) => {
            CypherClause::Foreach(aiondb_parser::cypher_ast::CypherForeachClause {
                variable: foreach.variable.clone(),
                expr: bind_expr_params(&foreach.expr, params, expected_types)?,
                clauses: foreach
                    .clauses
                    .iter()
                    .map(|clause| bind_cypher_clause_params(clause, params, expected_types))
                    .collect::<DbResult<Vec<_>>>()?,
                span: foreach.span,
            })
        }
    })
}

fn bind_cypher_patterns_params(
    patterns: &[aiondb_parser::cypher_ast::CypherPathPattern],
    params: &[Value],
    expected_types: &[DataType],
) -> DbResult<Vec<aiondb_parser::cypher_ast::CypherPathPattern>> {
    patterns
        .iter()
        .map(|pattern| bind_cypher_pattern_params(pattern, params, expected_types))
        .collect()
}

fn bind_cypher_pattern_params(
    pattern: &aiondb_parser::cypher_ast::CypherPathPattern,
    params: &[Value],
    expected_types: &[DataType],
) -> DbResult<aiondb_parser::cypher_ast::CypherPathPattern> {
    Ok(aiondb_parser::cypher_ast::CypherPathPattern {
        path_function: pattern.path_function,
        nodes: pattern
            .nodes
            .iter()
            .map(|node| {
                Ok(aiondb_parser::cypher_ast::CypherNodePattern {
                    variable: node.variable.clone(),
                    labels: node.labels.clone(),
                    properties: node
                        .properties
                        .iter()
                        .map(|(key, expr)| {
                            Ok((key.clone(), bind_expr_params(expr, params, expected_types)?))
                        })
                        .collect::<DbResult<Vec<_>>>()?,
                    span: node.span,
                })
            })
            .collect::<DbResult<Vec<_>>>()?,
        rels: pattern
            .rels
            .iter()
            .map(|rel| {
                Ok(aiondb_parser::cypher_ast::CypherRelPattern {
                    variable: rel.variable.clone(),
                    rel_type: rel.rel_type.clone(),
                    rel_types_alt: rel.rel_types_alt.clone(),
                    direction: rel.direction,
                    variable_length: rel.variable_length,
                    min_hops: rel.min_hops,
                    max_hops: rel.max_hops,
                    properties: rel
                        .properties
                        .iter()
                        .map(|(key, expr)| {
                            Ok((key.clone(), bind_expr_params(expr, params, expected_types)?))
                        })
                        .collect::<DbResult<Vec<_>>>()?,
                    span: rel.span,
                })
            })
            .collect::<DbResult<Vec<_>>>()?,
        path_variable: pattern.path_variable.clone(),
        span: pattern.span,
    })
}

fn bind_cypher_set_item_params(
    item: &aiondb_parser::cypher_ast::CypherSetItem,
    params: &[Value],
    expected_types: &[DataType],
) -> DbResult<aiondb_parser::cypher_ast::CypherSetItem> {
    use aiondb_parser::cypher_ast::CypherSetItem;
    Ok(match item {
        CypherSetItem::Property {
            variable,
            property,
            expr,
            span,
        } => CypherSetItem::Property {
            variable: variable.clone(),
            property: property.clone(),
            expr: bind_expr_params(expr, params, expected_types)?,
            span: *span,
        },
        CypherSetItem::Label {
            variable,
            label,
            span,
        } => CypherSetItem::Label {
            variable: variable.clone(),
            label: label.clone(),
            span: *span,
        },
        CypherSetItem::ReplaceProperties {
            variable,
            entries,
            span,
        } => CypherSetItem::ReplaceProperties {
            variable: variable.clone(),
            entries: bind_cypher_property_map_params(entries, params, expected_types)?,
            span: *span,
        },
        CypherSetItem::MergeProperties {
            variable,
            entries,
            span,
        } => CypherSetItem::MergeProperties {
            variable: variable.clone(),
            entries: bind_cypher_property_map_params(entries, params, expected_types)?,
            span: *span,
        },
    })
}

fn bind_cypher_property_map_params(
    entries: &[(String, Box<Expr>)],
    params: &[Value],
    expected_types: &[DataType],
) -> DbResult<Vec<(String, Box<Expr>)>> {
    entries
        .iter()
        .map(|(key, expr)| {
            Ok((
                key.clone(),
                Box::new(bind_expr_params(expr, params, expected_types)?),
            ))
        })
        .collect()
}

fn bind_cypher_return_item_params(
    item: &aiondb_parser::cypher_ast::CypherReturnItem,
    params: &[Value],
    expected_types: &[DataType],
) -> DbResult<aiondb_parser::cypher_ast::CypherReturnItem> {
    Ok(aiondb_parser::cypher_ast::CypherReturnItem {
        expr: bind_expr_params(&item.expr, params, expected_types)?,
        alias: item.alias.clone(),
        span: item.span,
    })
}

fn bind_order_by_items_params(
    items: &[OrderByItem],
    params: &[Value],
    expected_types: &[DataType],
) -> DbResult<Vec<OrderByItem>> {
    items
        .iter()
        .map(|item| {
            Ok(OrderByItem {
                expr: bind_expr_params(&item.expr, params, expected_types)?,
                descending: item.descending,
                nulls_first: item.nulls_first,
                span: item.span,
            })
        })
        .collect()
}

fn bind_set_operation_params(
    set_op: &SetOperationStatement,
    params: &[Value],
    expected_types: &[DataType],
) -> DbResult<SetOperationStatement> {
    Ok(SetOperationStatement {
        op: set_op.op,
        all: set_op.all,
        left: Box::new(bind_statement_params(&set_op.left, params, expected_types)?),
        right: Box::new(bind_statement_params(
            &set_op.right,
            params,
            expected_types,
        )?),
        order_by: set_op
            .order_by
            .iter()
            .map(|item| {
                Ok(OrderByItem {
                    expr: bind_expr_params(&item.expr, params, expected_types)?,
                    descending: item.descending,
                    nulls_first: item.nulls_first,
                    span: item.span,
                })
            })
            .collect::<DbResult<Vec<_>>>()?,
        order_by_span: set_op.order_by_span,
        limit: set_op
            .limit
            .as_ref()
            .map(|expr| bind_expr_params(expr, params, expected_types))
            .transpose()?,
        limit_span: set_op.limit_span,
        offset: set_op
            .offset
            .as_ref()
            .map(|expr| bind_expr_params(expr, params, expected_types))
            .transpose()?,
        offset_span: set_op.offset_span,
        span: set_op.span,
    })
}

fn bind_expr_params(expr: &Expr, params: &[Value], expected_types: &[DataType]) -> DbResult<Expr> {
    let _guard = enter_param_bind_depth(
        &PARAM_BIND_EXPR_DEPTH,
        MAX_PARAM_BIND_EXPR_DEPTH,
        "expression",
    )?;
    match expr {
        Expr::Literal(_, _) | Expr::Identifier(_) | Expr::Default { .. } => Ok(expr.clone()),
        Expr::Parameter { index, span } => {
            let value = params.get(index.saturating_sub(1)).ok_or_else(|| {
                DbError::protocol(format!("missing bound value for parameter ${index}"))
            })?;
            value_to_expr_with_expected(value, expected_types.get(index.saturating_sub(1)), *span)
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
                .map(|arg| bind_expr_params(arg, params, expected_types))
                .collect::<DbResult<Vec<_>>>()?,
            distinct: *distinct,
            filter: filter
                .as_ref()
                .map(|f| bind_expr_params(f, params, expected_types))
                .transpose()?
                .map(Box::new),
            span: *span,
        }),
        Expr::UnaryOp { op, expr, span } => Ok(Expr::UnaryOp {
            op: *op,
            expr: Box::new(bind_expr_params(expr, params, expected_types)?),
            span: *span,
        }),
        Expr::BinaryOp {
            left,
            op,
            right,
            span,
        } => Ok(Expr::BinaryOp {
            left: Box::new(bind_expr_params(left, params, expected_types)?),
            op: *op,
            right: Box::new(bind_expr_params(right, params, expected_types)?),
            span: *span,
        }),
        Expr::IsNull {
            expr,
            negated,
            span,
        } => Ok(Expr::IsNull {
            expr: Box::new(bind_expr_params(expr, params, expected_types)?),
            negated: *negated,
            span: *span,
        }),
        Expr::IsDistinctFrom {
            left,
            right,
            negated,
            span,
        } => Ok(Expr::IsDistinctFrom {
            left: Box::new(bind_expr_params(left, params, expected_types)?),
            right: Box::new(bind_expr_params(right, params, expected_types)?),
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
            expr: Box::new(bind_expr_params(expr, params, expected_types)?),
            pattern: Box::new(bind_expr_params(pattern, params, expected_types)?),
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
            expr: Box::new(bind_expr_params(expr, params, expected_types)?),
            list: list
                .iter()
                .map(|item| bind_expr_params(item, params, expected_types))
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
            expr: Box::new(bind_expr_params(expr, params, expected_types)?),
            low: Box::new(bind_expr_params(low, params, expected_types)?),
            high: Box::new(bind_expr_params(high, params, expected_types)?),
            negated: *negated,
            span: *span,
        }),
        Expr::Cast {
            expr,
            data_type,
            span,
        } => Ok(Expr::Cast {
            expr: Box::new(bind_expr_params(expr, params, expected_types)?),
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
                .as_ref()
                .map(|e| bind_expr_params(e, params, expected_types).map(Box::new))
                .transpose()?,
            conditions: conditions
                .iter()
                .map(|e| bind_expr_params(e, params, expected_types))
                .collect::<DbResult<Vec<_>>>()?,
            results: results
                .iter()
                .map(|e| bind_expr_params(e, params, expected_types))
                .collect::<DbResult<Vec<_>>>()?,
            else_result: else_result
                .as_ref()
                .map(|e| bind_expr_params(e, params, expected_types).map(Box::new))
                .transpose()?,
            span: *span,
        }),
        Expr::Array { elements, span } => Ok(Expr::Array {
            elements: elements
                .iter()
                .map(|e| bind_expr_params(e, params, expected_types))
                .collect::<DbResult<Vec<_>>>()?,
            span: *span,
        }),
        Expr::ArraySubquery { query, span } => Ok(Expr::ArraySubquery {
            query: Box::new(bind_select_params(query, params, expected_types)?),
            span: *span,
        }),
        Expr::Subquery { query, span } => Ok(Expr::Subquery {
            query: Box::new(bind_select_params(query, params, expected_types)?),
            span: *span,
        }),
        Expr::InSubquery {
            expr,
            query,
            negated,
            span,
        } => Ok(Expr::InSubquery {
            expr: Box::new(bind_expr_params(expr, params, expected_types)?),
            query: Box::new(bind_select_params(query, params, expected_types)?),
            negated: *negated,
            span: *span,
        }),
        Expr::Exists {
            query,
            negated,
            span,
        } => Ok(Expr::Exists {
            query: Box::new(bind_select_params(query, params, expected_types)?),
            negated: *negated,
            span: *span,
        }),
        Expr::CypherExists {
            query,
            negated,
            span,
        } => Ok(Expr::CypherExists {
            query: Box::new(bind_cypher_statement_params(query, params, expected_types)?),
            negated: *negated,
            span: *span,
        }),
        Expr::CypherPatternComprehension {
            pattern,
            where_clause,
            map_expr,
            span,
        } => Ok(Expr::CypherPatternComprehension {
            pattern: bind_cypher_pattern_params(pattern, params, expected_types)?,
            where_clause: where_clause
                .as_deref()
                .map(|expr| bind_expr_params(expr, params, expected_types))
                .transpose()?
                .map(Box::new),
            map_expr: Box::new(bind_expr_params(map_expr, params, expected_types)?),
            span: *span,
        }),
        Expr::WindowFunction {
            function,
            partition_by,
            order_by,
            window_name,
            span,
        } => Ok(Expr::WindowFunction {
            function: Box::new(bind_expr_params(function, params, expected_types)?),
            partition_by: partition_by
                .iter()
                .map(|e| bind_expr_params(e, params, expected_types))
                .collect::<DbResult<Vec<_>>>()?,
            order_by: order_by
                .iter()
                .map(|item| {
                    Ok(OrderByItem {
                        expr: bind_expr_params(&item.expr, params, expected_types)?,
                        descending: item.descending,
                        nulls_first: item.nulls_first,
                        span: item.span,
                    })
                })
                .collect::<DbResult<Vec<_>>>()?,
            window_name: window_name.clone(),
            span: *span,
        }),
    }
}

fn value_to_expr_with_expected(
    value: &Value,
    expected_type: Option<&DataType>,
    span: aiondb_parser::Span,
) -> DbResult<Expr> {
    let expr = value_to_expr(value, span)?;
    match (value, expected_type) {
        (Value::Array(_), Some(expected @ DataType::Array(_)))
        | (Value::Blob(_), Some(expected @ DataType::Blob)) => Ok(Expr::Cast {
            expr: Box::new(expr),
            data_type: expected.clone(),
            span,
        }),
        _ => Ok(expr),
    }
}

fn value_matches_expected_type(value: &Value, expected: &DataType) -> bool {
    value_matches_expected_type_at_depth(value, expected, 0)
}

fn value_matches_expected_type_at_depth(value: &Value, expected: &DataType, depth: usize) -> bool {
    if depth >= MAX_PARAM_VALUE_EXPR_DEPTH {
        return false;
    }
    match (value, expected) {
        (Value::Null, _) => true,
        (Value::LargeDate(_), DataType::Date) => true,
        (Value::Vector(vector), DataType::Vector { dims, .. }) => {
            *dims == 0 || vector.dims == *dims
        }
        (Value::Array(elements), DataType::Array(expected_elem)) => {
            elements.iter().all(|element| {
                array_element_matches_expected_type_at_depth(
                    element,
                    expected_elem.as_ref(),
                    depth + 1,
                )
            })
        }
        _ => value.data_type().as_ref().is_some_and(|ty| ty == expected),
    }
}

fn array_element_matches_expected_type_at_depth(
    value: &Value,
    expected_element: &DataType,
    depth: usize,
) -> bool {
    if depth >= MAX_PARAM_VALUE_EXPR_DEPTH {
        return false;
    }
    match value {
        Value::Null => true,
        Value::Array(elements) => elements.iter().all(|element| {
            array_element_matches_expected_type_at_depth(element, expected_element, depth + 1)
        }),
        _ => value_matches_expected_type_at_depth(value, expected_element, depth + 1),
    }
}

fn value_to_expr(value: &Value, span: aiondb_parser::Span) -> DbResult<Expr> {
    value_to_expr_at_depth(value, span, 0)
}

fn value_to_expr_at_depth(
    value: &Value,
    span: aiondb_parser::Span,
    depth: usize,
) -> DbResult<Expr> {
    if depth >= MAX_PARAM_VALUE_EXPR_DEPTH {
        return Err(DbError::program_limit(format!(
            "portal parameter nesting depth exceeds limit {MAX_PARAM_VALUE_EXPR_DEPTH}"
        )));
    }
    let literal = match value {
        Value::Null => Literal::Null,
        Value::Int(value) => Literal::Integer(i64::from(*value)),
        Value::Text(value) => Literal::String(value.clone()),
        Value::Boolean(value) => Literal::Boolean(*value),
        Value::BigInt(value) => {
            return Ok(Expr::Cast {
                expr: Box::new(Expr::Literal(Literal::Integer(*value), span)),
                data_type: DataType::BigInt,
                span,
            });
        }
        Value::Real(value) => Literal::NumericLit(value.to_string()),
        Value::Double(value) => Literal::NumericLit(value.to_string()),
        Value::Numeric(value) => Literal::NumericLit(value.to_string()),
        Value::Money(value) => {
            return Ok(Expr::Cast {
                expr: Box::new(Expr::Literal(
                    Literal::String(Value::Money(*value).to_string()),
                    span,
                )),
                data_type: DataType::Money,
                span,
            });
        }
        Value::Date(value) => {
            return Ok(Expr::Cast {
                expr: Box::new(Expr::Literal(Literal::String(value.to_string()), span)),
                data_type: DataType::Date,
                span,
            });
        }
        Value::LargeDate(value) => {
            return Ok(Expr::Cast {
                expr: Box::new(Expr::Literal(Literal::String(value.to_string()), span)),
                data_type: DataType::Date,
                span,
            });
        }
        Value::Time(value) => {
            return Ok(Expr::Cast {
                expr: Box::new(Expr::Literal(Literal::String(value.to_string()), span)),
                data_type: DataType::Time,
                span,
            });
        }
        Value::TimeTz(value, offset) => {
            return Ok(Expr::Cast {
                expr: Box::new(Expr::Literal(
                    Literal::String(format!("{value}{offset}")),
                    span,
                )),
                data_type: DataType::TimeTz,
                span,
            });
        }
        Value::Timestamp(value) => {
            return Ok(Expr::Cast {
                expr: Box::new(Expr::Literal(Literal::String(value.to_string()), span)),
                data_type: DataType::Timestamp,
                span,
            });
        }
        Value::TimestampTz(value) => {
            return Ok(Expr::Cast {
                expr: Box::new(Expr::Literal(Literal::String(value.to_string()), span)),
                data_type: DataType::TimestampTz,
                span,
            });
        }
        Value::Interval(value) => {
            return Ok(Expr::Cast {
                expr: Box::new(Expr::Literal(Literal::String(value.to_string()), span)),
                data_type: DataType::Interval,
                span,
            });
        }
        Value::Uuid(value) => {
            return Ok(Expr::Cast {
                expr: Box::new(Expr::Literal(
                    Literal::String(Value::Uuid(*value).to_string()),
                    span,
                )),
                data_type: DataType::Uuid,
                span,
            });
        }
        Value::Tid(value) => {
            return Ok(Expr::Cast {
                expr: Box::new(Expr::Literal(Literal::String(value.to_string()), span)),
                data_type: DataType::Tid,
                span,
            });
        }
        Value::PgLsn(value) => {
            return Ok(Expr::Cast {
                expr: Box::new(Expr::Literal(Literal::String(value.to_string()), span)),
                data_type: DataType::PgLsn,
                span,
            });
        }
        Value::Jsonb(value) => {
            return Ok(Expr::Cast {
                expr: Box::new(Expr::Literal(Literal::String(value.to_string()), span)),
                data_type: DataType::Jsonb,
                span,
            });
        }
        Value::MacAddr(value) => {
            return Ok(Expr::Cast {
                expr: Box::new(Expr::Literal(Literal::String(value.to_string()), span)),
                data_type: DataType::MacAddr,
                span,
            });
        }
        Value::MacAddr8(value) => {
            return Ok(Expr::Cast {
                expr: Box::new(Expr::Literal(Literal::String(value.to_string()), span)),
                data_type: DataType::MacAddr8,
                span,
            });
        }
        Value::Blob(bytes) => {
            let hex = bytes.iter().fold(
                String::with_capacity(bytes.len().saturating_mul(2)),
                |mut out, byte| {
                    use std::fmt::Write as _;
                    let _ = write!(out, "{byte:02x}");
                    out
                },
            );
            Literal::String(format!("\\x{hex}"))
        }
        Value::Vector(vector) => {
            let values = vector
                .values
                .iter()
                .map(|value| value.to_string())
                .collect::<Vec<_>>()
                .join(",");
            return Ok(Expr::Cast {
                expr: Box::new(Expr::Literal(Literal::String(format!("[{values}]")), span)),
                data_type: DataType::Vector {
                    dims: vector.dims,
                    element_type: aiondb_core::VectorElementType::Float32,
                },
                span,
            });
        }
        Value::Array(elements) => {
            return Ok(Expr::Array {
                elements: elements
                    .iter()
                    .map(|element| value_to_expr_at_depth(element, span, depth + 1))
                    .collect::<DbResult<Vec<_>>>()?,
                span,
            });
        }
    };
    Ok(Expr::Literal(literal, span))
}

#[cfg(test)]
mod tests {
    use super::*;
    use aiondb_parser::ast::CompatTaggedStatement;
    use aiondb_parser::Span;

    #[test]
    fn nested_array_param_matches_flat_array_type() {
        let params = vec![Value::Array(vec![
            Value::Array(vec![Value::Int(1), Value::Int(2)]),
            Value::Array(vec![Value::Int(3), Value::Int(4)]),
        ])];

        assert!(ensure_portal_param_types_compatible(
            &[DataType::Array(Box::new(DataType::Int))],
            &params
        )
        .is_ok());
    }

    #[test]
    fn nested_array_param_still_rejects_wrong_leaf_type() {
        let params = vec![Value::Array(vec![Value::Array(vec![Value::Text(
            "oops".to_owned(),
        )])])];

        let err = ensure_portal_param_types_compatible(
            &[DataType::Array(Box::new(DataType::Int))],
            &params,
        )
        .expect_err("wrong nested leaf type should be rejected");
        assert!(err.to_string().contains("parameter $1 expects INT[]"));
    }

    #[test]
    fn compat_tagged_statement_with_params_fails_explicitly() {
        let statement = Statement::CompatTagged(CompatTaggedStatement {
            tag: "ALTER TABLE".to_owned(),
            raw_sql: "ALTER TABLE t ADD COLUMN c int DEFAULT $1".to_owned(),
            span: Span::new(0, 1),
        });

        let err = bind_statement_params(&statement, &[Value::Int(1)], &[DataType::Int])
            .expect_err("compat-tagged statement binding should fail");
        assert!(
            err.to_string()
                .contains("parameter binding is not supported for compatibility-tagged statements"),
            "unexpected error: {err}"
        );
    }
}
