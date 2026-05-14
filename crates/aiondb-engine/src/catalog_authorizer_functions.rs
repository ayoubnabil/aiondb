#[derive(Clone, Debug, Eq, PartialEq)]
struct FunctionExecTarget {
    name: QualifiedName,
    arg_types: Vec<DataType>,
}

fn required_function_execs(plan: &PhysicalPlan) -> Vec<FunctionExecTarget> {
    let mut functions = BTreeMap::new();
    collect_functions_from_physical_plan(plan, &mut functions);
    functions.into_values().collect()
}

fn insert_required_function_exec(
    functions: &mut BTreeMap<String, FunctionExecTarget>,
    name: QualifiedName,
    arg_types: Vec<DataType>,
) {
    let key = function_exec_cache_key(&name, &arg_types);
    functions
        .entry(key)
        .or_insert(FunctionExecTarget { name, arg_types });
}

fn function_exec_cache_key(name: &QualifiedName, arg_types: &[DataType]) -> String {
    let schema = name
        .schema
        .as_ref()
        .map(|value| value.to_ascii_lowercase())
        .unwrap_or_default();
    let base = name.name.to_ascii_lowercase();
    format!("{schema}.{base}:{arg_types:?}")
}

enum FunctionPlanWork<'a> {
    Physical(&'a PhysicalPlan),
    Logical(&'a LogicalPlan),
    Cypher(&'a aiondb_plan::graph::CypherQueryPlan),
    Expr(&'a TypedExpr),
}

fn collect_functions_from_physical_plan(
    plan: &PhysicalPlan,
    functions: &mut BTreeMap<String, FunctionExecTarget>,
) {
    collect_functions_from_work([FunctionPlanWork::Physical(plan)], functions);
}

fn collect_functions_from_work<'a>(
    initial: impl IntoIterator<Item = FunctionPlanWork<'a>>,
    functions: &mut BTreeMap<String, FunctionExecTarget>,
) {
    let mut stack: Vec<FunctionPlanWork<'a>> = initial.into_iter().collect();
    while let Some(work) = stack.pop() {
        match work {
            FunctionPlanWork::Physical(plan) => {
                push_functions_from_physical_plan(plan, functions, &mut stack);
            }
            FunctionPlanWork::Logical(plan) => {
                push_functions_from_logical_plan(plan, functions, &mut stack);
            }
            FunctionPlanWork::Cypher(query) => {
                push_functions_from_cypher_query(query, functions, &mut stack);
            }
            FunctionPlanWork::Expr(expr) => {
                push_functions_from_expr(expr, functions, &mut stack);
            }
        }
    }
}

fn push_functions_from_physical_plan<'a>(
    plan: &'a PhysicalPlan,
    functions: &mut BTreeMap<String, FunctionExecTarget>,
    stack: &mut Vec<FunctionPlanWork<'a>>,
) {
    match plan {
        PhysicalPlan::ProjectOnce {
            outputs,
            filter,
            order_by,
            limit,
            offset,
            distinct_on,
            ..
        }
        | PhysicalPlan::ProjectTable {
            outputs,
            filter,
            order_by,
            limit,
            offset,
            distinct_on,
            ..
        }
        | PhysicalPlan::ProjectSource {
            outputs,
            filter,
            order_by,
            limit,
            offset,
            distinct_on,
            ..
        }
        | PhysicalPlan::NestedLoopJoin {
            outputs,
            filter,
            order_by,
            limit,
            offset,
            distinct_on,
            ..
        }
        | PhysicalPlan::HashJoin {
            outputs,
            filter,
            order_by,
            limit,
            offset,
            distinct_on,
            ..
        } => {
            if let PhysicalPlan::ProjectSource { source, .. } = plan {
                stack.push(FunctionPlanWork::Physical(source));
            }
            if let PhysicalPlan::NestedLoopJoin {
                left,
                right,
                condition,
                ..
            }
            | PhysicalPlan::HashJoin {
                left,
                right,
                condition,
                ..
            } = plan
            {
                stack.push(FunctionPlanWork::Physical(right));
                stack.push(FunctionPlanWork::Physical(left));
                push_functions_from_optional_expr(condition.as_ref(), stack);
            }
            push_functions_from_projection_exprs(outputs, stack);
            push_functions_from_optional_expr(filter.as_ref(), stack);
            push_functions_from_sort_exprs(order_by, stack);
            push_functions_from_optional_expr(limit.as_ref(), stack);
            push_functions_from_optional_expr(offset.as_ref(), stack);
            push_functions_from_exprs(distinct_on, stack);
        }
        PhysicalPlan::Aggregate {
            group_by,
            grouping_sets: _,
            aggregates,
            having,
            filter,
            order_by,
            limit,
            offset,
            distinct_on,
            ..
        }
        | PhysicalPlan::AggregateSource {
            group_by,
            grouping_sets: _,
            aggregates,
            having,
            filter,
            order_by,
            limit,
            offset,
            distinct_on,
            ..
        } => {
            if let PhysicalPlan::AggregateSource { source, .. } = plan {
                stack.push(FunctionPlanWork::Physical(source));
            }
            push_functions_from_exprs(group_by, stack);
            push_functions_from_projection_exprs(aggregates, stack);
            push_functions_from_optional_expr(having.as_ref(), stack);
            push_functions_from_optional_expr(filter.as_ref(), stack);
            push_functions_from_sort_exprs(order_by, stack);
            push_functions_from_optional_expr(limit.as_ref(), stack);
            push_functions_from_optional_expr(offset.as_ref(), stack);
            push_functions_from_exprs(distinct_on, stack);
        }
        PhysicalPlan::InsertValues {
            rows,
            on_conflict,
            returning,
            ..
        } => {
            for row in rows {
                push_functions_from_exprs(row, stack);
            }
            push_functions_from_on_conflict(on_conflict.as_ref(), stack);
            push_functions_from_projection_exprs(returning, stack);
        }
        PhysicalPlan::InsertSelect {
            assignments,
            source,
            on_conflict,
            returning,
            ..
        } => {
            push_functions_from_exprs(assignments, stack);
            stack.push(FunctionPlanWork::Physical(source));
            push_functions_from_on_conflict(on_conflict.as_ref(), stack);
            push_functions_from_projection_exprs(returning, stack);
        }
        PhysicalPlan::DeleteFromTable {
            filter, returning, ..
        } => {
            push_functions_from_optional_expr(filter.as_ref(), stack);
            push_functions_from_projection_exprs(returning, stack);
        }
        PhysicalPlan::UpdateTable {
            assignments,
            filter,
            returning,
            ..
        } => {
            push_functions_from_assignments(assignments, stack);
            push_functions_from_optional_expr(filter.as_ref(), stack);
            push_functions_from_projection_exprs(returning, stack);
        }
        PhysicalPlan::DistributedScan {
            outputs, filter, ..
        } => {
            push_functions_from_projection_exprs(outputs, stack);
            push_functions_from_optional_expr(filter.as_ref(), stack);
        }
        PhysicalPlan::PartialAggregate {
            source, group_by, ..
        } => {
            stack.push(FunctionPlanWork::Physical(source));
            push_functions_from_exprs(group_by, stack);
        }
        PhysicalPlan::FinalAggregate {
            partials,
            group_by,
            having,
            order_by,
            limit,
            offset,
            ..
        } => {
            for partial in partials {
                stack.push(FunctionPlanWork::Physical(partial));
            }
            push_functions_from_exprs(group_by, stack);
            push_functions_from_optional_expr(having.as_ref(), stack);
            push_functions_from_sort_exprs(order_by, stack);
            push_functions_from_optional_expr(limit.as_ref(), stack);
            push_functions_from_optional_expr(offset.as_ref(), stack);
        }
        PhysicalPlan::BroadcastHashJoin {
            broadcast,
            local,
            left_keys,
            right_keys,
            condition,
            outputs,
            ..
        } => {
            stack.push(FunctionPlanWork::Physical(local));
            stack.push(FunctionPlanWork::Physical(broadcast));
            push_functions_from_exprs(left_keys, stack);
            push_functions_from_exprs(right_keys, stack);
            push_functions_from_optional_expr(condition.as_ref(), stack);
            push_functions_from_projection_exprs(outputs, stack);
        }
        PhysicalPlan::SetOperation {
            left,
            right,
            order_by,
            limit,
            offset,
            ..
        } => {
            stack.push(FunctionPlanWork::Physical(right));
            stack.push(FunctionPlanWork::Physical(left));
            push_functions_from_sort_exprs(order_by, stack);
            push_functions_from_optional_expr(limit.as_ref(), stack);
            push_functions_from_optional_expr(offset.as_ref(), stack);
        }
        PhysicalPlan::ProjectValues {
            rows,
            order_by,
            limit,
            offset,
            ..
        } => {
            for row in rows {
                push_functions_from_exprs(row, stack);
            }
            push_functions_from_sort_exprs(order_by, stack);
            push_functions_from_optional_expr(limit.as_ref(), stack);
            push_functions_from_optional_expr(offset.as_ref(), stack);
        }
        PhysicalPlan::MergeTable(merge) => {
            push_functions_from_merge_plan(merge, functions, stack);
        }
        PhysicalPlan::RecursiveCte {
            base, recursive, ..
        } => {
            stack.push(FunctionPlanWork::Physical(recursive));
            stack.push(FunctionPlanWork::Physical(base));
        }
        PhysicalPlan::HybridFunctionScan {
            function_name,
            args,
            ..
        } => {
            let arg_types = args.iter().map(|arg| arg.data_type.clone()).collect();
            insert_required_function_exec(
                functions,
                QualifiedName::parse(function_name),
                arg_types,
            );
            push_functions_from_exprs(args, stack);
        }
        PhysicalPlan::CreateTableAs { source, .. } => {
            stack.push(FunctionPlanWork::Physical(source));
        }
        PhysicalPlan::CypherQuery(query) => {
            stack.push(FunctionPlanWork::Cypher(query));
        }
        _ => {}
    }
}

fn push_functions_from_cypher_query<'a>(
    query: &'a aiondb_plan::graph::CypherQueryPlan,
    _functions: &mut BTreeMap<String, FunctionExecTarget>,
    stack: &mut Vec<FunctionPlanWork<'a>>,
) {
    for pipeline_op in &query.pipeline {
        match pipeline_op {
            aiondb_plan::graph::CypherPipelineOp::Unwind(unwind_clause) => {
                stack.push(FunctionPlanWork::Expr(&unwind_clause.expr));
            }
            aiondb_plan::graph::CypherPipelineOp::With(with_clause) => {
                push_functions_from_projection_exprs(&with_clause.items, stack);
                push_functions_from_optional_expr(with_clause.filter.as_ref(), stack);
                push_functions_from_sort_exprs(&with_clause.order_by, stack);
                push_functions_from_optional_expr(with_clause.skip.as_ref(), stack);
                push_functions_from_optional_expr(with_clause.limit.as_ref(), stack);
            }
            aiondb_plan::graph::CypherPipelineOp::Match(match_clause) => {
                push_functions_from_cypher_match_clause(match_clause, stack);
            }
            aiondb_plan::graph::CypherPipelineOp::CallSubquery(subquery) => {
                stack.push(FunctionPlanWork::Cypher(subquery));
            }
        }
    }

    for match_clause in &query.matches {
        push_functions_from_cypher_match_clause(match_clause, stack);
    }

    for create_clause in &query.creates {
        for pattern in &create_clause.patterns {
            push_functions_from_cypher_pattern(pattern, stack);
        }
    }

    for merge_clause in &query.merges {
        push_functions_from_cypher_pattern(&merge_clause.pattern, stack);
        for set_item in &merge_clause.on_create_set {
            stack.push(FunctionPlanWork::Expr(&set_item.expr));
        }
        for set_item in &merge_clause.on_match_set {
            stack.push(FunctionPlanWork::Expr(&set_item.expr));
        }
    }

    for set_item in &query.sets {
        stack.push(FunctionPlanWork::Expr(&set_item.expr));
    }

    push_functions_from_projection_exprs(&query.returns, stack);
    push_functions_from_sort_exprs(&query.order_by, stack);
    push_functions_from_optional_expr(query.skip.as_ref(), stack);
    push_functions_from_optional_expr(query.limit.as_ref(), stack);

    if let Some(union) = &query.union {
        stack.push(FunctionPlanWork::Cypher(&union.right));
    }
}

fn push_functions_from_cypher_match_clause<'a>(
    match_clause: &'a aiondb_plan::graph::CypherMatchClause,
    stack: &mut Vec<FunctionPlanWork<'a>>,
) {
    for pattern in &match_clause.patterns {
        push_functions_from_cypher_pattern(pattern, stack);
    }
    push_functions_from_optional_expr(match_clause.filter.as_ref(), stack);
}

fn push_functions_from_cypher_pattern<'a>(
    pattern: &'a aiondb_plan::graph::CypherPattern,
    stack: &mut Vec<FunctionPlanWork<'a>>,
) {
    for node in &pattern.nodes {
        for property in &node.properties {
            stack.push(FunctionPlanWork::Expr(&property.value));
        }
    }
    for rel in &pattern.relationships {
        for property in &rel.properties {
            stack.push(FunctionPlanWork::Expr(&property.value));
        }
    }
}

fn push_functions_from_logical_plan<'a>(
    plan: &'a LogicalPlan,
    functions: &mut BTreeMap<String, FunctionExecTarget>,
    stack: &mut Vec<FunctionPlanWork<'a>>,
) {
    match plan {
        LogicalPlan::ProjectOnce {
            outputs,
            filter,
            order_by,
            limit,
            offset,
            distinct_on,
            ..
        }
        | LogicalPlan::ProjectTable {
            outputs,
            filter,
            order_by,
            limit,
            offset,
            distinct_on,
            ..
        }
        | LogicalPlan::ProjectSource {
            outputs,
            filter,
            order_by,
            limit,
            offset,
            distinct_on,
            ..
        }
        | LogicalPlan::NestedLoopJoin {
            outputs,
            filter,
            order_by,
            limit,
            offset,
            distinct_on,
            ..
        } => {
            if let LogicalPlan::ProjectSource { source, .. } = plan {
                stack.push(FunctionPlanWork::Logical(source));
            }
            if let LogicalPlan::NestedLoopJoin {
                left,
                right,
                condition,
                ..
            } = plan
            {
                stack.push(FunctionPlanWork::Logical(right));
                stack.push(FunctionPlanWork::Logical(left));
                push_functions_from_optional_expr(condition.as_ref(), stack);
            }
            push_functions_from_projection_exprs(outputs, stack);
            push_functions_from_optional_expr(filter.as_ref(), stack);
            push_functions_from_sort_exprs(order_by, stack);
            push_functions_from_optional_expr(limit.as_ref(), stack);
            push_functions_from_optional_expr(offset.as_ref(), stack);
            push_functions_from_exprs(distinct_on, stack);
        }
        LogicalPlan::Aggregate {
            group_by,
            grouping_sets: _,
            aggregates,
            having,
            filter,
            order_by,
            limit,
            offset,
            distinct_on,
            ..
        }
        | LogicalPlan::AggregateSource {
            group_by,
            grouping_sets: _,
            aggregates,
            having,
            filter,
            order_by,
            limit,
            offset,
            distinct_on,
            ..
        } => {
            if let LogicalPlan::AggregateSource { source, .. } = plan {
                stack.push(FunctionPlanWork::Logical(source));
            }
            push_functions_from_exprs(group_by, stack);
            push_functions_from_projection_exprs(aggregates, stack);
            push_functions_from_optional_expr(having.as_ref(), stack);
            push_functions_from_optional_expr(filter.as_ref(), stack);
            push_functions_from_sort_exprs(order_by, stack);
            push_functions_from_optional_expr(limit.as_ref(), stack);
            push_functions_from_optional_expr(offset.as_ref(), stack);
            push_functions_from_exprs(distinct_on, stack);
        }
        LogicalPlan::InsertValues {
            rows,
            on_conflict,
            returning,
            ..
        } => {
            for row in rows {
                push_functions_from_exprs(row, stack);
            }
            push_functions_from_on_conflict(on_conflict.as_ref(), stack);
            push_functions_from_projection_exprs(returning, stack);
        }
        LogicalPlan::InsertSelect {
            assignments,
            source,
            on_conflict,
            returning,
            ..
        } => {
            push_functions_from_exprs(assignments, stack);
            stack.push(FunctionPlanWork::Logical(source));
            push_functions_from_on_conflict(on_conflict.as_ref(), stack);
            push_functions_from_projection_exprs(returning, stack);
        }
        LogicalPlan::DeleteFromTable {
            filter, returning, ..
        } => {
            push_functions_from_optional_expr(filter.as_ref(), stack);
            push_functions_from_projection_exprs(returning, stack);
        }
        LogicalPlan::UpdateTable {
            assignments,
            filter,
            returning,
            ..
        } => {
            push_functions_from_assignments(assignments, stack);
            push_functions_from_optional_expr(filter.as_ref(), stack);
            push_functions_from_projection_exprs(returning, stack);
        }
        LogicalPlan::SetOperation {
            left,
            right,
            order_by,
            limit,
            offset,
            ..
        } => {
            stack.push(FunctionPlanWork::Logical(right));
            stack.push(FunctionPlanWork::Logical(left));
            push_functions_from_sort_exprs(order_by, stack);
            push_functions_from_optional_expr(limit.as_ref(), stack);
            push_functions_from_optional_expr(offset.as_ref(), stack);
        }
        LogicalPlan::ProjectValues {
            rows,
            order_by,
            limit,
            offset,
            ..
        } => {
            for row in rows {
                push_functions_from_exprs(row, stack);
            }
            push_functions_from_sort_exprs(order_by, stack);
            push_functions_from_optional_expr(limit.as_ref(), stack);
            push_functions_from_optional_expr(offset.as_ref(), stack);
        }
        LogicalPlan::MergeTable(merge) => {
            push_functions_from_merge_plan(merge, functions, stack);
        }
        LogicalPlan::RecursiveCte {
            base, recursive, ..
        } => {
            stack.push(FunctionPlanWork::Logical(recursive));
            stack.push(FunctionPlanWork::Logical(base));
        }
        LogicalPlan::HybridFunctionScan {
            function_name,
            args,
            ..
        } => {
            let arg_types = args.iter().map(|arg| arg.data_type.clone()).collect();
            insert_required_function_exec(
                functions,
                QualifiedName::parse(function_name),
                arg_types,
            );
            push_functions_from_exprs(args, stack);
        }
        LogicalPlan::CreateTableAs { source, .. } => {
            stack.push(FunctionPlanWork::Logical(source));
        }
        _ => {}
    }
}

fn push_functions_from_merge_plan<'a>(
    merge: &'a MergePlan,
    _functions: &mut BTreeMap<String, FunctionExecTarget>,
    stack: &mut Vec<FunctionPlanWork<'a>>,
) {
    if let Some(source_subquery_plan) = merge.source_subquery_plan.as_deref() {
        stack.push(FunctionPlanWork::Physical(source_subquery_plan));
    }
    stack.push(FunctionPlanWork::Expr(&merge.on_condition));
    for clause in &merge.when_clauses {
        push_functions_from_optional_expr(clause.condition.as_ref(), stack);
        match &clause.action {
            MergeActionPlan::Update { assignments } => {
                push_functions_from_assignments(assignments, stack);
            }
            MergeActionPlan::Insert { values } => {
                push_functions_from_exprs(values, stack);
            }
            MergeActionPlan::Delete
            | MergeActionPlan::InsertDefaultValues
            | MergeActionPlan::DoNothing => {}
        }
    }
}

fn push_functions_from_on_conflict<'a>(
    on_conflict: Option<&'a InsertOnConflict>,
    stack: &mut Vec<FunctionPlanWork<'a>>,
) {
    let Some(on_conflict) = on_conflict else {
        return;
    };
    match &on_conflict.action {
        OnConflictActionPlan::DoNothing => {}
        OnConflictActionPlan::DoUpdate {
            assignments,
            where_clause,
        } => {
            push_functions_from_assignments(assignments, stack);
            push_functions_from_optional_expr(where_clause.as_ref(), stack);
        }
    }
}

fn push_functions_from_assignments<'a>(
    assignments: &'a [UpdateAssignment],
    stack: &mut Vec<FunctionPlanWork<'a>>,
) {
    stack.extend(
        assignments
            .iter()
            .map(|assignment| FunctionPlanWork::Expr(&assignment.expr)),
    );
}

fn push_functions_from_projection_exprs<'a>(
    exprs: &'a [ProjectionExpr],
    stack: &mut Vec<FunctionPlanWork<'a>>,
) {
    stack.extend(exprs.iter().map(|expr| FunctionPlanWork::Expr(&expr.expr)));
}

fn push_functions_from_sort_exprs<'a>(
    exprs: &'a [SortExpr],
    stack: &mut Vec<FunctionPlanWork<'a>>,
) {
    stack.extend(exprs.iter().map(|expr| FunctionPlanWork::Expr(&expr.expr)));
}

fn push_functions_from_exprs<'a>(exprs: &'a [TypedExpr], stack: &mut Vec<FunctionPlanWork<'a>>) {
    stack.extend(exprs.iter().map(FunctionPlanWork::Expr));
}

fn push_functions_from_optional_expr<'a>(
    expr: Option<&'a TypedExpr>,
    stack: &mut Vec<FunctionPlanWork<'a>>,
) {
    if let Some(expr) = expr {
        stack.push(FunctionPlanWork::Expr(expr));
    }
}

fn push_functions_from_expr<'a>(
    expr: &'a TypedExpr,
    functions: &mut BTreeMap<String, FunctionExecTarget>,
    stack: &mut Vec<FunctionPlanWork<'a>>,
) {
    let mut expr_stack = vec![expr];
    while let Some(expr) = expr_stack.pop() {
        match &expr.kind {
            TypedExprKind::BinaryEq { left, right }
            | TypedExprKind::BinaryNe { left, right }
            | TypedExprKind::BinaryGe { left, right }
            | TypedExprKind::BinaryGt { left, right }
            | TypedExprKind::BinaryLe { left, right }
            | TypedExprKind::BinaryLt { left, right }
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
                expr_stack.push(right);
                expr_stack.push(left);
            }
            TypedExprKind::LogicalNot { expr }
            | TypedExprKind::Negate { expr }
            | TypedExprKind::IsNull { expr, .. }
            | TypedExprKind::Cast { expr, .. } => expr_stack.push(expr),
            TypedExprKind::Like { expr, pattern, .. } => {
                expr_stack.push(pattern);
                expr_stack.push(expr);
            }
            TypedExprKind::InList { expr, list, .. } => {
                expr_stack.extend(list);
                expr_stack.push(expr);
            }
            TypedExprKind::Between {
                expr, low, high, ..
            } => {
                expr_stack.push(high);
                expr_stack.push(low);
                expr_stack.push(expr);
            }
            TypedExprKind::CaseWhen {
                conditions,
                results,
                else_result,
            } => {
                if let Some(else_result) = else_result {
                    expr_stack.push(else_result);
                }
                expr_stack.extend(results);
                expr_stack.extend(conditions);
            }
            TypedExprKind::Coalesce { args } | TypedExprKind::ArrayConstruct { elements: args } => {
                expr_stack.extend(args)
            }
            TypedExprKind::AggCount { expr, filter, .. } => {
                if let Some(expr) = expr {
                    expr_stack.push(expr);
                }
                if let Some(filter) = filter {
                    expr_stack.push(filter);
                }
            }
            TypedExprKind::AggSum { expr, filter, .. }
            | TypedExprKind::AggAvg { expr, filter, .. }
            | TypedExprKind::AggAnyValue { expr, filter }
            | TypedExprKind::AggMin { expr, filter }
            | TypedExprKind::AggMax { expr, filter }
            | TypedExprKind::AggBoolAnd { expr, filter }
            | TypedExprKind::AggBoolOr { expr, filter }
            | TypedExprKind::AggStddevPop { expr, filter }
            | TypedExprKind::AggStddevSamp { expr, filter }
            | TypedExprKind::AggVarPop { expr, filter }
            | TypedExprKind::AggVarSamp { expr, filter } => {
                expr_stack.push(expr);
                if let Some(filter) = filter {
                    expr_stack.push(filter);
                }
            }
            TypedExprKind::AggStringAgg {
                expr,
                delimiter,
                filter,
                ..
            } => {
                expr_stack.push(delimiter);
                expr_stack.push(expr);
                if let Some(filter) = filter {
                    expr_stack.push(filter);
                }
            }
            TypedExprKind::AggArrayAgg { expr, filter, .. } => {
                expr_stack.push(expr);
                if let Some(filter) = filter {
                    expr_stack.push(filter);
                }
            }
            TypedExprKind::ScalarFunction { func, args } => {
                if let ScalarFunction::Generic(name) = func {
                    let arg_types = args.iter().map(|arg| arg.data_type.clone()).collect();
                    insert_required_function_exec(functions, QualifiedName::parse(name), arg_types);
                }
                expr_stack.extend(args);
            }
            TypedExprKind::UserFunction {
                name, args, params, ..
            } => {
                let arg_types = params
                    .iter()
                    .map(|(_, data_type)| data_type.clone())
                    .collect();
                insert_required_function_exec(functions, QualifiedName::parse(name), arg_types);
                expr_stack.extend(args);
            }
            TypedExprKind::ScalarSubquery { plan }
            | TypedExprKind::ArraySubquery { plan }
            | TypedExprKind::ExistsSubquery { plan, .. } => {
                stack.push(FunctionPlanWork::Logical(plan));
            }
            TypedExprKind::InSubquery { expr, plan, .. } => {
                expr_stack.push(expr);
                stack.push(FunctionPlanWork::Logical(plan));
            }
            TypedExprKind::WindowFunction {
                args,
                partition_by,
                order_by,
                ..
            } => {
                expr_stack.extend(args);
                expr_stack.extend(partition_by);
                for sort in order_by {
                    expr_stack.push(&sort.expr);
                }
            }
            TypedExprKind::ColumnRef { .. } | TypedExprKind::OuterColumnRef { .. } => {}
            TypedExprKind::Literal(_) | TypedExprKind::NextValue { .. } => {}
        }
    }
}

fn check_execute_privilege(
    catalog_reader: &dyn CatalogReader,
    identity: &AuthenticatedIdentity,
    function_target: &FunctionExecTarget,
) -> DbResult<()> {
    // Internal compatibility helper functions are part of the engine surface
    // and should not require explicit EXECUTE grants.
    if function_target
        .name
        .name
        .to_ascii_lowercase()
        .starts_with("__aiondb_")
    {
        return Ok(());
    }

    let txn = TxnId::default();
    let mut any_role_exists = false;
    let mut inherited_roles = BTreeSet::new();

    let mut roles_to_check: Vec<&str> = identity.roles.iter().map(String::as_str).collect();
    if !roles_to_check
        .iter()
        .any(|role| role.eq_ignore_ascii_case("public"))
    {
        roles_to_check.push("public");
    }

    for role_name in roles_to_check {
        if let Some(role_desc) = catalog_reader.get_role(txn, role_name)? {
            any_role_exists = true;
            if role_desc.superuser {
                return Ok(());
            }
        }
        inherited_roles.insert(role_name.to_owned());
        for privilege in catalog_reader.get_privileges(txn, role_name)? {
            if function_target_matches(&privilege.target, function_target) {
                match privilege.privilege {
                    CatalogPrivilege::All => return Ok(()),
                    CatalogPrivilege::Execute => return Ok(()),
                    _ => {}
                }
            }
            if let PrivilegeTarget::Role(member_of) = privilege.target {
                inherited_roles.insert(member_of);
            }
        }
    }

    if !any_role_exists {
        if catalog_has_any_roles(catalog_reader)? {
            return Err(DbError::insufficient_privilege(
                "no valid roles found for user",
            ));
        }
        if function_requires_explicit_execute_grant(&function_target.name) {
            return Err(DbError::insufficient_privilege(format!(
                "permission denied: EXECUTE on function {}",
                function_target.name
            )));
        }
        return Ok(());
    }

    if role_has_builtin_execute(&inherited_roles, &function_target.name) {
        return Ok(());
    }

    if let Some(owner) = matching_user_function_owner(catalog_reader, function_target)? {
        if identity
            .roles
            .iter()
            .any(|role| role.eq_ignore_ascii_case(&owner))
        {
            return Ok(());
        }
        return Err(DbError::insufficient_privilege(format!(
            "permission denied: EXECUTE on function {}",
            function_target.name
        )));
    }

    if !function_requires_explicit_execute_grant(&function_target.name)
        && !function_requires_execute_grant_when_roles_active(&function_target.name)
    {
        return Ok(());
    }

    Err(DbError::insufficient_privilege(format!(
        "permission denied: EXECUTE on function {}",
        function_target.name
    )))
}

fn matching_user_function_owner(
    catalog_reader: &dyn CatalogReader,
    function_target: &FunctionExecTarget,
) -> DbResult<Option<String>> {
    for function in catalog_reader.list_functions(TxnId::default())? {
        let function_name = QualifiedName::parse(&function.name);
        let name_matches = function_name
            .object_name()
            .eq_ignore_ascii_case(function_target.name.object_name())
            && match (
                function_name.schema_name(),
                function_target.name.schema_name(),
            ) {
                (Some(left), Some(right)) => left.eq_ignore_ascii_case(right),
                (None, _) | (_, None) => true,
            };
        if !name_matches {
            continue;
        }
        if function.params.len() != function_target.arg_types.len() {
            continue;
        }
        if function
            .params
            .iter()
            .zip(&function_target.arg_types)
            .all(|(param, arg_type)| param.data_type == *arg_type)
        {
            return Ok(function.owner.clone());
        }
    }
    Ok(None)
}

fn function_target_matches(target: &PrivilegeTarget, function_target: &FunctionExecTarget) -> bool {
    match target {
        PrivilegeTarget::Function(FunctionPrivilegeTarget { name, arg_types }) => {
            name.name.eq_ignore_ascii_case(&function_target.name.name)
                && match (&name.schema, &function_target.name.schema) {
                    (Some(left), Some(right)) => left.eq_ignore_ascii_case(right),
                    (None, _) | (_, None) => true,
                }
                && function_signature_matches(
                    &function_target.name,
                    arg_types.as_deref(),
                    &function_target.arg_types,
                )
        }
        _ => false,
    }
}

fn function_signature_matches(
    function_name: &QualifiedName,
    granted_signature: Option<&[DataType]>,
    call_signature: &[DataType],
) -> bool {
    match granted_signature {
        None => true,
        Some(granted) if granted == call_signature => true,
        Some(granted)
            if function_name.name.eq_ignore_ascii_case("vector_top_k_ids")
                || function_name.name.eq_ignore_ascii_case("vector_top_k_hits") =>
        {
            vector_top_k_hybrid_signature_compatible(granted, call_signature)
        }
        Some(granted)
            if function_name
                .name
                .eq_ignore_ascii_case("vector_recommend_top_k_hits") =>
        {
            vector_recommend_top_k_hybrid_signature_compatible(granted, call_signature)
        }
        Some(granted)
            if function_name
                .name
                .eq_ignore_ascii_case("vector_prefetch_top_k_hits") =>
        {
            vector_prefetch_top_k_hybrid_signature_compatible(granted, call_signature)
        }
        Some(granted)
            if function_name
                .name
                .eq_ignore_ascii_case("full_text_top_k_hits") =>
        {
            full_text_top_k_hybrid_signature_compatible(granted, call_signature)
        }
        Some(granted)
            if function_name
                .name
                .eq_ignore_ascii_case("hybrid_search_top_k_hits") =>
        {
            hybrid_search_top_k_hybrid_signature_compatible(granted, call_signature)
        }
        Some(_) => false,
    }
}

fn vector_top_k_hybrid_signature_compatible(granted: &[DataType], call: &[DataType]) -> bool {
    // Keep existing grants on the historic 4-arg signature working for
    // optional-argument invocations:
    // vector_top_k_ids(text,text,text,integer[, metric[, ef[, distance_threshold[, exact[, score_threshold]]]]])
    let granted_base = [
        DataType::Text,
        DataType::Text,
        DataType::Text,
        DataType::Int,
    ];
    if granted != granted_base {
        return false;
    }
    if !(4..=10).contains(&call.len()) {
        return false;
    }
    if call.first() != Some(&DataType::Text)
        || call.get(1) != Some(&DataType::Text)
        || call.get(2) != Some(&DataType::Text)
        || !matches!(call.get(3), Some(DataType::Int | DataType::BigInt))
    {
        return false;
    }
    if call.get(4).is_some_and(|ty| *ty != DataType::Text) {
        return false;
    }
    if call
        .get(5)
        .is_some_and(|ty| !matches!(ty, DataType::Int | DataType::BigInt))
    {
        return false;
    }
    if call.get(6).is_some_and(|ty| !is_numeric_type(ty)) {
        return false;
    }
    if call.get(7).is_some_and(|ty| *ty != DataType::Boolean) {
        return false;
    }
    if call.get(8).is_some_and(|ty| !is_numeric_type(ty)) {
        return false;
    }
    if call
        .get(9)
        .is_some_and(|ty| !matches!(ty, DataType::Jsonb | DataType::Text))
    {
        return false;
    }
    true
}

fn vector_recommend_top_k_hybrid_signature_compatible(
    granted: &[DataType],
    call: &[DataType],
) -> bool {
    // Keep grants on the 5-arg signature working for optional-argument calls:
    // vector_recommend_top_k_hits(text,text,examples,examples,integer
    //   [, metric[, ef[, distance_threshold[, exact[, score_threshold[, options]]]]]])
    // where examples can be text/jsonb/int/bigint/vector/array.
    let granted_base = [
        DataType::Text,
        DataType::Text,
        DataType::Text,
        DataType::Text,
        DataType::Int,
    ];
    if granted != granted_base {
        return false;
    }
    if !(5..=11).contains(&call.len()) {
        return false;
    }
    if call.first() != Some(&DataType::Text)
        || call.get(1) != Some(&DataType::Text)
        || call
            .get(2)
            .is_some_and(|ty| !is_vector_recommend_examples_type(ty))
        || call
            .get(3)
            .is_some_and(|ty| !is_vector_recommend_examples_type(ty))
        || !matches!(call.get(4), Some(DataType::Int | DataType::BigInt))
    {
        return false;
    }
    if call.get(5).is_some_and(|ty| *ty != DataType::Text) {
        return false;
    }
    if call
        .get(6)
        .is_some_and(|ty| !matches!(ty, DataType::Int | DataType::BigInt))
    {
        return false;
    }
    if call.get(7).is_some_and(|ty| !is_numeric_type(ty)) {
        return false;
    }
    if call.get(8).is_some_and(|ty| *ty != DataType::Boolean) {
        return false;
    }
    if call.get(9).is_some_and(|ty| !is_numeric_type(ty)) {
        return false;
    }
    if call
        .get(10)
        .is_some_and(|ty| !matches!(ty, DataType::Jsonb | DataType::Text))
    {
        return false;
    }
    true
}

fn is_vector_recommend_examples_type(data_type: &DataType) -> bool {
    matches!(
        data_type,
        DataType::Text
            | DataType::Jsonb
            | DataType::Int
            | DataType::BigInt
            | DataType::Vector { .. }
            | DataType::Array(_)
    )
}

fn vector_prefetch_top_k_hybrid_signature_compatible(
    granted: &[DataType],
    call: &[DataType],
) -> bool {
    // Keep grants on the 5-arg signature working for optional-argument calls:
    // vector_prefetch_top_k_hits(text,text,text,text,integer[, metric[, distance_threshold[, score_threshold[, options]]]])
    let granted_base = [
        DataType::Text,
        DataType::Text,
        DataType::Text,
        DataType::Text,
        DataType::Int,
    ];
    if granted != granted_base {
        return false;
    }
    if !(5..=9).contains(&call.len()) {
        return false;
    }
    if call.first() != Some(&DataType::Text)
        || call.get(1) != Some(&DataType::Text)
        || call.get(2) != Some(&DataType::Text)
        || !matches!(
            call.get(3),
            Some(DataType::Text | DataType::Jsonb | DataType::Array(_))
        )
        || !matches!(call.get(4), Some(DataType::Int | DataType::BigInt))
    {
        return false;
    }
    if call.get(5).is_some_and(|ty| *ty != DataType::Text) {
        return false;
    }
    if call.get(6).is_some_and(|ty| !is_numeric_type(ty)) {
        return false;
    }
    if call.get(7).is_some_and(|ty| !is_numeric_type(ty)) {
        return false;
    }
    if call
        .get(8)
        .is_some_and(|ty| !matches!(ty, DataType::Jsonb | DataType::Text))
    {
        return false;
    }
    true
}

fn full_text_top_k_hybrid_signature_compatible(granted: &[DataType], call: &[DataType]) -> bool {
    // Keep grants on the 4-arg signature working for optional-argument calls:
    // full_text_top_k_hits(text,text,text,integer
    //   [, query_mode[, config[, score_threshold[, options]]]])
    let granted_base = [
        DataType::Text,
        DataType::Text,
        DataType::Text,
        DataType::Int,
    ];
    if granted != granted_base {
        return false;
    }
    if !(4..=8).contains(&call.len()) {
        return false;
    }
    if call.first() != Some(&DataType::Text)
        || call.get(1) != Some(&DataType::Text)
        || call.get(2) != Some(&DataType::Text)
        || !matches!(call.get(3), Some(DataType::Int | DataType::BigInt))
    {
        return false;
    }
    if call.get(4).is_some_and(|ty| *ty != DataType::Text) {
        return false;
    }
    if call.get(5).is_some_and(|ty| *ty != DataType::Text) {
        return false;
    }
    if call.get(6).is_some_and(|ty| !is_numeric_type(ty)) {
        return false;
    }
    if call
        .get(7)
        .is_some_and(|ty| !matches!(ty, DataType::Jsonb | DataType::Text))
    {
        return false;
    }
    true
}

fn hybrid_search_top_k_hybrid_signature_compatible(
    granted: &[DataType],
    call: &[DataType],
) -> bool {
    // Keep grants on the 6-arg signature working for optional options calls:
    // hybrid_search_top_k_hits(text,text,text,text,text,integer[, options])
    let granted_base = [
        DataType::Text,
        DataType::Text,
        DataType::Text,
        DataType::Text,
        DataType::Text,
        DataType::Int,
    ];
    if granted != granted_base {
        return false;
    }
    if !(6..=7).contains(&call.len()) {
        return false;
    }
    if call.first() != Some(&DataType::Text)
        || call.get(1) != Some(&DataType::Text)
        || call.get(2) != Some(&DataType::Text)
        || !matches!(
            call.get(3),
            Some(DataType::Text | DataType::Vector { .. } | DataType::Jsonb | DataType::Array(_))
        )
        || call.get(4) != Some(&DataType::Text)
        || !matches!(call.get(5), Some(DataType::Int | DataType::BigInt))
    {
        return false;
    }
    if call
        .get(6)
        .is_some_and(|ty| !matches!(ty, DataType::Jsonb | DataType::Text))
    {
        return false;
    }
    true
}

fn is_numeric_type(data_type: &DataType) -> bool {
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

fn role_has_builtin_execute(role_names: &BTreeSet<String>, function_name: &QualifiedName) -> bool {
    role_names
        .iter()
        .any(|role| role.eq_ignore_ascii_case("pg_monitor"))
        && PG_MONITOR_FS_HELPERS
            .iter()
            .any(|builtin| function_name.name.eq_ignore_ascii_case(builtin))
}

fn function_requires_explicit_execute_grant(function_name: &QualifiedName) -> bool {
    let normalized_name = function_name.name.to_ascii_lowercase();
    matches!(
        normalized_name.as_str(),
        "pg_read_file"
            | "pg_read_binary_file"
            | "pg_ls_dir"
            | "pg_ls_waldir"
            | "pg_ls_logdir"
            | "pg_ls_archive_statusdir"
            | "pg_ls_tmpdir"
    ) || PG_MONITOR_FS_HELPERS
        .iter()
        .any(|builtin| normalized_name == *builtin)
}

fn function_requires_execute_grant_when_roles_active(function_name: &QualifiedName) -> bool {
    let normalized_name = function_name.name.to_ascii_lowercase();
    matches!(
        normalized_name.as_str(),
        "vector_top_k_ids"
            | "vector_top_k_hits"
            | "vector_recommend_top_k_hits"
            | "vector_prefetch_top_k_hits"
            | "full_text_top_k_hits"
            | "hybrid_search_top_k_hits"
    )
}

const PG_MONITOR_FS_HELPERS: [&str; 3] = [
    "pg_ls_logicalsnapdir",
    "pg_ls_logicalmapdir",
    "pg_ls_replslotdir",
];

#[cfg(test)]
mod tests {
    use super::*;
    use aiondb_core::RelationId;

    #[test]
    fn matches_privilege_all_covers_select() {
        assert!(matches_privilege(
            &CatalogPrivilege::All,
            CatalogPrivilege::Select
        ));
    }

    #[test]
    fn matches_privilege_exact_match() {
        assert!(matches_privilege(
            &CatalogPrivilege::Insert,
            CatalogPrivilege::Insert
        ));
    }

    #[test]
    fn matches_privilege_mismatch() {
        assert!(!matches_privilege(
            &CatalogPrivilege::Select,
            CatalogPrivilege::Insert
        ));
    }

    #[test]
    fn matches_target_same_table() {
        let target = PrivilegeTarget::Table(QualifiedName::unqualified("users"));
        let name = QualifiedName::unqualified("users");
        assert!(matches_target(&target, &name));
    }

    #[test]
    fn matches_target_case_insensitive() {
        let target = PrivilegeTarget::Table(QualifiedName::unqualified("Users"));
        let name = QualifiedName::unqualified("users");
        assert!(matches_target(&target, &name));
    }

    #[test]
    fn matches_target_schema_target_not_table() {
        let target = PrivilegeTarget::Schema("public".to_string());
        let name = QualifiedName::unqualified("users");
        assert!(!matches_target(&target, &name));
    }

    #[test]
    fn required_privileges_select() {
        let plan = PhysicalPlan::ProjectTable {
            table_id: RelationId::new(1),
            outputs: vec![],
            filter: None,
            order_by: vec![],
            limit: None,
            offset: None,
            distinct: false,
            distinct_on: vec![],
            access_path: aiondb_plan::ScanAccessPath::SeqScan,
        };
        let reqs = required_privileges(&plan);
        assert_eq!(reqs.len(), 1);
        assert_eq!(reqs[0].0, CatalogPrivilege::Select);
    }

    #[test]
    fn required_privileges_create_table_empty() {
        let plan = PhysicalPlan::CreateTable {
            relation_name: "t".to_string(),
            columns: vec![],
            defaults: vec![],
            identities: vec![],
            typed_table_of: None,
            primary_key_columns: vec![],
            unique_constraints: vec![],
            foreign_keys: vec![],
            check_constraints: vec![],
            shard_key_columns: vec![],
            shard_count: None,
        };
        assert!(required_privileges(&plan).is_empty());
    }

    #[test]
    fn required_function_execs_collects_generic_function_calls() {
        let plan = PhysicalPlan::ProjectOnce {
            outputs: vec![ProjectionExpr {
                field: aiondb_plan::ResultField {
                    name: "value".to_owned(),
                    data_type: aiondb_core::DataType::Text,
                    text_type_modifier: None,
                    nullable: false,
                },
                expr: TypedExpr::scalar_function(
                    ScalarFunction::Generic("pg_read_file".to_owned()),
                    vec![TypedExpr::literal(
                        aiondb_core::Value::Text("postmaster.pid".to_owned()),
                        aiondb_core::DataType::Text,
                        false,
                    )],
                    aiondb_core::DataType::Text,
                    false,
                ),
            }],
            filter: None,
            order_by: vec![],
            limit: None,
            offset: None,
            distinct: false,
            distinct_on: vec![],
        };

        assert_eq!(
            required_function_execs(&plan),
            vec![FunctionExecTarget {
                name: QualifiedName::unqualified("pg_read_file"),
                arg_types: vec![aiondb_core::DataType::Text],
            }]
        );
    }

    #[test]
    fn required_function_execs_collects_cypher_scalar_function_calls() {
        let plan = PhysicalPlan::CypherQuery(Box::new(aiondb_plan::graph::CypherQueryPlan {
            pipeline: Vec::new(),
            matches: Vec::new(),
            creates: Vec::new(),
            merges: Vec::new(),
            sets: Vec::new(),
            deletes: Vec::new(),
            returns: vec![ProjectionExpr {
                field: aiondb_plan::ResultField {
                    name: "value".to_owned(),
                    data_type: aiondb_core::DataType::Text,
                    text_type_modifier: None,
                    nullable: false,
                },
                expr: TypedExpr::scalar_function(
                    ScalarFunction::Generic("pg_read_file".to_owned()),
                    vec![TypedExpr::literal(
                        aiondb_core::Value::Text("postmaster.pid".to_owned()),
                        aiondb_core::DataType::Text,
                        false,
                    )],
                    aiondb_core::DataType::Text,
                    false,
                ),
            }],
            order_by: Vec::new(),
            skip: None,
            limit: None,
            distinct: false,
            union: None,
        }));

        assert_eq!(
            required_function_execs(&plan),
            vec![FunctionExecTarget {
                name: QualifiedName::unqualified("pg_read_file"),
                arg_types: vec![aiondb_core::DataType::Text],
            }]
        );
    }

    #[test]
    fn function_requires_explicit_execute_grant_for_fs_helpers() {
        assert!(function_requires_explicit_execute_grant(
            &QualifiedName::unqualified("pg_read_file")
        ));
        assert!(function_requires_explicit_execute_grant(
            &QualifiedName::unqualified("pg_ls_dir")
        ));
        assert!(function_requires_explicit_execute_grant(
            &QualifiedName::unqualified("pg_ls_logicalsnapdir")
        ));
        assert!(function_requires_explicit_execute_grant(
            &QualifiedName::unqualified("pg_ls_logicalmapdir")
        ));
        assert!(function_requires_explicit_execute_grant(
            &QualifiedName::unqualified("pg_ls_replslotdir")
        ));
    }

    #[test]
    fn function_requires_explicit_execute_grant_is_false_for_safe_helpers() {
        assert!(!function_requires_explicit_execute_grant(
            &QualifiedName::unqualified("now")
        ));
    }

    #[test]
    fn hybrid_function_name_matches_accepts_schema_qualified_names() {
        assert!(hybrid_function_name_matches(
            "public.vector_top_k_ids",
            "vector_top_k_ids"
        ));
        assert!(hybrid_function_name_matches(
            "pg_catalog.graph_neighbors",
            "graph_neighbors"
        ));
    }

    #[test]
    fn vector_top_k_ids_optional_signature_matches_legacy_grant() {
        let granted = vec![
            DataType::Text,
            DataType::Text,
            DataType::Text,
            DataType::Int,
        ];
        let call = vec![
            DataType::Text,
            DataType::Text,
            DataType::Text,
            DataType::Int,
            DataType::Text,
            DataType::BigInt,
            DataType::Double,
            DataType::Boolean,
            DataType::Numeric,
            DataType::Jsonb,
        ];
        assert!(function_signature_matches(
            &QualifiedName::unqualified("vector_top_k_ids"),
            Some(&granted),
            &call
        ));
    }

    #[test]
    fn vector_top_k_ids_optional_signature_rejects_wrong_optional_type() {
        let granted = vec![
            DataType::Text,
            DataType::Text,
            DataType::Text,
            DataType::Int,
        ];
        let call = vec![
            DataType::Text,
            DataType::Text,
            DataType::Text,
            DataType::Int,
            DataType::Text,
            DataType::Int,
            DataType::Double,
            DataType::Text,
        ];
        assert!(!function_signature_matches(
            &QualifiedName::unqualified("vector_top_k_ids"),
            Some(&granted),
            &call
        ));
    }

    #[test]
    fn vector_top_k_ids_optional_signature_rejects_wrong_options_type() {
        let granted = vec![
            DataType::Text,
            DataType::Text,
            DataType::Text,
            DataType::Int,
        ];
        let call = vec![
            DataType::Text,
            DataType::Text,
            DataType::Text,
            DataType::Int,
            DataType::Text,
            DataType::Int,
            DataType::Double,
            DataType::Boolean,
            DataType::Numeric,
            DataType::Boolean,
        ];
        assert!(!function_signature_matches(
            &QualifiedName::unqualified("vector_top_k_ids"),
            Some(&granted),
            &call
        ));
    }

    #[test]
    fn vector_top_k_hits_optional_signature_matches_legacy_grant() {
        let granted = vec![
            DataType::Text,
            DataType::Text,
            DataType::Text,
            DataType::Int,
        ];
        let call = vec![
            DataType::Text,
            DataType::Text,
            DataType::Text,
            DataType::Int,
            DataType::Text,
            DataType::BigInt,
            DataType::Double,
            DataType::Boolean,
            DataType::Numeric,
            DataType::Jsonb,
        ];
        assert!(function_signature_matches(
            &QualifiedName::unqualified("vector_top_k_hits"),
            Some(&granted),
            &call
        ));
    }

    #[test]
    fn vector_recommend_top_k_hits_optional_signature_matches_legacy_grant() {
        let granted = vec![
            DataType::Text,
            DataType::Text,
            DataType::Text,
            DataType::Text,
            DataType::Int,
        ];
        let call = vec![
            DataType::Text,
            DataType::Text,
            DataType::Text,
            DataType::Text,
            DataType::Int,
            DataType::Text,
            DataType::BigInt,
            DataType::Double,
            DataType::Boolean,
            DataType::Numeric,
            DataType::Jsonb,
        ];
        assert!(function_signature_matches(
            &QualifiedName::unqualified("vector_recommend_top_k_hits"),
            Some(&granted),
            &call
        ));
    }

    #[test]
    fn vector_recommend_top_k_hits_optional_signature_accepts_json_examples() {
        let granted = vec![
            DataType::Text,
            DataType::Text,
            DataType::Text,
            DataType::Text,
            DataType::Int,
        ];
        let call = vec![
            DataType::Text,
            DataType::Text,
            DataType::Jsonb,
            DataType::Array(Box::new(DataType::BigInt)),
            DataType::Int,
            DataType::Text,
            DataType::BigInt,
            DataType::Double,
            DataType::Boolean,
            DataType::Numeric,
            DataType::Jsonb,
        ];
        assert!(function_signature_matches(
            &QualifiedName::unqualified("vector_recommend_top_k_hits"),
            Some(&granted),
            &call
        ));
    }

    #[test]
    fn vector_prefetch_top_k_hits_optional_signature_matches_legacy_grant() {
        let granted = vec![
            DataType::Text,
            DataType::Text,
            DataType::Text,
            DataType::Text,
            DataType::Int,
        ];
        let call = vec![
            DataType::Text,
            DataType::Text,
            DataType::Text,
            DataType::Jsonb,
            DataType::Int,
            DataType::Text,
            DataType::Double,
            DataType::Numeric,
            DataType::Jsonb,
        ];
        assert!(function_signature_matches(
            &QualifiedName::unqualified("vector_prefetch_top_k_hits"),
            Some(&granted),
            &call
        ));
    }

    #[test]
    fn full_text_top_k_hits_optional_signature_matches_legacy_grant() {
        let granted = vec![
            DataType::Text,
            DataType::Text,
            DataType::Text,
            DataType::Int,
        ];
        let call = vec![
            DataType::Text,
            DataType::Text,
            DataType::Text,
            DataType::Int,
            DataType::Text,
            DataType::Text,
            DataType::Double,
            DataType::Jsonb,
        ];
        assert!(function_signature_matches(
            &QualifiedName::unqualified("full_text_top_k_hits"),
            Some(&granted),
            &call
        ));
    }

    #[test]
    fn hybrid_search_top_k_hits_optional_signature_matches_legacy_grant() {
        let granted = vec![
            DataType::Text,
            DataType::Text,
            DataType::Text,
            DataType::Text,
            DataType::Text,
            DataType::Int,
        ];
        let call = vec![
            DataType::Text,
            DataType::Text,
            DataType::Text,
            DataType::Vector {
                dims: 2,
                element_type: aiondb_core::VectorElementType::Float32,
            },
            DataType::Text,
            DataType::BigInt,
            DataType::Jsonb,
        ];
        assert!(function_signature_matches(
            &QualifiedName::unqualified("hybrid_search_top_k_hits"),
            Some(&granted),
            &call
        ));
    }
}
