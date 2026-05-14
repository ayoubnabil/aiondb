use aiondb_core::{Row, Value};
use aiondb_plan::{LogicalPlan, TypedExpr, TypedExprKind};

pub(crate) fn substitute_outer_column_refs(expr: &TypedExpr, outer_row: &Row) -> TypedExpr {
    match &expr.kind {
        TypedExprKind::OuterColumnRef { ordinal, .. } => {
            let value = outer_row
                .values
                .get(*ordinal)
                .cloned()
                .unwrap_or(Value::Null);
            let nullable = matches!(value, Value::Null);
            TypedExpr::literal(value, expr.data_type.clone(), nullable)
        }
        TypedExprKind::BinaryEq { left, right } => TypedExpr::binary_eq(
            substitute_outer_column_refs(left, outer_row),
            substitute_outer_column_refs(right, outer_row),
        ),
        TypedExprKind::BinaryNe { left, right } => TypedExpr::binary_ne(
            substitute_outer_column_refs(left, outer_row),
            substitute_outer_column_refs(right, outer_row),
        ),
        TypedExprKind::BinaryGt { left, right } => TypedExpr::binary_gt(
            substitute_outer_column_refs(left, outer_row),
            substitute_outer_column_refs(right, outer_row),
        ),
        TypedExprKind::BinaryGe { left, right } => TypedExpr::binary_ge(
            substitute_outer_column_refs(left, outer_row),
            substitute_outer_column_refs(right, outer_row),
        ),
        TypedExprKind::BinaryLt { left, right } => TypedExpr::binary_lt(
            substitute_outer_column_refs(left, outer_row),
            substitute_outer_column_refs(right, outer_row),
        ),
        TypedExprKind::BinaryLe { left, right } => TypedExpr::binary_le(
            substitute_outer_column_refs(left, outer_row),
            substitute_outer_column_refs(right, outer_row),
        ),
        TypedExprKind::LogicalAnd { left, right } => TypedExpr::logical_and(
            substitute_outer_column_refs(left, outer_row),
            substitute_outer_column_refs(right, outer_row),
        ),
        TypedExprKind::LogicalOr { left, right } => TypedExpr::logical_or(
            substitute_outer_column_refs(left, outer_row),
            substitute_outer_column_refs(right, outer_row),
        ),
        TypedExprKind::LogicalNot { expr: inner } => {
            TypedExpr::logical_not(substitute_outer_column_refs(inner, outer_row))
        }
        TypedExprKind::ArithAdd { left, right } => TypedExpr::arith_add(
            substitute_outer_column_refs(left, outer_row),
            substitute_outer_column_refs(right, outer_row),
            expr.data_type.clone(),
            expr.nullable,
        ),
        TypedExprKind::ArithSub { left, right } => TypedExpr::arith_sub(
            substitute_outer_column_refs(left, outer_row),
            substitute_outer_column_refs(right, outer_row),
            expr.data_type.clone(),
            expr.nullable,
        ),
        TypedExprKind::ArithMul { left, right } => TypedExpr::arith_mul(
            substitute_outer_column_refs(left, outer_row),
            substitute_outer_column_refs(right, outer_row),
            expr.data_type.clone(),
            expr.nullable,
        ),
        TypedExprKind::ArithDiv { left, right } => TypedExpr::arith_div(
            substitute_outer_column_refs(left, outer_row),
            substitute_outer_column_refs(right, outer_row),
            expr.data_type.clone(),
            expr.nullable,
        ),
        TypedExprKind::ArithMod { left, right } => TypedExpr::arith_mod(
            substitute_outer_column_refs(left, outer_row),
            substitute_outer_column_refs(right, outer_row),
            expr.data_type.clone(),
            expr.nullable,
        ),
        TypedExprKind::IsNull {
            expr: inner,
            negated,
        } => TypedExpr::is_null(substitute_outer_column_refs(inner, outer_row), *negated),
        TypedExprKind::Cast {
            expr: inner,
            target_type,
        } => TypedExpr::cast(
            substitute_outer_column_refs(inner, outer_row),
            target_type.clone(),
        ),
        TypedExprKind::Like {
            expr: inner,
            pattern,
            negated,
            case_insensitive,
        } => TypedExpr::like(
            substitute_outer_column_refs(inner, outer_row),
            substitute_outer_column_refs(pattern, outer_row),
            *negated,
            *case_insensitive,
        ),
        TypedExprKind::InList {
            expr: inner,
            list,
            negated,
        } => TypedExpr::in_list(
            substitute_outer_column_refs(inner, outer_row),
            list.iter()
                .map(|e| substitute_outer_column_refs(e, outer_row))
                .collect(),
            *negated,
        ),
        TypedExprKind::Between {
            expr: inner,
            low,
            high,
            negated,
        } => TypedExpr::between(
            substitute_outer_column_refs(inner, outer_row),
            substitute_outer_column_refs(low, outer_row),
            substitute_outer_column_refs(high, outer_row),
            *negated,
        ),
        TypedExprKind::Concat { left, right } => {
            let l = substitute_outer_column_refs(left, outer_row);
            let r = substitute_outer_column_refs(right, outer_row);
            TypedExpr::concat_typed(l, r, expr.data_type.clone())
        }
        TypedExprKind::Negate { expr: inner } => TypedExpr::negate(
            substitute_outer_column_refs(inner, outer_row),
            expr.data_type.clone(),
            expr.nullable,
        ),
        TypedExprKind::CaseWhen {
            conditions,
            results,
            else_result,
        } => TypedExpr::case_when(
            conditions
                .iter()
                .map(|c| substitute_outer_column_refs(c, outer_row))
                .collect(),
            results
                .iter()
                .map(|r| substitute_outer_column_refs(r, outer_row))
                .collect(),
            else_result
                .as_ref()
                .map(|e| substitute_outer_column_refs(e, outer_row)),
            expr.data_type.clone(),
            expr.nullable,
        ),
        TypedExprKind::Coalesce { args } => TypedExpr::coalesce(
            args.iter()
                .map(|a| substitute_outer_column_refs(a, outer_row))
                .collect(),
            expr.data_type.clone(),
        ),
        TypedExprKind::IsDistinctFrom {
            left,
            right,
            negated,
        } => TypedExpr::is_distinct_from(
            substitute_outer_column_refs(left, outer_row),
            substitute_outer_column_refs(right, outer_row),
            *negated,
        ),
        TypedExprKind::ScalarFunction { func, args } => TypedExpr::scalar_function(
            func.clone(),
            args.iter()
                .map(|a| substitute_outer_column_refs(a, outer_row))
                .collect(),
            expr.data_type.clone(),
            expr.nullable,
        ),
        TypedExprKind::Nullif { left, right } => TypedExpr::nullif(
            substitute_outer_column_refs(left, outer_row),
            substitute_outer_column_refs(right, outer_row),
            expr.data_type.clone(),
        ),
        TypedExprKind::AggCount {
            expr: inner,
            distinct,
            filter,
        } => TypedExpr {
            kind: TypedExprKind::AggCount {
                expr: inner
                    .as_ref()
                    .map(|e| Box::new(substitute_outer_column_refs(e, outer_row))),
                distinct: *distinct,
                filter: filter
                    .as_ref()
                    .map(|f| Box::new(substitute_outer_column_refs(f, outer_row))),
            },
            data_type: expr.data_type.clone(),
            nullable: expr.nullable,
        },
        TypedExprKind::AggSum {
            expr: inner,
            distinct,
            filter,
        } => TypedExpr {
            kind: TypedExprKind::AggSum {
                expr: Box::new(substitute_outer_column_refs(inner, outer_row)),
                distinct: *distinct,
                filter: filter
                    .as_ref()
                    .map(|f| Box::new(substitute_outer_column_refs(f, outer_row))),
            },
            data_type: expr.data_type.clone(),
            nullable: expr.nullable,
        },
        TypedExprKind::AggAnyValue {
            expr: inner,
            filter,
        } => TypedExpr {
            kind: TypedExprKind::AggAnyValue {
                expr: Box::new(substitute_outer_column_refs(inner, outer_row)),
                filter: filter
                    .as_ref()
                    .map(|f| Box::new(substitute_outer_column_refs(f, outer_row))),
            },
            data_type: expr.data_type.clone(),
            nullable: expr.nullable,
        },
        TypedExprKind::AggMin {
            expr: inner,
            filter,
        } => TypedExpr {
            kind: TypedExprKind::AggMin {
                expr: Box::new(substitute_outer_column_refs(inner, outer_row)),
                filter: filter
                    .as_ref()
                    .map(|f| Box::new(substitute_outer_column_refs(f, outer_row))),
            },
            data_type: expr.data_type.clone(),
            nullable: expr.nullable,
        },
        TypedExprKind::AggMax {
            expr: inner,
            filter,
        } => TypedExpr {
            kind: TypedExprKind::AggMax {
                expr: Box::new(substitute_outer_column_refs(inner, outer_row)),
                filter: filter
                    .as_ref()
                    .map(|f| Box::new(substitute_outer_column_refs(f, outer_row))),
            },
            data_type: expr.data_type.clone(),
            nullable: expr.nullable,
        },
        TypedExprKind::JsonGet { left, right } => TypedExpr::json_get(
            substitute_outer_column_refs(left, outer_row),
            substitute_outer_column_refs(right, outer_row),
        ),
        TypedExprKind::JsonGetText { left, right } => TypedExpr::json_get_text(
            substitute_outer_column_refs(left, outer_row),
            substitute_outer_column_refs(right, outer_row),
        ),
        TypedExprKind::JsonContains { left, right } => TypedExpr::json_contains(
            substitute_outer_column_refs(left, outer_row),
            substitute_outer_column_refs(right, outer_row),
        ),
        TypedExprKind::JsonContainedBy { left, right } => TypedExpr::json_contained_by(
            substitute_outer_column_refs(left, outer_row),
            substitute_outer_column_refs(right, outer_row),
        ),
        TypedExprKind::ArrayConcat { left, right } => TypedExpr::array_concat(
            substitute_outer_column_refs(left, outer_row),
            substitute_outer_column_refs(right, outer_row),
            expr.data_type.clone(),
        ),
        TypedExprKind::ArrayContains { left, right } => TypedExpr::array_contains(
            substitute_outer_column_refs(left, outer_row),
            substitute_outer_column_refs(right, outer_row),
        ),
        TypedExprKind::ArrayContainedBy { left, right } => TypedExpr::array_contained_by(
            substitute_outer_column_refs(left, outer_row),
            substitute_outer_column_refs(right, outer_row),
        ),
        TypedExprKind::ArrayOverlap { left, right } => TypedExpr::array_overlap(
            substitute_outer_column_refs(left, outer_row),
            substitute_outer_column_refs(right, outer_row),
        ),
        TypedExprKind::ArrayConstruct { elements } => TypedExpr::array_construct(
            elements
                .iter()
                .map(|e| substitute_outer_column_refs(e, outer_row))
                .collect(),
            match &expr.data_type {
                aiondb_core::DataType::Array(inner) => inner.as_ref().clone(),
                other => other.clone(),
            },
            expr.nullable,
        ),
        TypedExprKind::ScalarSubquery { plan } => TypedExpr::scalar_subquery(
            substitute_outer_refs_in_plan(plan, outer_row),
            expr.data_type.clone(),
            expr.nullable,
        ),
        TypedExprKind::ArraySubquery { plan } => TypedExpr::array_subquery(
            substitute_outer_refs_in_plan(plan, outer_row),
            expr.data_type.clone(),
        ),
        TypedExprKind::ExistsSubquery { plan, negated } => {
            TypedExpr::exists_subquery(substitute_outer_refs_in_plan(plan, outer_row), *negated)
        }
        TypedExprKind::InSubquery {
            expr: inner,
            plan,
            negated,
        } => TypedExpr::in_subquery(
            substitute_outer_column_refs(inner, outer_row),
            substitute_outer_refs_in_plan(plan, outer_row),
            *negated,
        ),
        _ => expr.clone(),
    }
}

pub(crate) fn substitute_outer_refs_in_plan(plan: &LogicalPlan, outer_row: &Row) -> LogicalPlan {
    match plan {
        LogicalPlan::ProjectOnce {
            outputs,
            filter,
            order_by,
            limit,
            offset,
            distinct,
            distinct_on,
        } => LogicalPlan::ProjectOnce {
            outputs: outputs
                .iter()
                .map(|o| aiondb_plan::ProjectionExpr {
                    field: o.field.clone(),
                    expr: substitute_outer_column_refs(&o.expr, outer_row),
                })
                .collect(),
            filter: filter
                .as_ref()
                .map(|f| substitute_outer_column_refs(f, outer_row)),
            order_by: order_by
                .iter()
                .map(|s| aiondb_plan::SortExpr {
                    expr: substitute_outer_column_refs(&s.expr, outer_row),
                    descending: s.descending,
                    nulls_first: s.nulls_first,
                })
                .collect(),
            limit: limit
                .as_ref()
                .map(|l| substitute_outer_column_refs(l, outer_row)),
            offset: offset
                .as_ref()
                .map(|o| substitute_outer_column_refs(o, outer_row)),
            distinct: *distinct,
            distinct_on: distinct_on
                .iter()
                .map(|e| substitute_outer_column_refs(e, outer_row))
                .collect(),
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
        } => LogicalPlan::ProjectTable {
            table_id: *table_id,
            outputs: outputs
                .iter()
                .map(|o| aiondb_plan::ProjectionExpr {
                    field: o.field.clone(),
                    expr: substitute_outer_column_refs(&o.expr, outer_row),
                })
                .collect(),
            filter: filter
                .as_ref()
                .map(|f| substitute_outer_column_refs(f, outer_row)),
            order_by: order_by
                .iter()
                .map(|s| aiondb_plan::SortExpr {
                    expr: substitute_outer_column_refs(&s.expr, outer_row),
                    descending: s.descending,
                    nulls_first: s.nulls_first,
                })
                .collect(),
            limit: limit
                .as_ref()
                .map(|l| substitute_outer_column_refs(l, outer_row)),
            offset: offset
                .as_ref()
                .map(|o| substitute_outer_column_refs(o, outer_row)),
            distinct: *distinct,
            distinct_on: distinct_on
                .iter()
                .map(|e| substitute_outer_column_refs(e, outer_row))
                .collect(),
        },
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
        } => LogicalPlan::LockingProjectTable {
            table_id: *table_id,
            outputs: outputs
                .iter()
                .map(|o| aiondb_plan::ProjectionExpr {
                    field: o.field.clone(),
                    expr: substitute_outer_column_refs(&o.expr, outer_row),
                })
                .collect(),
            filter: filter
                .as_ref()
                .map(|f| substitute_outer_column_refs(f, outer_row)),
            order_by: order_by
                .iter()
                .map(|s| aiondb_plan::SortExpr {
                    expr: substitute_outer_column_refs(&s.expr, outer_row),
                    descending: s.descending,
                    nulls_first: s.nulls_first,
                })
                .collect(),
            limit: limit
                .as_ref()
                .map(|l| substitute_outer_column_refs(l, outer_row)),
            offset: offset
                .as_ref()
                .map(|o| substitute_outer_column_refs(o, outer_row)),
            distinct: *distinct,
            distinct_on: distinct_on
                .iter()
                .map(|e| substitute_outer_column_refs(e, outer_row))
                .collect(),
            row_lock: *row_lock,
        },
        LogicalPlan::ProjectSource {
            source,
            outputs,
            filter,
            order_by,
            limit,
            offset,
            distinct,
            distinct_on,
        } => LogicalPlan::ProjectSource {
            source: Box::new(substitute_outer_refs_in_plan(source, outer_row)),
            outputs: outputs
                .iter()
                .map(|output| aiondb_plan::ProjectionExpr {
                    field: output.field.clone(),
                    expr: substitute_outer_column_refs(&output.expr, outer_row),
                })
                .collect(),
            filter: filter
                .as_ref()
                .map(|expr| substitute_outer_column_refs(expr, outer_row)),
            order_by: order_by
                .iter()
                .map(|sort| aiondb_plan::SortExpr {
                    expr: substitute_outer_column_refs(&sort.expr, outer_row),
                    descending: sort.descending,
                    nulls_first: sort.nulls_first,
                })
                .collect(),
            limit: limit
                .as_ref()
                .map(|expr| substitute_outer_column_refs(expr, outer_row)),
            offset: offset
                .as_ref()
                .map(|expr| substitute_outer_column_refs(expr, outer_row)),
            distinct: *distinct,
            distinct_on: distinct_on
                .iter()
                .map(|expr| substitute_outer_column_refs(expr, outer_row))
                .collect(),
        },
        LogicalPlan::HybridFunctionScan {
            function_name,
            args,
            output_fields,
        } => LogicalPlan::HybridFunctionScan {
            function_name: function_name.clone(),
            args: args
                .iter()
                .map(|expr| substitute_outer_column_refs(expr, outer_row))
                .collect(),
            output_fields: output_fields.clone(),
        },
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
        } => LogicalPlan::Aggregate {
            table_id: *table_id,
            group_by: group_by
                .iter()
                .map(|expr| substitute_outer_column_refs(expr, outer_row))
                .collect(),
            grouping_sets: grouping_sets.clone(),
            aggregates: aggregates
                .iter()
                .map(|output| aiondb_plan::ProjectionExpr {
                    field: output.field.clone(),
                    expr: substitute_outer_column_refs(&output.expr, outer_row),
                })
                .collect(),
            having: having
                .as_ref()
                .map(|expr| substitute_outer_column_refs(expr, outer_row)),
            filter: filter
                .as_ref()
                .map(|expr| substitute_outer_column_refs(expr, outer_row)),
            order_by: order_by
                .iter()
                .map(|sort| aiondb_plan::SortExpr {
                    expr: substitute_outer_column_refs(&sort.expr, outer_row),
                    descending: sort.descending,
                    nulls_first: sort.nulls_first,
                })
                .collect(),
            limit: limit
                .as_ref()
                .map(|expr| substitute_outer_column_refs(expr, outer_row)),
            offset: offset
                .as_ref()
                .map(|expr| substitute_outer_column_refs(expr, outer_row)),
            distinct: *distinct,
            distinct_on: distinct_on
                .iter()
                .map(|expr| substitute_outer_column_refs(expr, outer_row))
                .collect(),
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
        } => LogicalPlan::AggregateSource {
            source: Box::new(substitute_outer_refs_in_plan(source, outer_row)),
            group_by: group_by
                .iter()
                .map(|expr| substitute_outer_column_refs(expr, outer_row))
                .collect(),
            grouping_sets: grouping_sets.clone(),
            aggregates: aggregates
                .iter()
                .map(|output| aiondb_plan::ProjectionExpr {
                    field: output.field.clone(),
                    expr: substitute_outer_column_refs(&output.expr, outer_row),
                })
                .collect(),
            having: having
                .as_ref()
                .map(|expr| substitute_outer_column_refs(expr, outer_row)),
            filter: filter
                .as_ref()
                .map(|expr| substitute_outer_column_refs(expr, outer_row)),
            order_by: order_by
                .iter()
                .map(|sort| aiondb_plan::SortExpr {
                    expr: substitute_outer_column_refs(&sort.expr, outer_row),
                    descending: sort.descending,
                    nulls_first: sort.nulls_first,
                })
                .collect(),
            limit: limit
                .as_ref()
                .map(|expr| substitute_outer_column_refs(expr, outer_row)),
            offset: offset
                .as_ref()
                .map(|expr| substitute_outer_column_refs(expr, outer_row)),
            distinct: *distinct,
            distinct_on: distinct_on
                .iter()
                .map(|expr| substitute_outer_column_refs(expr, outer_row))
                .collect(),
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
        } => LogicalPlan::SetOperation {
            op: *op,
            all: *all,
            left: Box::new(substitute_outer_refs_in_plan(left, outer_row)),
            right: Box::new(substitute_outer_refs_in_plan(right, outer_row)),
            output_fields: output_fields.clone(),
            order_by: order_by
                .iter()
                .map(|sort| aiondb_plan::SortExpr {
                    expr: substitute_outer_column_refs(&sort.expr, outer_row),
                    descending: sort.descending,
                    nulls_first: sort.nulls_first,
                })
                .collect(),
            limit: limit
                .as_ref()
                .map(|expr| substitute_outer_column_refs(expr, outer_row)),
            offset: offset
                .as_ref()
                .map(|expr| substitute_outer_column_refs(expr, outer_row)),
        },
        LogicalPlan::ProjectValues {
            output_fields,
            rows,
            order_by,
            limit,
            offset,
        } => LogicalPlan::ProjectValues {
            output_fields: output_fields.clone(),
            rows: rows
                .iter()
                .map(|row| {
                    row.iter()
                        .map(|expr| substitute_outer_column_refs(expr, outer_row))
                        .collect()
                })
                .collect(),
            order_by: order_by
                .iter()
                .map(|sort| aiondb_plan::SortExpr {
                    expr: substitute_outer_column_refs(&sort.expr, outer_row),
                    descending: sort.descending,
                    nulls_first: sort.nulls_first,
                })
                .collect(),
            limit: limit
                .as_ref()
                .map(|expr| substitute_outer_column_refs(expr, outer_row)),
            offset: offset
                .as_ref()
                .map(|expr| substitute_outer_column_refs(expr, outer_row)),
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
        } => LogicalPlan::NestedLoopJoin {
            left: Box::new(substitute_outer_refs_in_plan(left, outer_row)),
            right: Box::new(substitute_outer_refs_in_plan(right, outer_row)),
            join_type: *join_type,
            condition: condition
                .as_ref()
                .map(|expr| substitute_outer_column_refs(expr, outer_row)),
            outputs: outputs
                .iter()
                .map(|output| aiondb_plan::ProjectionExpr {
                    field: output.field.clone(),
                    expr: substitute_outer_column_refs(&output.expr, outer_row),
                })
                .collect(),
            filter: filter
                .as_ref()
                .map(|expr| substitute_outer_column_refs(expr, outer_row)),
            order_by: order_by
                .iter()
                .map(|sort| aiondb_plan::SortExpr {
                    expr: substitute_outer_column_refs(&sort.expr, outer_row),
                    descending: sort.descending,
                    nulls_first: sort.nulls_first,
                })
                .collect(),
            limit: limit
                .as_ref()
                .map(|expr| substitute_outer_column_refs(expr, outer_row)),
            offset: offset
                .as_ref()
                .map(|expr| substitute_outer_column_refs(expr, outer_row)),
            distinct: *distinct,
            distinct_on: distinct_on
                .iter()
                .map(|expr| substitute_outer_column_refs(expr, outer_row))
                .collect(),
        },
        LogicalPlan::RecursiveCte {
            base,
            recursive,
            union_all,
            output_fields,
        } => LogicalPlan::RecursiveCte {
            base: Box::new(substitute_outer_refs_in_plan(base, outer_row)),
            recursive: Box::new(substitute_outer_refs_in_plan(recursive, outer_row)),
            union_all: *union_all,
            output_fields: output_fields.clone(),
        },
        other => other.clone(),
    }
}

pub(crate) fn expr_contains_outer_refs(expr: &TypedExpr) -> bool {
    match &expr.kind {
        TypedExprKind::OuterColumnRef { .. } => true,
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
        | TypedExprKind::IsDistinctFrom { left, right, .. }
        | TypedExprKind::Nullif { left, right } => {
            expr_contains_outer_refs(left) || expr_contains_outer_refs(right)
        }
        TypedExprKind::LogicalNot { expr }
        | TypedExprKind::Negate { expr }
        | TypedExprKind::IsNull { expr, .. }
        | TypedExprKind::Cast { expr, .. } => expr_contains_outer_refs(expr),
        TypedExprKind::Like { expr, pattern, .. } => {
            expr_contains_outer_refs(expr) || expr_contains_outer_refs(pattern)
        }
        TypedExprKind::InList { expr, list, .. } => {
            expr_contains_outer_refs(expr) || list.iter().any(expr_contains_outer_refs)
        }
        TypedExprKind::Between {
            expr, low, high, ..
        } => {
            expr_contains_outer_refs(expr)
                || expr_contains_outer_refs(low)
                || expr_contains_outer_refs(high)
        }
        TypedExprKind::CaseWhen {
            conditions,
            results,
            else_result,
        } => {
            conditions.iter().any(expr_contains_outer_refs)
                || results.iter().any(expr_contains_outer_refs)
                || else_result
                    .as_ref()
                    .is_some_and(|expr| expr_contains_outer_refs(expr))
        }
        TypedExprKind::Coalesce { args }
        | TypedExprKind::ScalarFunction { args, .. }
        | TypedExprKind::ArrayConstruct { elements: args }
        | TypedExprKind::UserFunction { args, .. } => args.iter().any(expr_contains_outer_refs),
        TypedExprKind::AggCount { expr, filter, .. } => {
            expr.as_ref()
                .is_some_and(|expr| expr_contains_outer_refs(expr))
                || filter
                    .as_ref()
                    .is_some_and(|filter_expr| expr_contains_outer_refs(filter_expr))
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
            expr_contains_outer_refs(expr)
                || filter
                    .as_ref()
                    .is_some_and(|filter_expr| expr_contains_outer_refs(filter_expr))
        }
        TypedExprKind::AggStringAgg {
            expr,
            delimiter,
            filter,
            ..
        } => {
            expr_contains_outer_refs(expr)
                || expr_contains_outer_refs(delimiter)
                || filter
                    .as_ref()
                    .is_some_and(|filter_expr| expr_contains_outer_refs(filter_expr))
        }
        TypedExprKind::AggArrayAgg { expr, filter, .. } => {
            expr_contains_outer_refs(expr)
                || filter
                    .as_ref()
                    .is_some_and(|filter_expr| expr_contains_outer_refs(filter_expr))
        }
        TypedExprKind::ScalarSubquery { plan }
        | TypedExprKind::ArraySubquery { plan }
        | TypedExprKind::ExistsSubquery { plan, .. } => logical_plan_contains_outer_refs(plan),
        TypedExprKind::InSubquery { expr, plan, .. } => {
            expr_contains_outer_refs(expr) || logical_plan_contains_outer_refs(plan)
        }
        TypedExprKind::WindowFunction {
            args,
            partition_by,
            order_by,
            ..
        } => {
            args.iter().any(expr_contains_outer_refs)
                || partition_by.iter().any(expr_contains_outer_refs)
                || order_by
                    .iter()
                    .any(|sort| expr_contains_outer_refs(&sort.expr))
        }
        _ => false,
    }
}

pub(crate) fn logical_plan_contains_outer_refs(plan: &LogicalPlan) -> bool {
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
        } => {
            outputs
                .iter()
                .any(|output| expr_contains_outer_refs(&output.expr))
                || filter.as_ref().is_some_and(expr_contains_outer_refs)
                || order_by
                    .iter()
                    .any(|sort| expr_contains_outer_refs(&sort.expr))
                || limit.as_ref().is_some_and(expr_contains_outer_refs)
                || offset.as_ref().is_some_and(expr_contains_outer_refs)
                || distinct_on.iter().any(expr_contains_outer_refs)
        }
        LogicalPlan::ProjectSource {
            source,
            outputs,
            filter,
            order_by,
            limit,
            offset,
            distinct_on,
            ..
        } => {
            logical_plan_contains_outer_refs(source)
                || outputs
                    .iter()
                    .any(|output| expr_contains_outer_refs(&output.expr))
                || filter.as_ref().is_some_and(expr_contains_outer_refs)
                || order_by
                    .iter()
                    .any(|sort| expr_contains_outer_refs(&sort.expr))
                || limit.as_ref().is_some_and(expr_contains_outer_refs)
                || offset.as_ref().is_some_and(expr_contains_outer_refs)
                || distinct_on.iter().any(expr_contains_outer_refs)
        }
        LogicalPlan::HybridFunctionScan { args, .. } => args.iter().any(expr_contains_outer_refs),
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
        } => {
            group_by.iter().any(expr_contains_outer_refs)
                || aggregates
                    .iter()
                    .any(|output| expr_contains_outer_refs(&output.expr))
                || having.as_ref().is_some_and(expr_contains_outer_refs)
                || filter.as_ref().is_some_and(expr_contains_outer_refs)
                || order_by
                    .iter()
                    .any(|sort| expr_contains_outer_refs(&sort.expr))
                || limit.as_ref().is_some_and(expr_contains_outer_refs)
                || offset.as_ref().is_some_and(expr_contains_outer_refs)
                || distinct_on.iter().any(expr_contains_outer_refs)
        }
        LogicalPlan::AggregateSource {
            source,
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
            logical_plan_contains_outer_refs(source)
                || group_by.iter().any(expr_contains_outer_refs)
                || aggregates
                    .iter()
                    .any(|output| expr_contains_outer_refs(&output.expr))
                || having.as_ref().is_some_and(expr_contains_outer_refs)
                || filter.as_ref().is_some_and(expr_contains_outer_refs)
                || order_by
                    .iter()
                    .any(|sort| expr_contains_outer_refs(&sort.expr))
                || limit.as_ref().is_some_and(expr_contains_outer_refs)
                || offset.as_ref().is_some_and(expr_contains_outer_refs)
                || distinct_on.iter().any(expr_contains_outer_refs)
        }
        LogicalPlan::SetOperation {
            left,
            right,
            order_by,
            limit,
            offset,
            ..
        } => {
            logical_plan_contains_outer_refs(left)
                || logical_plan_contains_outer_refs(right)
                || order_by
                    .iter()
                    .any(|sort| expr_contains_outer_refs(&sort.expr))
                || limit.as_ref().is_some_and(expr_contains_outer_refs)
                || offset.as_ref().is_some_and(expr_contains_outer_refs)
        }
        LogicalPlan::ProjectValues {
            rows,
            order_by,
            limit,
            offset,
            ..
        } => {
            rows.iter().flatten().any(expr_contains_outer_refs)
                || order_by
                    .iter()
                    .any(|sort| expr_contains_outer_refs(&sort.expr))
                || limit.as_ref().is_some_and(expr_contains_outer_refs)
                || offset.as_ref().is_some_and(expr_contains_outer_refs)
        }
        LogicalPlan::NestedLoopJoin {
            left,
            right,
            condition,
            outputs,
            filter,
            order_by,
            limit,
            offset,
            distinct_on,
            ..
        } => {
            logical_plan_contains_outer_refs(left)
                || logical_plan_contains_outer_refs(right)
                || condition.as_ref().is_some_and(expr_contains_outer_refs)
                || outputs
                    .iter()
                    .any(|output| expr_contains_outer_refs(&output.expr))
                || filter.as_ref().is_some_and(expr_contains_outer_refs)
                || order_by
                    .iter()
                    .any(|sort| expr_contains_outer_refs(&sort.expr))
                || limit.as_ref().is_some_and(expr_contains_outer_refs)
                || offset.as_ref().is_some_and(expr_contains_outer_refs)
                || distinct_on.iter().any(expr_contains_outer_refs)
        }
        LogicalPlan::RecursiveCte {
            base, recursive, ..
        } => logical_plan_contains_outer_refs(base) || logical_plan_contains_outer_refs(recursive),
        _ => false,
    }
}

/// Collect the *outer-column-reference* ordinals reachable from `expr`,
/// descending through nested subquery plans. Used to derive a stable
/// cache key for correlated EXISTS / scalar-subquery memoization: the
/// subquery's value depends only on the values of these ordinals in
/// the outer row, so two outer rows that agree on those ordinals share
/// a result regardless of differences in non-correlated columns.
pub(crate) fn collect_outer_ordinals_in_expr(expr: &TypedExpr, out: &mut Vec<usize>) {
    match &expr.kind {
        TypedExprKind::OuterColumnRef { ordinal, .. } => out.push(*ordinal),
        TypedExprKind::ScalarSubquery { plan }
        | TypedExprKind::ArraySubquery { plan }
        | TypedExprKind::ExistsSubquery { plan, .. } => collect_outer_ordinals_in_plan(plan, out),
        TypedExprKind::InSubquery {
            expr: inner, plan, ..
        } => {
            collect_outer_ordinals_in_expr(inner, out);
            collect_outer_ordinals_in_plan(plan, out);
        }
        _ => {
            // Fall back to the structural visit done by
            // `expr_contains_outer_refs`: walk every child expression.
            // Reusing the same depth-first traversal through children
            // means we don't have to re-enumerate every TypedExprKind
            // variant here.
            collect_outer_ordinals_via_children(expr, out);
        }
    }
}

#[allow(clippy::enum_glob_use)]
fn collect_outer_ordinals_via_children(expr: &TypedExpr, out: &mut Vec<usize>) {
    use TypedExprKind::*;
    match &expr.kind {
        Literal(_) | ColumnRef { .. } | OuterColumnRef { .. } | NextValue { .. } => {}
        BinaryEq { left, right }
        | BinaryNe { left, right }
        | BinaryGe { left, right }
        | BinaryGt { left, right }
        | BinaryLe { left, right }
        | BinaryLt { left, right }
        | LogicalAnd { left, right }
        | LogicalOr { left, right }
        | ArithAdd { left, right }
        | ArithSub { left, right }
        | ArithMul { left, right }
        | ArithDiv { left, right }
        | ArithMod { left, right }
        | Concat { left, right }
        | JsonGet { left, right }
        | JsonGetText { left, right }
        | JsonPathGet { left, right }
        | JsonPathGetText { left, right }
        | JsonContains { left, right }
        | JsonContainedBy { left, right }
        | JsonKeyExists { left, right }
        | JsonAnyKeyExists { left, right }
        | JsonAllKeysExist { left, right }
        | ArrayConcat { left, right }
        | ArrayContains { left, right }
        | ArrayContainedBy { left, right }
        | ArrayOverlap { left, right }
        | IsDistinctFrom { left, right, .. }
        | Nullif { left, right } => {
            collect_outer_ordinals_in_expr(left, out);
            collect_outer_ordinals_in_expr(right, out);
        }
        LogicalNot { expr: inner }
        | Negate { expr: inner }
        | IsNull { expr: inner, .. }
        | Cast { expr: inner, .. } => collect_outer_ordinals_in_expr(inner, out),
        Like { expr, pattern, .. } => {
            collect_outer_ordinals_in_expr(expr, out);
            collect_outer_ordinals_in_expr(pattern, out);
        }
        InList { expr, list, .. } => {
            collect_outer_ordinals_in_expr(expr, out);
            for e in list {
                collect_outer_ordinals_in_expr(e, out);
            }
        }
        Between {
            expr, low, high, ..
        } => {
            collect_outer_ordinals_in_expr(expr, out);
            collect_outer_ordinals_in_expr(low, out);
            collect_outer_ordinals_in_expr(high, out);
        }
        CaseWhen {
            conditions,
            results,
            else_result,
        } => {
            for e in conditions {
                collect_outer_ordinals_in_expr(e, out);
            }
            for e in results {
                collect_outer_ordinals_in_expr(e, out);
            }
            if let Some(e) = else_result {
                collect_outer_ordinals_in_expr(e, out);
            }
        }
        Coalesce { args }
        | ScalarFunction { args, .. }
        | ArrayConstruct { elements: args }
        | UserFunction { args, .. } => {
            for e in args {
                collect_outer_ordinals_in_expr(e, out);
            }
        }
        AggCount { expr, filter, .. } => {
            if let Some(e) = expr {
                collect_outer_ordinals_in_expr(e, out);
            }
            if let Some(e) = filter {
                collect_outer_ordinals_in_expr(e, out);
            }
        }
        AggSum { expr, filter, .. }
        | AggAvg { expr, filter, .. }
        | AggAnyValue { expr, filter }
        | AggMin { expr, filter }
        | AggMax { expr, filter }
        | AggBoolAnd { expr, filter }
        | AggBoolOr { expr, filter }
        | AggStddevPop { expr, filter }
        | AggStddevSamp { expr, filter }
        | AggVarPop { expr, filter }
        | AggVarSamp { expr, filter } => {
            collect_outer_ordinals_in_expr(expr, out);
            if let Some(e) = filter {
                collect_outer_ordinals_in_expr(e, out);
            }
        }
        AggStringAgg {
            expr,
            delimiter,
            filter,
            ..
        } => {
            collect_outer_ordinals_in_expr(expr, out);
            collect_outer_ordinals_in_expr(delimiter, out);
            if let Some(e) = filter {
                collect_outer_ordinals_in_expr(e, out);
            }
        }
        AggArrayAgg { expr, filter, .. } => {
            collect_outer_ordinals_in_expr(expr, out);
            if let Some(e) = filter {
                collect_outer_ordinals_in_expr(e, out);
            }
        }
        ScalarSubquery { plan } | ArraySubquery { plan } | ExistsSubquery { plan, .. } => {
            collect_outer_ordinals_in_plan(plan, out)
        }
        InSubquery {
            expr: inner, plan, ..
        } => {
            collect_outer_ordinals_in_expr(inner, out);
            collect_outer_ordinals_in_plan(plan, out);
        }
        WindowFunction {
            args,
            partition_by,
            order_by,
            ..
        } => {
            for e in args {
                collect_outer_ordinals_in_expr(e, out);
            }
            for e in partition_by {
                collect_outer_ordinals_in_expr(e, out);
            }
            for s in order_by {
                collect_outer_ordinals_in_expr(&s.expr, out);
            }
        }
    }
}

pub(crate) fn collect_outer_ordinals_in_plan(plan: &LogicalPlan, out: &mut Vec<usize>) {
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
        } => {
            for o in outputs {
                collect_outer_ordinals_in_expr(&o.expr, out);
            }
            if let Some(e) = filter {
                collect_outer_ordinals_in_expr(e, out);
            }
            for s in order_by {
                collect_outer_ordinals_in_expr(&s.expr, out);
            }
            if let Some(e) = limit {
                collect_outer_ordinals_in_expr(e, out);
            }
            if let Some(e) = offset {
                collect_outer_ordinals_in_expr(e, out);
            }
            for e in distinct_on {
                collect_outer_ordinals_in_expr(e, out);
            }
        }
        LogicalPlan::ProjectSource {
            source,
            outputs,
            filter,
            order_by,
            limit,
            offset,
            distinct_on,
            ..
        } => {
            collect_outer_ordinals_in_plan(source, out);
            for o in outputs {
                collect_outer_ordinals_in_expr(&o.expr, out);
            }
            if let Some(e) = filter {
                collect_outer_ordinals_in_expr(e, out);
            }
            for s in order_by {
                collect_outer_ordinals_in_expr(&s.expr, out);
            }
            if let Some(e) = limit {
                collect_outer_ordinals_in_expr(e, out);
            }
            if let Some(e) = offset {
                collect_outer_ordinals_in_expr(e, out);
            }
            for e in distinct_on {
                collect_outer_ordinals_in_expr(e, out);
            }
        }
        LogicalPlan::HybridFunctionScan { args, .. } => {
            for e in args {
                collect_outer_ordinals_in_expr(e, out);
            }
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
        } => {
            for e in group_by {
                collect_outer_ordinals_in_expr(e, out);
            }
            for o in aggregates {
                collect_outer_ordinals_in_expr(&o.expr, out);
            }
            if let Some(e) = having {
                collect_outer_ordinals_in_expr(e, out);
            }
            if let Some(e) = filter {
                collect_outer_ordinals_in_expr(e, out);
            }
            for s in order_by {
                collect_outer_ordinals_in_expr(&s.expr, out);
            }
            if let Some(e) = limit {
                collect_outer_ordinals_in_expr(e, out);
            }
            if let Some(e) = offset {
                collect_outer_ordinals_in_expr(e, out);
            }
            for e in distinct_on {
                collect_outer_ordinals_in_expr(e, out);
            }
        }
        LogicalPlan::AggregateSource {
            source,
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
            collect_outer_ordinals_in_plan(source, out);
            for e in group_by {
                collect_outer_ordinals_in_expr(e, out);
            }
            for o in aggregates {
                collect_outer_ordinals_in_expr(&o.expr, out);
            }
            if let Some(e) = having {
                collect_outer_ordinals_in_expr(e, out);
            }
            if let Some(e) = filter {
                collect_outer_ordinals_in_expr(e, out);
            }
            for s in order_by {
                collect_outer_ordinals_in_expr(&s.expr, out);
            }
            if let Some(e) = limit {
                collect_outer_ordinals_in_expr(e, out);
            }
            if let Some(e) = offset {
                collect_outer_ordinals_in_expr(e, out);
            }
            for e in distinct_on {
                collect_outer_ordinals_in_expr(e, out);
            }
        }
        LogicalPlan::SetOperation {
            left,
            right,
            order_by,
            limit,
            offset,
            ..
        } => {
            collect_outer_ordinals_in_plan(left, out);
            collect_outer_ordinals_in_plan(right, out);
            for s in order_by {
                collect_outer_ordinals_in_expr(&s.expr, out);
            }
            if let Some(e) = limit {
                collect_outer_ordinals_in_expr(e, out);
            }
            if let Some(e) = offset {
                collect_outer_ordinals_in_expr(e, out);
            }
        }
        LogicalPlan::ProjectValues {
            rows,
            order_by,
            limit,
            offset,
            ..
        } => {
            for r in rows {
                for e in r {
                    collect_outer_ordinals_in_expr(e, out);
                }
            }
            for s in order_by {
                collect_outer_ordinals_in_expr(&s.expr, out);
            }
            if let Some(e) = limit {
                collect_outer_ordinals_in_expr(e, out);
            }
            if let Some(e) = offset {
                collect_outer_ordinals_in_expr(e, out);
            }
        }
        LogicalPlan::NestedLoopJoin {
            left,
            right,
            condition,
            outputs,
            filter,
            order_by,
            limit,
            offset,
            distinct_on,
            ..
        } => {
            collect_outer_ordinals_in_plan(left, out);
            collect_outer_ordinals_in_plan(right, out);
            if let Some(e) = condition {
                collect_outer_ordinals_in_expr(e, out);
            }
            for o in outputs {
                collect_outer_ordinals_in_expr(&o.expr, out);
            }
            if let Some(e) = filter {
                collect_outer_ordinals_in_expr(e, out);
            }
            for s in order_by {
                collect_outer_ordinals_in_expr(&s.expr, out);
            }
            if let Some(e) = limit {
                collect_outer_ordinals_in_expr(e, out);
            }
            if let Some(e) = offset {
                collect_outer_ordinals_in_expr(e, out);
            }
            for e in distinct_on {
                collect_outer_ordinals_in_expr(e, out);
            }
        }
        LogicalPlan::RecursiveCte {
            base, recursive, ..
        } => {
            collect_outer_ordinals_in_plan(base, out);
            collect_outer_ordinals_in_plan(recursive, out);
        }
        _ => {}
    }
}

pub(crate) fn rewrite_positional_params(
    expr: aiondb_parser::Expr,
    params: &[(String, aiondb_core::DataType)],
) -> aiondb_parser::Expr {
    use aiondb_parser::{Expr, ObjectName};

    match expr {
        Expr::Parameter { index, span } => {
            if index >= 1 && index <= params.len() {
                let name = &params[index - 1].0;
                let resolved_name = if name.is_empty() {
                    format!("__p{index}")
                } else {
                    name.clone()
                };
                return Expr::Identifier(ObjectName {
                    parts: vec![resolved_name],
                    span,
                });
            }
            Expr::Parameter { index, span }
        }
        Expr::UnaryOp {
            op,
            expr: inner,
            span,
        } => Expr::UnaryOp {
            op,
            expr: Box::new(rewrite_positional_params(*inner, params)),
            span,
        },
        Expr::BinaryOp {
            left,
            op,
            right,
            span,
        } => Expr::BinaryOp {
            left: Box::new(rewrite_positional_params(*left, params)),
            op,
            right: Box::new(rewrite_positional_params(*right, params)),
            span,
        },
        Expr::IsNull {
            expr: inner,
            negated,
            span,
        } => Expr::IsNull {
            expr: Box::new(rewrite_positional_params(*inner, params)),
            negated,
            span,
        },
        Expr::Between {
            expr: inner,
            low,
            high,
            negated,
            span,
        } => Expr::Between {
            expr: Box::new(rewrite_positional_params(*inner, params)),
            low: Box::new(rewrite_positional_params(*low, params)),
            high: Box::new(rewrite_positional_params(*high, params)),
            negated,
            span,
        },
        Expr::Cast {
            expr: inner,
            data_type,
            span,
        } => Expr::Cast {
            expr: Box::new(rewrite_positional_params(*inner, params)),
            data_type,
            span,
        },
        Expr::FunctionCall {
            name,
            args,
            distinct,
            filter,
            span,
        } => Expr::FunctionCall {
            name,
            args: args
                .into_iter()
                .map(|a| rewrite_positional_params(a, params))
                .collect(),
            distinct,
            filter: filter.map(|f| Box::new(rewrite_positional_params(*f, params))),
            span,
        },
        Expr::InList {
            expr: inner,
            list,
            negated,
            span,
        } => Expr::InList {
            expr: Box::new(rewrite_positional_params(*inner, params)),
            list: list
                .into_iter()
                .map(|e| rewrite_positional_params(e, params))
                .collect(),
            negated,
            span,
        },
        Expr::CaseWhen {
            operand,
            conditions,
            results,
            else_result,
            span,
        } => Expr::CaseWhen {
            operand: operand.map(|o| Box::new(rewrite_positional_params(*o, params))),
            conditions: conditions
                .into_iter()
                .map(|c| rewrite_positional_params(c, params))
                .collect(),
            results: results
                .into_iter()
                .map(|r| rewrite_positional_params(r, params))
                .collect(),
            else_result: else_result.map(|e| Box::new(rewrite_positional_params(*e, params))),
            span,
        },
        other => other,
    }
}

pub(crate) fn synthetic_relation_for_params(
    params: &[(String, aiondb_core::DataType)],
) -> aiondb_catalog::TableDescriptor {
    use aiondb_catalog::{ColumnDescriptor, QualifiedName, TableDescriptor};
    use aiondb_core::{ColumnId, RelationId, SchemaId};

    TableDescriptor {
        table_id: RelationId::default(),
        schema_id: SchemaId::default(),
        name: QualifiedName::new(None::<String>, "__fn_params__"),
        columns: params
            .iter()
            .enumerate()
            .map(|(i, (name, data_type))| {
                let col_name = if name.is_empty() {
                    format!("__p{}", i + 1)
                } else {
                    name.clone()
                };
                ColumnDescriptor {
                    column_id: ColumnId::new(u64::try_from(i).unwrap_or(u64::MAX)),
                    name: col_name,
                    data_type: data_type.clone(),
                    raw_type_name: None,
                    text_type_modifier: None,
                    nullable: true,
                    ordinal_position: u32::try_from(i.saturating_add(1)).unwrap_or(u32::MAX),
                    default_value: None,
                }
            })
            .collect(),
        identity_columns: Vec::new(),
        primary_key: None,
        foreign_keys: Vec::new(),
        check_constraints: Vec::new(),
        shard_config: None,
        owner: None,
    }
}
