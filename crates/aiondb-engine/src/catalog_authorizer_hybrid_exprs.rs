fn collect_required_hybrid_privileges_from_cypher_query(
    catalog_reader: &dyn CatalogReader,
    query: &aiondb_plan::graph::CypherQueryPlan,
    reqs: &mut Vec<(CatalogPrivilege, RelationId)>,
) -> DbResult<()> {
    let _guard = enter_auth_depth(
        &AUTH_CYPHER_QUERY_DEPTH,
        MAX_AUTH_CYPHER_QUERY_DEPTH,
        "hybrid cypher query",
    )?;
    for pipeline_op in &query.pipeline {
        match pipeline_op {
            aiondb_plan::graph::CypherPipelineOp::Unwind(unwind_clause) => {
                collect_required_hybrid_privileges_from_expr(
                    catalog_reader,
                    &unwind_clause.expr,
                    reqs,
                )?;
            }
            aiondb_plan::graph::CypherPipelineOp::With(with_clause) => {
                collect_required_hybrid_privileges_from_projection_exprs(
                    catalog_reader,
                    &with_clause.items,
                    reqs,
                )?;
                collect_required_hybrid_privileges_from_optional_expr(
                    catalog_reader,
                    with_clause.filter.as_ref(),
                    reqs,
                )?;
                collect_required_hybrid_privileges_from_sort_exprs(
                    catalog_reader,
                    &with_clause.order_by,
                    reqs,
                )?;
                collect_required_hybrid_privileges_from_optional_expr(
                    catalog_reader,
                    with_clause.skip.as_ref(),
                    reqs,
                )?;
                collect_required_hybrid_privileges_from_optional_expr(
                    catalog_reader,
                    with_clause.limit.as_ref(),
                    reqs,
                )?;
            }
            aiondb_plan::graph::CypherPipelineOp::Match(match_clause) => {
                collect_required_hybrid_privileges_from_cypher_match_clause(
                    catalog_reader,
                    match_clause,
                    reqs,
                )?;
            }
            aiondb_plan::graph::CypherPipelineOp::CallSubquery(subquery) => {
                collect_required_hybrid_privileges_from_cypher_query(
                    catalog_reader,
                    subquery,
                    reqs,
                )?;
            }
            aiondb_plan::graph::CypherPipelineOp::Foreach(foreach) => {
                collect_required_hybrid_privileges_from_cypher_foreach(
                    catalog_reader,
                    foreach,
                    reqs,
                )?;
            }
            aiondb_plan::graph::CypherPipelineOp::ProcedureCall(_) => {}
        }
    }

    for match_clause in &query.matches {
        collect_required_hybrid_privileges_from_cypher_match_clause(
            catalog_reader,
            match_clause,
            reqs,
        )?;
    }

    for create_clause in &query.creates {
        for pattern in &create_clause.patterns {
            collect_required_hybrid_privileges_from_cypher_pattern(catalog_reader, pattern, reqs)?;
        }
    }

    for merge_clause in &query.merges {
        collect_required_hybrid_privileges_from_cypher_pattern(
            catalog_reader,
            &merge_clause.pattern,
            reqs,
        )?;
        for set_item in &merge_clause.on_create_set {
            collect_required_hybrid_privileges_from_expr(catalog_reader, &set_item.expr, reqs)?;
        }
        for set_item in &merge_clause.on_match_set {
            collect_required_hybrid_privileges_from_expr(catalog_reader, &set_item.expr, reqs)?;
        }
    }

    for set_item in &query.sets {
        collect_required_hybrid_privileges_from_expr(catalog_reader, &set_item.expr, reqs)?;
    }

    collect_required_hybrid_privileges_from_projection_exprs(catalog_reader, &query.returns, reqs)?;
    collect_required_hybrid_privileges_from_sort_exprs(catalog_reader, &query.order_by, reqs)?;
    collect_required_hybrid_privileges_from_optional_expr(
        catalog_reader,
        query.skip.as_ref(),
        reqs,
    )?;
    collect_required_hybrid_privileges_from_optional_expr(
        catalog_reader,
        query.limit.as_ref(),
        reqs,
    )?;

    if let Some(union) = &query.union {
        collect_required_hybrid_privileges_from_cypher_query(catalog_reader, &union.right, reqs)?;
    }

    Ok(())
}

fn collect_required_hybrid_privileges_from_cypher_foreach(
    catalog_reader: &dyn CatalogReader,
    foreach: &aiondb_plan::graph::CypherForeachPlan,
    reqs: &mut Vec<(CatalogPrivilege, RelationId)>,
) -> DbResult<()> {
    collect_required_hybrid_privileges_from_expr(catalog_reader, &foreach.expr, reqs)?;
    for op in &foreach.body {
        match op {
            aiondb_plan::graph::CypherForeachOp::Set(set_item) => {
                collect_required_hybrid_privileges_from_expr(
                    catalog_reader,
                    &set_item.expr,
                    reqs,
                )?;
            }
            aiondb_plan::graph::CypherForeachOp::Create(create_clause) => {
                for pattern in &create_clause.patterns {
                    collect_required_hybrid_privileges_from_cypher_pattern(
                        catalog_reader,
                        pattern,
                        reqs,
                    )?;
                }
            }
            aiondb_plan::graph::CypherForeachOp::Merge(merge_clause) => {
                collect_required_hybrid_privileges_from_cypher_pattern(
                    catalog_reader,
                    &merge_clause.pattern,
                    reqs,
                )?;
                for set_item in merge_clause
                    .on_create_set
                    .iter()
                    .chain(merge_clause.on_match_set.iter())
                {
                    collect_required_hybrid_privileges_from_expr(
                        catalog_reader,
                        &set_item.expr,
                        reqs,
                    )?;
                }
            }
            aiondb_plan::graph::CypherForeachOp::Delete(_) => {}
            aiondb_plan::graph::CypherForeachOp::Foreach(nested) => {
                collect_required_hybrid_privileges_from_cypher_foreach(
                    catalog_reader,
                    nested,
                    reqs,
                )?;
            }
        }
    }
    Ok(())
}

fn collect_required_hybrid_privileges_from_cypher_match_clause(
    catalog_reader: &dyn CatalogReader,
    match_clause: &aiondb_plan::graph::CypherMatchClause,
    reqs: &mut Vec<(CatalogPrivilege, RelationId)>,
) -> DbResult<()> {
    for pattern in &match_clause.patterns {
        collect_required_hybrid_privileges_from_cypher_pattern(catalog_reader, pattern, reqs)?;
    }
    collect_required_hybrid_privileges_from_optional_expr(
        catalog_reader,
        match_clause.filter.as_ref(),
        reqs,
    )
}

fn collect_required_hybrid_privileges_from_cypher_pattern(
    catalog_reader: &dyn CatalogReader,
    pattern: &aiondb_plan::graph::CypherPattern,
    reqs: &mut Vec<(CatalogPrivilege, RelationId)>,
) -> DbResult<()> {
    for node in &pattern.nodes {
        for property in &node.properties {
            collect_required_hybrid_privileges_from_expr(catalog_reader, &property.value, reqs)?;
        }
    }
    for rel in &pattern.relationships {
        for property in &rel.properties {
            collect_required_hybrid_privileges_from_expr(catalog_reader, &property.value, reqs)?;
        }
    }
    Ok(())
}

fn collect_required_hybrid_privileges_from_logical_plan(
    catalog_reader: &dyn CatalogReader,
    plan: &LogicalPlan,
    reqs: &mut Vec<(CatalogPrivilege, RelationId)>,
) -> DbResult<()> {
    let _guard = enter_auth_depth(
        &AUTH_HYBRID_PLAN_DEPTH,
        MAX_AUTH_HYBRID_PLAN_DEPTH,
        "hybrid logical plan",
    )?;
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
                collect_required_hybrid_privileges_from_logical_plan(catalog_reader, source, reqs)?;
            }
            if let LogicalPlan::NestedLoopJoin {
                left,
                right,
                condition,
                ..
            } = plan
            {
                collect_required_hybrid_privileges_from_logical_plan(catalog_reader, left, reqs)?;
                collect_required_hybrid_privileges_from_logical_plan(catalog_reader, right, reqs)?;
                collect_required_hybrid_privileges_from_optional_expr(
                    catalog_reader,
                    condition.as_ref(),
                    reqs,
                )?;
            }
            collect_required_hybrid_privileges_from_projection_exprs(
                catalog_reader,
                outputs,
                reqs,
            )?;
            collect_required_hybrid_privileges_from_optional_expr(
                catalog_reader,
                filter.as_ref(),
                reqs,
            )?;
            collect_required_hybrid_privileges_from_sort_exprs(catalog_reader, order_by, reqs)?;
            collect_required_hybrid_privileges_from_optional_expr(
                catalog_reader,
                limit.as_ref(),
                reqs,
            )?;
            collect_required_hybrid_privileges_from_optional_expr(
                catalog_reader,
                offset.as_ref(),
                reqs,
            )?;
            collect_required_hybrid_privileges_from_exprs(catalog_reader, distinct_on, reqs)?;
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
                collect_required_hybrid_privileges_from_logical_plan(catalog_reader, source, reqs)?;
            }
            collect_required_hybrid_privileges_from_exprs(catalog_reader, group_by, reqs)?;
            collect_required_hybrid_privileges_from_projection_exprs(
                catalog_reader,
                aggregates,
                reqs,
            )?;
            collect_required_hybrid_privileges_from_optional_expr(
                catalog_reader,
                having.as_ref(),
                reqs,
            )?;
            collect_required_hybrid_privileges_from_optional_expr(
                catalog_reader,
                filter.as_ref(),
                reqs,
            )?;
            collect_required_hybrid_privileges_from_sort_exprs(catalog_reader, order_by, reqs)?;
            collect_required_hybrid_privileges_from_optional_expr(
                catalog_reader,
                limit.as_ref(),
                reqs,
            )?;
            collect_required_hybrid_privileges_from_optional_expr(
                catalog_reader,
                offset.as_ref(),
                reqs,
            )?;
            collect_required_hybrid_privileges_from_exprs(catalog_reader, distinct_on, reqs)?;
        }
        LogicalPlan::InsertValues {
            rows,
            on_conflict,
            returning,
            ..
        } => {
            for row in rows {
                collect_required_hybrid_privileges_from_exprs(catalog_reader, row, reqs)?;
            }
            collect_required_hybrid_privileges_from_on_conflict(
                catalog_reader,
                on_conflict.as_ref(),
                reqs,
            )?;
            collect_required_hybrid_privileges_from_projection_exprs(
                catalog_reader,
                returning,
                reqs,
            )?;
        }
        LogicalPlan::InsertSelect {
            assignments,
            source,
            on_conflict,
            returning,
            ..
        } => {
            collect_required_hybrid_privileges_from_exprs(catalog_reader, assignments, reqs)?;
            collect_required_hybrid_privileges_from_logical_plan(catalog_reader, source, reqs)?;
            collect_required_hybrid_privileges_from_on_conflict(
                catalog_reader,
                on_conflict.as_ref(),
                reqs,
            )?;
            collect_required_hybrid_privileges_from_projection_exprs(
                catalog_reader,
                returning,
                reqs,
            )?;
        }
        LogicalPlan::DeleteFromTable {
            filter, returning, ..
        } => {
            collect_required_hybrid_privileges_from_optional_expr(
                catalog_reader,
                filter.as_ref(),
                reqs,
            )?;
            collect_required_hybrid_privileges_from_projection_exprs(
                catalog_reader,
                returning,
                reqs,
            )?;
        }
        LogicalPlan::UpdateTable {
            assignments,
            filter,
            returning,
            ..
        } => {
            collect_required_hybrid_privileges_from_assignments(catalog_reader, assignments, reqs)?;
            collect_required_hybrid_privileges_from_optional_expr(
                catalog_reader,
                filter.as_ref(),
                reqs,
            )?;
            collect_required_hybrid_privileges_from_projection_exprs(
                catalog_reader,
                returning,
                reqs,
            )?;
        }
        LogicalPlan::SetOperation {
            left,
            right,
            order_by,
            limit,
            offset,
            ..
        } => {
            collect_required_hybrid_privileges_from_logical_plan(catalog_reader, left, reqs)?;
            collect_required_hybrid_privileges_from_logical_plan(catalog_reader, right, reqs)?;
            collect_required_hybrid_privileges_from_sort_exprs(catalog_reader, order_by, reqs)?;
            collect_required_hybrid_privileges_from_optional_expr(
                catalog_reader,
                limit.as_ref(),
                reqs,
            )?;
            collect_required_hybrid_privileges_from_optional_expr(
                catalog_reader,
                offset.as_ref(),
                reqs,
            )?;
        }
        LogicalPlan::ProjectValues {
            rows,
            order_by,
            limit,
            offset,
            ..
        } => {
            for row in rows {
                collect_required_hybrid_privileges_from_exprs(catalog_reader, row, reqs)?;
            }
            collect_required_hybrid_privileges_from_sort_exprs(catalog_reader, order_by, reqs)?;
            collect_required_hybrid_privileges_from_optional_expr(
                catalog_reader,
                limit.as_ref(),
                reqs,
            )?;
            collect_required_hybrid_privileges_from_optional_expr(
                catalog_reader,
                offset.as_ref(),
                reqs,
            )?;
        }
        LogicalPlan::MergeTable(merge) => {
            collect_required_hybrid_privileges_from_merge_plan(catalog_reader, merge, reqs)?;
        }
        LogicalPlan::RecursiveCte {
            base, recursive, ..
        } => {
            collect_required_hybrid_privileges_from_logical_plan(catalog_reader, base, reqs)?;
            collect_required_hybrid_privileges_from_logical_plan(catalog_reader, recursive, reqs)?;
        }
        LogicalPlan::HybridFunctionScan {
            function_name,
            args,
            ..
        } => {
            push_required_hybrid_privileges(
                reqs,
                required_hybrid_function_privileges(catalog_reader, function_name, args)?,
            );
            collect_required_hybrid_privileges_from_exprs(catalog_reader, args, reqs)?;
        }
        LogicalPlan::CreateTableAs { source, .. } => {
            collect_required_hybrid_privileges_from_logical_plan(catalog_reader, source, reqs)?;
        }
        _ => {}
    }
    Ok(())
}

fn collect_required_hybrid_privileges_from_merge_plan(
    catalog_reader: &dyn CatalogReader,
    merge: &MergePlan,
    reqs: &mut Vec<(CatalogPrivilege, RelationId)>,
) -> DbResult<()> {
    if let Some(source_subquery_plan) = merge.source_subquery_plan.as_deref() {
        collect_required_hybrid_privileges_from_physical_plan(
            catalog_reader,
            source_subquery_plan,
            reqs,
        )?;
    }
    collect_required_hybrid_privileges_from_expr(catalog_reader, &merge.on_condition, reqs)?;
    for clause in &merge.when_clauses {
        collect_required_hybrid_privileges_from_optional_expr(
            catalog_reader,
            clause.condition.as_ref(),
            reqs,
        )?;
        match &clause.action {
            MergeActionPlan::Update { assignments } => {
                collect_required_hybrid_privileges_from_assignments(
                    catalog_reader,
                    assignments,
                    reqs,
                )?;
            }
            MergeActionPlan::Insert { values } => {
                collect_required_hybrid_privileges_from_exprs(catalog_reader, values, reqs)?;
            }
            MergeActionPlan::Delete
            | MergeActionPlan::InsertDefaultValues
            | MergeActionPlan::DoNothing => {}
        }
    }
    Ok(())
}

fn collect_required_hybrid_privileges_from_on_conflict(
    catalog_reader: &dyn CatalogReader,
    on_conflict: Option<&InsertOnConflict>,
    reqs: &mut Vec<(CatalogPrivilege, RelationId)>,
) -> DbResult<()> {
    let Some(on_conflict) = on_conflict else {
        return Ok(());
    };
    match &on_conflict.action {
        OnConflictActionPlan::DoNothing => {}
        OnConflictActionPlan::DoUpdate {
            assignments,
            where_clause,
        } => {
            collect_required_hybrid_privileges_from_assignments(catalog_reader, assignments, reqs)?;
            collect_required_hybrid_privileges_from_optional_expr(
                catalog_reader,
                where_clause.as_ref(),
                reqs,
            )?;
        }
    }
    Ok(())
}

fn collect_required_hybrid_privileges_from_assignments(
    catalog_reader: &dyn CatalogReader,
    assignments: &[UpdateAssignment],
    reqs: &mut Vec<(CatalogPrivilege, RelationId)>,
) -> DbResult<()> {
    for assignment in assignments {
        collect_required_hybrid_privileges_from_expr(catalog_reader, &assignment.expr, reqs)?;
    }
    Ok(())
}

fn collect_required_hybrid_privileges_from_projection_exprs(
    catalog_reader: &dyn CatalogReader,
    exprs: &[ProjectionExpr],
    reqs: &mut Vec<(CatalogPrivilege, RelationId)>,
) -> DbResult<()> {
    for expr in exprs {
        collect_required_hybrid_privileges_from_expr(catalog_reader, &expr.expr, reqs)?;
    }
    Ok(())
}

fn collect_required_hybrid_privileges_from_sort_exprs(
    catalog_reader: &dyn CatalogReader,
    exprs: &[SortExpr],
    reqs: &mut Vec<(CatalogPrivilege, RelationId)>,
) -> DbResult<()> {
    for expr in exprs {
        collect_required_hybrid_privileges_from_expr(catalog_reader, &expr.expr, reqs)?;
    }
    Ok(())
}

fn collect_required_hybrid_privileges_from_exprs(
    catalog_reader: &dyn CatalogReader,
    exprs: &[TypedExpr],
    reqs: &mut Vec<(CatalogPrivilege, RelationId)>,
) -> DbResult<()> {
    for expr in exprs {
        collect_required_hybrid_privileges_from_expr(catalog_reader, expr, reqs)?;
    }
    Ok(())
}

fn collect_required_hybrid_privileges_from_optional_expr(
    catalog_reader: &dyn CatalogReader,
    expr: Option<&TypedExpr>,
    reqs: &mut Vec<(CatalogPrivilege, RelationId)>,
) -> DbResult<()> {
    if let Some(expr) = expr {
        collect_required_hybrid_privileges_from_expr(catalog_reader, expr, reqs)?;
    }
    Ok(())
}

fn collect_required_hybrid_privileges_from_expr(
    catalog_reader: &dyn CatalogReader,
    expr: &TypedExpr,
    reqs: &mut Vec<(CatalogPrivilege, RelationId)>,
) -> DbResult<()> {
    let _guard = enter_auth_depth(
        &AUTH_HYBRID_EXPR_DEPTH,
        MAX_AUTH_HYBRID_EXPR_DEPTH,
        "hybrid expression",
    )?;
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
            collect_required_hybrid_privileges_from_expr(catalog_reader, left, reqs)?;
            collect_required_hybrid_privileges_from_expr(catalog_reader, right, reqs)?;
        }
        TypedExprKind::LogicalNot { expr }
        | TypedExprKind::Negate { expr }
        | TypedExprKind::IsNull { expr, .. }
        | TypedExprKind::Cast { expr, .. } => {
            collect_required_hybrid_privileges_from_expr(catalog_reader, expr, reqs)?;
        }
        TypedExprKind::Like { expr, pattern, .. } => {
            collect_required_hybrid_privileges_from_expr(catalog_reader, expr, reqs)?;
            collect_required_hybrid_privileges_from_expr(catalog_reader, pattern, reqs)?;
        }
        TypedExprKind::InList { expr, list, .. } => {
            collect_required_hybrid_privileges_from_expr(catalog_reader, expr, reqs)?;
            collect_required_hybrid_privileges_from_exprs(catalog_reader, list, reqs)?;
        }
        TypedExprKind::Between {
            expr, low, high, ..
        } => {
            collect_required_hybrid_privileges_from_expr(catalog_reader, expr, reqs)?;
            collect_required_hybrid_privileges_from_expr(catalog_reader, low, reqs)?;
            collect_required_hybrid_privileges_from_expr(catalog_reader, high, reqs)?;
        }
        TypedExprKind::CaseWhen {
            conditions,
            results,
            else_result,
        } => {
            collect_required_hybrid_privileges_from_exprs(catalog_reader, conditions, reqs)?;
            collect_required_hybrid_privileges_from_exprs(catalog_reader, results, reqs)?;
            collect_required_hybrid_privileges_from_optional_expr(
                catalog_reader,
                else_result.as_deref(),
                reqs,
            )?;
        }
        TypedExprKind::Coalesce { args } | TypedExprKind::ArrayConstruct { elements: args } => {
            collect_required_hybrid_privileges_from_exprs(catalog_reader, args, reqs)?;
        }
        TypedExprKind::AggCount { expr, filter, .. } => {
            collect_required_hybrid_privileges_from_optional_expr(
                catalog_reader,
                expr.as_deref(),
                reqs,
            )?;
            collect_required_hybrid_privileges_from_optional_expr(
                catalog_reader,
                filter.as_deref(),
                reqs,
            )?;
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
            collect_required_hybrid_privileges_from_expr(catalog_reader, expr, reqs)?;
            collect_required_hybrid_privileges_from_optional_expr(
                catalog_reader,
                filter.as_deref(),
                reqs,
            )?;
        }
        TypedExprKind::AggStringAgg {
            expr,
            delimiter,
            filter,
            ..
        } => {
            collect_required_hybrid_privileges_from_expr(catalog_reader, expr, reqs)?;
            collect_required_hybrid_privileges_from_expr(catalog_reader, delimiter, reqs)?;
            collect_required_hybrid_privileges_from_optional_expr(
                catalog_reader,
                filter.as_deref(),
                reqs,
            )?;
        }
        TypedExprKind::AggArrayAgg { expr, filter, .. } => {
            collect_required_hybrid_privileges_from_expr(catalog_reader, expr, reqs)?;
            collect_required_hybrid_privileges_from_optional_expr(
                catalog_reader,
                filter.as_deref(),
                reqs,
            )?;
        }
        TypedExprKind::ScalarFunction { func, args } => {
            if let ScalarFunction::Generic(name) = func {
                push_required_hybrid_privileges(
                    reqs,
                    required_hybrid_function_privileges(catalog_reader, name, args)?,
                );
            }
            collect_required_hybrid_privileges_from_exprs(catalog_reader, args, reqs)?;
        }
        TypedExprKind::UserFunction { args, .. } => {
            collect_required_hybrid_privileges_from_exprs(catalog_reader, args, reqs)?;
        }
        TypedExprKind::ScalarSubquery { plan }
        | TypedExprKind::ArraySubquery { plan }
        | TypedExprKind::ExistsSubquery { plan, .. } => {
            collect_required_hybrid_privileges_from_logical_plan(catalog_reader, plan, reqs)?;
        }
        TypedExprKind::InSubquery { expr, plan, .. } => {
            collect_required_hybrid_privileges_from_expr(catalog_reader, expr, reqs)?;
            collect_required_hybrid_privileges_from_logical_plan(catalog_reader, plan, reqs)?;
        }
        TypedExprKind::WindowFunction {
            args,
            partition_by,
            order_by,
            ..
        } => {
            collect_required_hybrid_privileges_from_exprs(catalog_reader, args, reqs)?;
            collect_required_hybrid_privileges_from_exprs(catalog_reader, partition_by, reqs)?;
            collect_required_hybrid_privileges_from_sort_exprs(catalog_reader, order_by, reqs)?;
        }
        TypedExprKind::ColumnRef { .. } | TypedExprKind::OuterColumnRef { .. } => {}
        TypedExprKind::Literal(_) | TypedExprKind::NextValue { .. } => {}
    }
    Ok(())
}
