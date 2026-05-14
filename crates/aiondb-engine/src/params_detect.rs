#![allow(clippy::pedantic)]

use super::*;

enum ParameterWork<'a> {
    Statement(&'a Statement),
    Select(&'a SelectStatement),
    SetOperation(&'a SetOperationStatement),
    Expr(&'a Expr),
    SelectItems(&'a [SelectItem]),
    Merge(&'a MergeStatement),
    CypherStatement(&'a aiondb_parser::cypher_ast::CypherStatement),
    CypherClause(&'a aiondb_parser::cypher_ast::CypherClause),
    CypherPattern(&'a aiondb_parser::cypher_ast::CypherPathPattern),
    CypherSetItem(&'a aiondb_parser::cypher_ast::CypherSetItem),
}

pub(crate) fn statement_contains_parameters(statement: &Statement) -> bool {
    contains_parameters_work([ParameterWork::Statement(statement)])
}

fn contains_parameters_work<'a>(initial: impl IntoIterator<Item = ParameterWork<'a>>) -> bool {
    let mut stack: Vec<ParameterWork<'a>> = initial.into_iter().collect();
    while let Some(work) = stack.pop() {
        match work {
            ParameterWork::Statement(statement) => push_statement_work(statement, &mut stack),
            ParameterWork::Select(select) => push_select_work(select, &mut stack),
            ParameterWork::SetOperation(set_op) => push_set_operation_work(set_op, &mut stack),
            ParameterWork::Expr(expr) => {
                if push_expr_work(expr, &mut stack) {
                    return true;
                }
            }
            ParameterWork::SelectItems(items) => {
                for item in items {
                    stack.push(ParameterWork::Expr(&item.expr));
                }
            }
            ParameterWork::Merge(merge) => push_merge_work(merge, &mut stack),
            ParameterWork::CypherStatement(statement) => {
                push_cypher_statement_work(statement, &mut stack);
            }
            ParameterWork::CypherClause(clause) => push_cypher_clause_work(clause, &mut stack),
            ParameterWork::CypherPattern(pattern) => {
                push_cypher_pattern_work(pattern, &mut stack);
            }
            ParameterWork::CypherSetItem(item) => push_cypher_set_item_work(item, &mut stack),
        }
    }
    false
}

fn push_statement_work<'a>(statement: &'a Statement, stack: &mut Vec<ParameterWork<'a>>) {
    match statement {
        Statement::Begin { .. }
        | Statement::Commit { .. }
        | Statement::Rollback { .. }
        | Statement::PrepareTransaction { .. }
        | Statement::PrepareStmt { .. }
        | Statement::ExecuteStmt { .. }
        | Statement::DeallocateStmt { .. }
        | Statement::DeclareStmt { .. }
        | Statement::FetchStmt { .. }
        | Statement::MoveStmt { .. }
        | Statement::CloseStmt { .. }
        | Statement::CommitPrepared { .. }
        | Statement::RollbackPrepared { .. }
        | Statement::Checkpoint { .. }
        | Statement::Savepoint { .. }
        | Statement::RollbackToSavepoint { .. }
        | Statement::ReleaseSavepoint { .. }
        | Statement::CreateTable(_)
        | Statement::CreateSequence(_)
        | Statement::CreateIndex(_)
        | Statement::TruncateTable(_)
        | Statement::DropTable(_)
        | Statement::DropIndex(_)
        | Statement::DropSequence(_)
        | Statement::AlterTable(_)
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
        | Statement::DropSchema(_)
        | Statement::Analyze { .. }
        | Statement::Vacuum { .. }
        | Statement::Backup { .. }
        | Statement::Restore { .. }
        | Statement::SetTransaction(_)
        | Statement::SetSessionCharacteristics(_)
        | Statement::SetConstraints(_)
        | Statement::CreateFunction(_)
        | Statement::DropFunction(_)
        | Statement::CreateTrigger(_)
        | Statement::DropTrigger(_)
        | Statement::CreateTableAs(_)
        | Statement::CreateTenant { .. }
        | Statement::DropTenant { .. }
        | Statement::SetTenant { .. }
        | Statement::SetVariable(_)
        | Statement::ShowVariable(_)
        | Statement::ResetVariable(_)
        | Statement::AlterTriggerRename(_)
        | Statement::CreateExtension(_)
        | Statement::DropExtension(_)
        | Statement::SecurityLabel(_)
        | Statement::Comment(_)
        | Statement::Listen { .. }
        | Statement::Unlisten { .. }
        | Statement::Notify { .. }
        | Statement::Lock(_)
        | Statement::Discard(_)
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
        | Statement::DoStmt { .. }
        | Statement::DropOwned(_)
        | Statement::ReassignOwned(_)
        | Statement::Load { .. }
        | Statement::AlterSystem(_) => {}
        Statement::Cypher(cypher) => stack.push(ParameterWork::CypherStatement(cypher)),
        statement if statement.compat_tag().is_some() => {}
        Statement::CreateSchema(schema) => {
            for statement in &schema.body {
                stack.push(ParameterWork::Statement(statement));
            }
        }
        Statement::Explain {
            statement: inner, ..
        } => stack.push(ParameterWork::Statement(inner)),
        Statement::Copy(copy) => {
            if let Some(inner) = copy.query.as_deref() {
                stack.push(ParameterWork::Statement(inner));
            }
        }
        Statement::SetOperation(set_op) => stack.push(ParameterWork::SetOperation(set_op)),
        Statement::Delete(delete) => {
            if let Some(selection) = &delete.selection {
                stack.push(ParameterWork::Expr(selection));
            }
            stack.push(ParameterWork::SelectItems(&delete.returning));
        }
        Statement::Insert(insert) => {
            for expr in insert.rows.iter().flatten() {
                stack.push(ParameterWork::Expr(expr));
            }
            if let Some(query) = &insert.query {
                stack.push(ParameterWork::Select(query));
            }
            if let Some(oc) = &insert.on_conflict {
                if let aiondb_parser::OnConflictAction::DoUpdate {
                    assignments,
                    where_clause,
                } = &oc.action
                {
                    for assignment in assignments {
                        stack.push(ParameterWork::Expr(&assignment.expr));
                    }
                    if let Some(where_clause) = where_clause {
                        stack.push(ParameterWork::Expr(where_clause));
                    }
                }
            }
            stack.push(ParameterWork::SelectItems(&insert.returning));
        }
        Statement::Select(select) => stack.push(ParameterWork::Select(select)),
        Statement::Update(update) => {
            for assignment in &update.assignments {
                stack.push(ParameterWork::Expr(&assignment.expr));
            }
            if let Some(selection) = &update.selection {
                stack.push(ParameterWork::Expr(selection));
            }
            stack.push(ParameterWork::SelectItems(&update.returning));
        }
        Statement::Merge(merge) => stack.push(ParameterWork::Merge(merge)),
        _ => {}
    }
}

fn push_select_work<'a>(select: &'a SelectStatement, stack: &mut Vec<ParameterWork<'a>>) {
    for cte in &select.ctes {
        stack.push(ParameterWork::Statement(&cte.query));
        if let Some(term) = &cte.recursive_term {
            stack.push(ParameterWork::Select(term));
        }
    }
    for item in &select.items {
        stack.push(ParameterWork::Expr(&item.expr));
    }
    for join in &select.joins {
        if let Some(condition) = &join.condition {
            stack.push(ParameterWork::Expr(condition));
        }
    }
    if let Some(selection) = &select.selection {
        stack.push(ParameterWork::Expr(selection));
    }
    stack.extend(select.group_by.iter().map(ParameterWork::Expr));
    if let Some(having) = &select.having {
        stack.push(ParameterWork::Expr(having));
    }
    for window in &select.window_definitions {
        stack.extend(window.partition_by.iter().map(ParameterWork::Expr));
        for item in &window.order_by {
            stack.push(ParameterWork::Expr(&item.expr));
        }
    }
    for item in &select.order_by {
        stack.push(ParameterWork::Expr(&item.expr));
    }
    if let Some(limit) = &select.limit {
        stack.push(ParameterWork::Expr(limit));
    }
    if let Some(offset) = &select.offset {
        stack.push(ParameterWork::Expr(offset));
    }
    if let aiondb_parser::DistinctKind::DistinctOn(exprs) = &select.distinct {
        stack.extend(exprs.iter().map(ParameterWork::Expr));
    }
}

fn push_expr_work<'a>(expr: &'a Expr, stack: &mut Vec<ParameterWork<'a>>) -> bool {
    match expr {
        Expr::Parameter { .. } => return true,
        Expr::Literal(_, _) | Expr::Identifier(_) | Expr::Default { .. } => {}
        Expr::FunctionCall { args, filter, .. } => {
            stack.extend(args.iter().map(ParameterWork::Expr));
            if let Some(filter) = filter {
                stack.push(ParameterWork::Expr(filter));
            }
        }
        Expr::UnaryOp { expr, .. } | Expr::IsNull { expr, .. } | Expr::Cast { expr, .. } => {
            stack.push(ParameterWork::Expr(expr));
        }
        Expr::BinaryOp { left, right, .. }
        | Expr::IsDistinctFrom { left, right, .. }
        | Expr::Like {
            expr: left,
            pattern: right,
            ..
        } => {
            stack.push(ParameterWork::Expr(right));
            stack.push(ParameterWork::Expr(left));
        }
        Expr::InList { expr, list, .. } => {
            stack.extend(list.iter().map(ParameterWork::Expr));
            stack.push(ParameterWork::Expr(expr));
        }
        Expr::Between {
            expr, low, high, ..
        } => {
            stack.push(ParameterWork::Expr(high));
            stack.push(ParameterWork::Expr(low));
            stack.push(ParameterWork::Expr(expr));
        }
        Expr::CaseWhen {
            operand,
            conditions,
            results,
            else_result,
            ..
        } => {
            if let Some(else_result) = else_result {
                stack.push(ParameterWork::Expr(else_result));
            }
            stack.extend(results.iter().map(ParameterWork::Expr));
            stack.extend(conditions.iter().map(ParameterWork::Expr));
            if let Some(operand) = operand {
                stack.push(ParameterWork::Expr(operand));
            }
        }
        Expr::Array { elements, .. } => {
            stack.extend(elements.iter().map(ParameterWork::Expr));
        }
        Expr::ArraySubquery { query, .. } | Expr::Subquery { query, .. } => {
            stack.push(ParameterWork::Select(query));
        }
        Expr::InSubquery { expr, query, .. } => {
            stack.push(ParameterWork::Select(query));
            stack.push(ParameterWork::Expr(expr));
        }
        Expr::Exists { query, .. } => stack.push(ParameterWork::Select(query)),
        Expr::CypherExists { query, .. } => stack.push(ParameterWork::CypherStatement(query)),
        Expr::CypherPatternComprehension {
            pattern,
            where_clause,
            map_expr,
            ..
        } => {
            stack.push(ParameterWork::CypherPattern(pattern));
            if let Some(where_clause) = where_clause {
                stack.push(ParameterWork::Expr(where_clause));
            }
            stack.push(ParameterWork::Expr(map_expr));
        }
        Expr::WindowFunction {
            function,
            partition_by,
            order_by,
            ..
        } => {
            stack.push(ParameterWork::Expr(function));
            stack.extend(partition_by.iter().map(ParameterWork::Expr));
            for item in order_by {
                stack.push(ParameterWork::Expr(&item.expr));
            }
        }
    }
    false
}

fn push_set_operation_work<'a>(
    set_op: &'a SetOperationStatement,
    stack: &mut Vec<ParameterWork<'a>>,
) {
    stack.push(ParameterWork::Statement(&set_op.right));
    stack.push(ParameterWork::Statement(&set_op.left));
    for item in &set_op.order_by {
        stack.push(ParameterWork::Expr(&item.expr));
    }
    if let Some(limit) = &set_op.limit {
        stack.push(ParameterWork::Expr(limit));
    }
    if let Some(offset) = &set_op.offset {
        stack.push(ParameterWork::Expr(offset));
    }
}

fn push_merge_work<'a>(merge: &'a MergeStatement, stack: &mut Vec<ParameterWork<'a>>) {
    if let MergeSource::Subquery(query) = &merge.source {
        stack.push(ParameterWork::Select(query));
    }
    stack.push(ParameterWork::Expr(&merge.on_condition));
    for clause in &merge.when_clauses {
        if let Some(condition) = &clause.condition {
            stack.push(ParameterWork::Expr(condition));
        }
        match &clause.action {
            MergeAction::Update { assignments } => {
                for assignment in assignments {
                    stack.push(ParameterWork::Expr(&assignment.expr));
                }
            }
            MergeAction::Insert { values, .. } => {
                stack.extend(values.iter().map(ParameterWork::Expr));
            }
            MergeAction::Delete | MergeAction::InsertDefaultValues | MergeAction::DoNothing => {}
        }
    }
}

fn push_cypher_statement_work<'a>(
    statement: &'a aiondb_parser::cypher_ast::CypherStatement,
    stack: &mut Vec<ParameterWork<'a>>,
) {
    for clause in &statement.clauses {
        stack.push(ParameterWork::CypherClause(clause));
    }
    if let Some(union) = &statement.union {
        stack.push(ParameterWork::CypherStatement(&union.right));
    }
}

fn push_cypher_clause_work<'a>(
    clause: &'a aiondb_parser::cypher_ast::CypherClause,
    stack: &mut Vec<ParameterWork<'a>>,
) {
    use aiondb_parser::cypher_ast::CypherClause;
    match clause {
        CypherClause::Match(match_clause) => {
            for pattern in &match_clause.patterns {
                stack.push(ParameterWork::CypherPattern(pattern));
            }
            if let Some(where_clause) = &match_clause.where_clause {
                stack.push(ParameterWork::Expr(where_clause));
            }
        }
        CypherClause::Create(create) => {
            for pattern in &create.patterns {
                stack.push(ParameterWork::CypherPattern(pattern));
            }
        }
        CypherClause::Merge(merge) => {
            stack.push(ParameterWork::CypherPattern(&merge.pattern));
            for action in &merge.actions {
                for item in &action.items {
                    stack.push(ParameterWork::CypherSetItem(item));
                }
            }
        }
        CypherClause::Set(set) => {
            for item in &set.items {
                stack.push(ParameterWork::CypherSetItem(item));
            }
        }
        CypherClause::Delete(_) | CypherClause::Remove(_) => {}
        CypherClause::Unwind(unwind) => stack.push(ParameterWork::Expr(&unwind.expr)),
        CypherClause::With(with) => {
            for item in &with.items {
                stack.push(ParameterWork::Expr(&item.expr));
            }
            if let Some(where_clause) = &with.where_clause {
                stack.push(ParameterWork::Expr(where_clause));
            }
            for item in &with.order_by {
                stack.push(ParameterWork::Expr(&item.expr));
            }
            if let Some(skip) = &with.skip {
                stack.push(ParameterWork::Expr(skip));
            }
            if let Some(limit) = &with.limit {
                stack.push(ParameterWork::Expr(limit));
            }
        }
        CypherClause::Return(ret) => {
            for item in &ret.items {
                stack.push(ParameterWork::Expr(&item.expr));
            }
            for item in &ret.order_by {
                stack.push(ParameterWork::Expr(&item.expr));
            }
            if let Some(skip) = &ret.skip {
                stack.push(ParameterWork::Expr(skip));
            }
            if let Some(limit) = &ret.limit {
                stack.push(ParameterWork::Expr(limit));
            }
        }
        CypherClause::Call(call) => {
            stack.extend(call.args.iter().map(ParameterWork::Expr));
            if let Some(subquery) = call.subquery.as_deref() {
                stack.push(ParameterWork::CypherStatement(subquery));
            }
        }
        CypherClause::Foreach(foreach) => {
            stack.push(ParameterWork::Expr(&foreach.expr));
            for clause in &foreach.clauses {
                stack.push(ParameterWork::CypherClause(clause));
            }
        }
    }
}

fn push_cypher_pattern_work<'a>(
    pattern: &'a aiondb_parser::cypher_ast::CypherPathPattern,
    stack: &mut Vec<ParameterWork<'a>>,
) {
    for node in &pattern.nodes {
        for (_, expr) in &node.properties {
            stack.push(ParameterWork::Expr(expr));
        }
    }
    for rel in &pattern.rels {
        for (_, expr) in &rel.properties {
            stack.push(ParameterWork::Expr(expr));
        }
    }
}

fn push_cypher_set_item_work<'a>(
    item: &'a aiondb_parser::cypher_ast::CypherSetItem,
    stack: &mut Vec<ParameterWork<'a>>,
) {
    use aiondb_parser::cypher_ast::CypherSetItem;
    match item {
        CypherSetItem::Property { expr, .. } => stack.push(ParameterWork::Expr(expr)),
        CypherSetItem::Label { .. } => {}
        CypherSetItem::ReplaceProperties { entries, .. }
        | CypherSetItem::MergeProperties { entries, .. } => {
            for (_, expr) in entries {
                stack.push(ParameterWork::Expr(expr));
            }
        }
    }
}
