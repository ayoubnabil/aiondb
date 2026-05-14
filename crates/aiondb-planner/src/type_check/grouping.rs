use aiondb_catalog::{ColumnDescriptor, TableDescriptor};
use aiondb_core::{ColumnId, DataType, DbError, DbResult, ErrorReport, SqlState, Value};
use aiondb_eval::current_search_path_schemas;
use aiondb_parser::*;
use aiondb_plan::*;

use super::aggregates::is_aggregate_function_name;
use super::expr::infer_expr;
use super::params;
use super::support::*;
use super::typed::TypedSetBranch;
use crate::binder::{BoundJoin, BoundOrderBy, BoundSelect};

pub(super) fn validate_scalar_group_expressions(
    select: &BoundSelect,
    outputs: &[ProjectionExpr],
    having: Option<&TypedExpr>,
    order_by: &[SortExpr],
) -> DbResult<()> {
    if !select.group_by.is_empty() || !select_uses_scalar_group(select, outputs, having, order_by) {
        return Ok(());
    }

    for projection in &select.projections {
        if let Some(name) = find_scalar_group_ungrouped_column(&projection.expr) {
            return Err(grouping_column_error(select, name));
        }
    }
    if let Some(expr) = &select.having {
        if let Some(name) = find_scalar_group_ungrouped_column(expr) {
            return Err(grouping_column_error(select, name));
        }
    }
    for item in &select.order_by {
        if let Some(name) = find_scalar_group_ungrouped_column(&item.expr) {
            return Err(grouping_column_error(select, name));
        }
    }
    Ok(())
}

pub(super) fn validate_aggregate_grouping(
    select: &BoundSelect,
    outputs: &[ProjectionExpr],
    group_by: &[TypedExpr],
    having: Option<&TypedExpr>,
    order_by: &[SortExpr],
) -> DbResult<()> {
    if select.group_by.is_empty() {
        return Ok(());
    }

    // Build an extended group_by list that includes functionally dependent columns.
    // If the table has a primary key and all PK columns appear in group_by,
    // then all table columns are considered grouped (PostgreSQL functional dependency).
    let extended_group_by: Vec<TypedExpr>;
    let effective_group_by: &[TypedExpr] = if let Some(ref table) = select.relation {
        if let Some(ref pk_col_ids) = table.primary_key {
            if !pk_col_ids.is_empty() && pk_covers_group_by(table, pk_col_ids, group_by) {
                let mut ext = group_by.to_vec();
                for (idx, col) in table.columns.iter().enumerate() {
                    let col_ref = TypedExpr {
                        kind: TypedExprKind::ColumnRef {
                            name: col.name.clone(),
                            ordinal: idx,
                        },
                        data_type: col.data_type.clone(),
                        nullable: col.nullable,
                    };
                    if !ext.iter().any(|e| e == &col_ref) {
                        let qualified_ref = TypedExpr {
                            kind: TypedExprKind::ColumnRef {
                                name: format!("{}\0{}", table.name.object_name(), col.name),
                                ordinal: idx,
                            },
                            data_type: col.data_type.clone(),
                            nullable: col.nullable,
                        };
                        if !ext.iter().any(|e| e == &qualified_ref) {
                            ext.push(col_ref);
                            ext.push(qualified_ref);
                        }
                    }
                }
                extended_group_by = ext;
                &extended_group_by
            } else {
                group_by
            }
        } else {
            group_by
        }
    } else {
        group_by
    };

    for (projection, typed_projection) in select.projections.iter().zip(outputs) {
        if typed_expr_has_ungrouped_column(&typed_projection.expr, effective_group_by) {
            return match find_scalar_group_ungrouped_column(&projection.expr) {
                Some(name) => Err(grouping_column_error(select, name)),
                None => Err(ungrouped_column_fallback_error()),
            };
        }
    }
    if let (Some(expr), Some(typed_expr)) = (select.having.as_ref(), having) {
        if typed_expr_has_ungrouped_column(typed_expr, effective_group_by) {
            return match find_scalar_group_ungrouped_column(expr) {
                Some(name) => Err(grouping_column_error(select, name)),
                None => Err(ungrouped_column_fallback_error()),
            };
        }
    }
    for (item, typed_item) in select.order_by.iter().zip(order_by) {
        if typed_expr_has_ungrouped_column(&typed_item.expr, effective_group_by) {
            return match find_scalar_group_ungrouped_column(&item.expr) {
                Some(name) => Err(grouping_column_error(select, name)),
                None => Err(ungrouped_column_fallback_error()),
            };
        }
    }
    Ok(())
}

/// Check if all primary key columns appear in the `group_by` expressions.
pub(super) fn pk_covers_group_by(
    table: &TableDescriptor,
    pk_col_ids: &[ColumnId],
    group_by: &[TypedExpr],
) -> bool {
    let col_map: std::collections::HashMap<ColumnId, &str> = table
        .columns
        .iter()
        .map(|c| (c.column_id, c.name.as_str()))
        .collect();
    let gb_names: std::collections::HashSet<String> = group_by
        .iter()
        .filter_map(|gb| {
            if let TypedExprKind::ColumnRef { name, .. } = &gb.kind {
                Some(
                    name.split('\0')
                        .next_back()
                        .unwrap_or(name)
                        .to_ascii_lowercase(),
                )
            } else {
                None
            }
        })
        .collect();
    pk_col_ids.iter().all(|pk_id| {
        col_map
            .get(pk_id)
            .is_some_and(|pk_name| gb_names.contains(&pk_name.to_ascii_lowercase()))
    })
}

pub(super) fn select_uses_scalar_group(
    select: &BoundSelect,
    outputs: &[ProjectionExpr],
    having: Option<&TypedExpr>,
    order_by: &[SortExpr],
) -> bool {
    select.having.is_some()
        || outputs
            .iter()
            .any(|projection| typed_expr_contains_aggregate(&projection.expr))
        || having.is_some_and(typed_expr_contains_aggregate)
        || order_by
            .iter()
            .any(|item| typed_expr_contains_aggregate(&item.expr))
}

pub(super) fn grouping_column_error(select: &BoundSelect, name: &ObjectName) -> DbError {
    let column_name = format_grouping_column_name(select, name);
    DbError::Bind(Box::new(
        ErrorReport::new(
            SqlState::SyntaxError,
            format!(
                "column \"{column_name}\" must appear in the GROUP BY clause or be used in an aggregate function"
            ),
        )
        .with_position(name.span.start + 1),
    ))
}

pub(super) fn ungrouped_column_fallback_error() -> DbError {
    DbError::Bind(Box::new(ErrorReport::new(
        SqlState::SyntaxError,
        "expression must appear in the GROUP BY clause or be used in an aggregate function"
            .to_owned(),
    )))
}

pub(super) fn format_grouping_column_name(select: &BoundSelect, name: &ObjectName) -> String {
    match name.parts.as_slice() {
        [column_name] => select.relation.as_ref().map_or_else(
            || column_name.clone(),
            |relation| format!("{}.{}", relation.name.object_name(), column_name),
        ),
        _ => name.parts.join("."),
    }
}

pub(super) fn remap_group_by_ambiguity_to_order_by(
    err: DbError,
    group_by_expr: &Expr,
    order_by: &[BoundOrderBy],
) -> DbError {
    let is_ambiguous_column = err.sqlstate() == SqlState::SyntaxError
        && err.report().message.starts_with("column reference \"")
        && err.report().message.ends_with("\" is ambiguous");
    if !is_ambiguous_column {
        return err;
    }

    if let Some(order_item) = order_by
        .iter()
        .find(|item| exprs_match_ignoring_span(&item.expr, group_by_expr))
    {
        return err.with_position(order_item.expr.span().start + 1);
    }

    err
}

pub(super) fn exprs_match_ignoring_span(left: &Expr, right: &Expr) -> bool {
    match (left, right) {
        (Expr::Literal(left_value, _), Expr::Literal(right_value, _)) => left_value == right_value,
        (Expr::Identifier(left_name), Expr::Identifier(right_name)) => {
            left_name.parts == right_name.parts
        }
        (
            Expr::Parameter {
                index: left_index, ..
            },
            Expr::Parameter {
                index: right_index, ..
            },
        ) => left_index == right_index,
        (Expr::Default { .. }, Expr::Default { .. }) => true,
        (
            Expr::FunctionCall {
                name: left_name,
                args: left_args,
                distinct: left_distinct,
                filter: left_filter,
                ..
            },
            Expr::FunctionCall {
                name: right_name,
                args: right_args,
                distinct: right_distinct,
                filter: right_filter,
                ..
            },
        ) => {
            left_name.parts == right_name.parts
                && left_distinct == right_distinct
                && expr_lists_match_ignoring_span(left_args, right_args)
                && option_box_exprs_match_ignoring_span(
                    left_filter.as_deref(),
                    right_filter.as_deref(),
                )
        }
        (
            Expr::UnaryOp {
                op: left_op,
                expr: left_expr,
                ..
            },
            Expr::UnaryOp {
                op: right_op,
                expr: right_expr,
                ..
            },
        ) => left_op == right_op && exprs_match_ignoring_span(left_expr, right_expr),
        (
            Expr::BinaryOp {
                left: left_left,
                op: left_op,
                right: left_right,
                ..
            },
            Expr::BinaryOp {
                left: right_left,
                op: right_op,
                right: right_right,
                ..
            },
        ) => {
            left_op == right_op
                && exprs_match_ignoring_span(left_left, right_left)
                && exprs_match_ignoring_span(left_right, right_right)
        }
        (
            Expr::IsNull {
                expr: left_expr,
                negated: left_negated,
                ..
            },
            Expr::IsNull {
                expr: right_expr,
                negated: right_negated,
                ..
            },
        ) => left_negated == right_negated && exprs_match_ignoring_span(left_expr, right_expr),
        (
            Expr::IsDistinctFrom {
                left: left_left,
                right: left_right,
                negated: left_negated,
                ..
            },
            Expr::IsDistinctFrom {
                left: right_left,
                right: right_right,
                negated: right_negated,
                ..
            },
        ) => {
            left_negated == right_negated
                && exprs_match_ignoring_span(left_left, right_left)
                && exprs_match_ignoring_span(left_right, right_right)
        }
        (
            Expr::Like {
                expr: left_expr,
                pattern: left_pattern,
                negated: left_negated,
                case_insensitive: left_case_insensitive,
                ..
            },
            Expr::Like {
                expr: right_expr,
                pattern: right_pattern,
                negated: right_negated,
                case_insensitive: right_case_insensitive,
                ..
            },
        ) => {
            left_negated == right_negated
                && left_case_insensitive == right_case_insensitive
                && exprs_match_ignoring_span(left_expr, right_expr)
                && exprs_match_ignoring_span(left_pattern, right_pattern)
        }
        (
            Expr::InList {
                expr: left_expr,
                list: left_list,
                negated: left_negated,
                ..
            },
            Expr::InList {
                expr: right_expr,
                list: right_list,
                negated: right_negated,
                ..
            },
        ) => {
            left_negated == right_negated
                && exprs_match_ignoring_span(left_expr, right_expr)
                && expr_lists_match_ignoring_span(left_list, right_list)
        }
        (
            Expr::Between {
                expr: left_expr,
                low: left_low,
                high: left_high,
                negated: left_negated,
                ..
            },
            Expr::Between {
                expr: right_expr,
                low: right_low,
                high: right_high,
                negated: right_negated,
                ..
            },
        ) => {
            left_negated == right_negated
                && exprs_match_ignoring_span(left_expr, right_expr)
                && exprs_match_ignoring_span(left_low, right_low)
                && exprs_match_ignoring_span(left_high, right_high)
        }
        (
            Expr::Cast {
                expr: left_expr,
                data_type: left_type,
                ..
            },
            Expr::Cast {
                expr: right_expr,
                data_type: right_type,
                ..
            },
        ) => left_type == right_type && exprs_match_ignoring_span(left_expr, right_expr),
        (
            Expr::CaseWhen {
                operand: left_operand,
                conditions: left_conditions,
                results: left_results,
                else_result: left_else,
                ..
            },
            Expr::CaseWhen {
                operand: right_operand,
                conditions: right_conditions,
                results: right_results,
                else_result: right_else,
                ..
            },
        ) => {
            option_box_exprs_match_ignoring_span(left_operand.as_deref(), right_operand.as_deref())
                && expr_lists_match_ignoring_span(left_conditions, right_conditions)
                && expr_lists_match_ignoring_span(left_results, right_results)
                && option_box_exprs_match_ignoring_span(left_else.as_deref(), right_else.as_deref())
        }
        (
            Expr::Array {
                elements: left_elements,
                ..
            },
            Expr::Array {
                elements: right_elements,
                ..
            },
        ) => expr_lists_match_ignoring_span(left_elements, right_elements),
        _ => false,
    }
}

pub(super) fn expr_lists_match_ignoring_span(left: &[Expr], right: &[Expr]) -> bool {
    left.len() == right.len()
        && left
            .iter()
            .zip(right)
            .all(|(left_expr, right_expr)| exprs_match_ignoring_span(left_expr, right_expr))
}

pub(super) fn option_box_exprs_match_ignoring_span(
    left: Option<&Expr>,
    right: Option<&Expr>,
) -> bool {
    match (left, right) {
        (Some(left_expr), Some(right_expr)) => exprs_match_ignoring_span(left_expr, right_expr),
        (None, None) => true,
        _ => false,
    }
}

pub(super) fn find_scalar_group_ungrouped_column(expr: &Expr) -> Option<&ObjectName> {
    find_ungrouped_column(expr, &[])
}

pub(super) fn typed_expr_has_ungrouped_column(
    expr: &TypedExpr,
    grouped_exprs: &[TypedExpr],
) -> bool {
    if let TypedExprKind::ColumnRef { ordinal, .. } = &expr.kind {
        if grouped_exprs.iter().any(|grouped_expr| {
            matches!(
                &grouped_expr.kind,
                TypedExprKind::ColumnRef {
                    ordinal: grouped_ordinal,
                    ..
                } if grouped_ordinal == ordinal
            )
        }) {
            return false;
        }
    }
    if grouped_exprs
        .iter()
        .any(|grouped_expr| grouped_expr == expr)
    {
        return false;
    }

    match &expr.kind {
        TypedExprKind::Literal(_)
        | TypedExprKind::NextValue { .. }
        | TypedExprKind::ScalarSubquery { .. }
        | TypedExprKind::ArraySubquery { .. }
        | TypedExprKind::ExistsSubquery { .. } => false,
        TypedExprKind::ColumnRef { .. } => true,
        // Outer column references are constants from the inner query's
        // perspective and do not need to appear in GROUP BY.
        TypedExprKind::OuterColumnRef { .. } => false,
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
            typed_expr_has_ungrouped_column(left, grouped_exprs)
                || typed_expr_has_ungrouped_column(right, grouped_exprs)
        }
        TypedExprKind::LogicalNot { expr }
        | TypedExprKind::Negate { expr }
        | TypedExprKind::IsNull { expr, .. }
        | TypedExprKind::Cast { expr, .. } => typed_expr_has_ungrouped_column(expr, grouped_exprs),
        TypedExprKind::Like { expr, pattern, .. } => {
            typed_expr_has_ungrouped_column(expr, grouped_exprs)
                || typed_expr_has_ungrouped_column(pattern, grouped_exprs)
        }
        TypedExprKind::InList { expr, list, .. } => {
            typed_expr_has_ungrouped_column(expr, grouped_exprs)
                || list
                    .iter()
                    .any(|item| typed_expr_has_ungrouped_column(item, grouped_exprs))
        }
        TypedExprKind::Between {
            expr, low, high, ..
        } => {
            typed_expr_has_ungrouped_column(expr, grouped_exprs)
                || typed_expr_has_ungrouped_column(low, grouped_exprs)
                || typed_expr_has_ungrouped_column(high, grouped_exprs)
        }
        TypedExprKind::CaseWhen {
            conditions,
            results,
            else_result,
        } => {
            conditions
                .iter()
                .any(|expr| typed_expr_has_ungrouped_column(expr, grouped_exprs))
                || results
                    .iter()
                    .any(|expr| typed_expr_has_ungrouped_column(expr, grouped_exprs))
                || else_result
                    .as_deref()
                    .is_some_and(|expr| typed_expr_has_ungrouped_column(expr, grouped_exprs))
        }
        TypedExprKind::ScalarFunction {
            func: ScalarFunction::Generic(ref name),
            ..
        } if name == "grouping" => {
            // The grouping() function is allowed to reference any group-by
            // column regardless of grouping set membership.
            false
        }
        TypedExprKind::Coalesce { args }
        | TypedExprKind::ScalarFunction { args, .. }
        | TypedExprKind::UserFunction { args, .. }
        | TypedExprKind::ArrayConstruct { elements: args } => args
            .iter()
            .any(|expr| typed_expr_has_ungrouped_column(expr, grouped_exprs)),
        TypedExprKind::AggCount { .. }
        | TypedExprKind::AggSum { .. }
        | TypedExprKind::AggAvg { .. }
        | TypedExprKind::AggAnyValue { .. }
        | TypedExprKind::AggMin { .. }
        | TypedExprKind::AggMax { .. }
        | TypedExprKind::AggStringAgg { .. }
        | TypedExprKind::AggArrayAgg { .. }
        | TypedExprKind::AggBoolAnd { .. }
        | TypedExprKind::AggBoolOr { .. }
        | TypedExprKind::AggStddevPop { .. }
        | TypedExprKind::AggStddevSamp { .. }
        | TypedExprKind::AggVarPop { .. }
        | TypedExprKind::AggVarSamp { .. } => false,
        TypedExprKind::InSubquery { expr, .. } => {
            typed_expr_has_ungrouped_column(expr, grouped_exprs)
        }
        TypedExprKind::WindowFunction {
            args,
            partition_by,
            order_by,
            ..
        } => {
            args.iter()
                .any(|expr| typed_expr_has_ungrouped_column(expr, grouped_exprs))
                || partition_by
                    .iter()
                    .any(|expr| typed_expr_has_ungrouped_column(expr, grouped_exprs))
                || order_by
                    .iter()
                    .any(|item| typed_expr_has_ungrouped_column(&item.expr, grouped_exprs))
        }
    }
}

pub(super) fn find_ungrouped_column<'a>(
    expr: &'a Expr,
    grouped_exprs: &[Expr],
) -> Option<&'a ObjectName> {
    if grouped_exprs
        .iter()
        .any(|grouped_expr| grouped_expr == expr)
    {
        return None;
    }

    match expr {
        Expr::Identifier(name) => {
            if name.parts.len() == 1 && name.parts[0] == "*" {
                None
            } else {
                Some(name)
            }
        }
        Expr::FunctionCall {
            name, args, filter, ..
        } => {
            if name
                .parts
                .last()
                .is_some_and(|part| is_aggregate_function_name(part) || part == "grouping")
            {
                return None;
            }
            args.iter()
                .find_map(|arg| find_ungrouped_column(arg, grouped_exprs))
                .or_else(|| {
                    filter
                        .as_deref()
                        .and_then(|expr| find_ungrouped_column(expr, grouped_exprs))
                })
        }
        Expr::UnaryOp { expr, .. } | Expr::IsNull { expr, .. } | Expr::Cast { expr, .. } => {
            find_ungrouped_column(expr, grouped_exprs)
        }
        Expr::BinaryOp { left, right, .. }
        | Expr::IsDistinctFrom { left, right, .. }
        | Expr::Like {
            expr: left,
            pattern: right,
            ..
        } => find_ungrouped_column(left, grouped_exprs)
            .or_else(|| find_ungrouped_column(right, grouped_exprs)),
        Expr::InList { expr, list, .. } => {
            find_ungrouped_column(expr, grouped_exprs).or_else(|| {
                list.iter()
                    .find_map(|expr| find_ungrouped_column(expr, grouped_exprs))
            })
        }
        Expr::Between {
            expr, low, high, ..
        } => find_ungrouped_column(expr, grouped_exprs)
            .or_else(|| find_ungrouped_column(low, grouped_exprs))
            .or_else(|| find_ungrouped_column(high, grouped_exprs)),
        Expr::CaseWhen {
            operand,
            conditions,
            results,
            else_result,
            ..
        } => operand
            .as_deref()
            .and_then(|expr| find_ungrouped_column(expr, grouped_exprs))
            .or_else(|| {
                conditions
                    .iter()
                    .find_map(|expr| find_ungrouped_column(expr, grouped_exprs))
            })
            .or_else(|| {
                results
                    .iter()
                    .find_map(|expr| find_ungrouped_column(expr, grouped_exprs))
            })
            .or_else(|| {
                else_result
                    .as_deref()
                    .and_then(|expr| find_ungrouped_column(expr, grouped_exprs))
            }),
        Expr::Array { elements, .. } => elements
            .iter()
            .find_map(|expr| find_ungrouped_column(expr, grouped_exprs)),
        Expr::WindowFunction {
            function,
            partition_by,
            order_by,
            ..
        } => find_ungrouped_column(function, grouped_exprs)
            .or_else(|| {
                partition_by
                    .iter()
                    .find_map(|expr| find_ungrouped_column(expr, grouped_exprs))
            })
            .or_else(|| {
                order_by
                    .iter()
                    .find_map(|item| find_ungrouped_column(&item.expr, grouped_exprs))
            }),
        Expr::Literal(_, _)
        | Expr::Parameter { .. }
        | Expr::Default { .. }
        | Expr::ArraySubquery { .. }
        | Expr::Subquery { .. }
        | Expr::Exists { .. }
        | Expr::CypherExists { .. }
        | Expr::CypherPatternComprehension { .. } => None,
        Expr::InSubquery { expr, .. } => find_ungrouped_column(expr, grouped_exprs),
    }
}

pub(super) fn typed_expr_contains_aggregate(expr: &TypedExpr) -> bool {
    match &expr.kind {
        TypedExprKind::AggCount { .. }
        | TypedExprKind::AggSum { .. }
        | TypedExprKind::AggAvg { .. }
        | TypedExprKind::AggAnyValue { .. }
        | TypedExprKind::AggMin { .. }
        | TypedExprKind::AggMax { .. }
        | TypedExprKind::AggStringAgg { .. }
        | TypedExprKind::AggArrayAgg { .. }
        | TypedExprKind::AggBoolAnd { .. }
        | TypedExprKind::AggBoolOr { .. }
        | TypedExprKind::AggStddevPop { .. }
        | TypedExprKind::AggStddevSamp { .. }
        | TypedExprKind::AggVarPop { .. }
        | TypedExprKind::AggVarSamp { .. } => true,
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
        | TypedExprKind::Nullif { left, right }
        | TypedExprKind::IsDistinctFrom { left, right, .. }
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
        | TypedExprKind::ArrayOverlap { left, right } => {
            typed_expr_contains_aggregate(left) || typed_expr_contains_aggregate(right)
        }
        TypedExprKind::LogicalNot { expr }
        | TypedExprKind::Negate { expr }
        | TypedExprKind::IsNull { expr, .. }
        | TypedExprKind::Cast { expr, .. } => typed_expr_contains_aggregate(expr),
        TypedExprKind::Like { expr, pattern, .. } => {
            typed_expr_contains_aggregate(expr) || typed_expr_contains_aggregate(pattern)
        }
        TypedExprKind::InList { expr, list, .. } => {
            typed_expr_contains_aggregate(expr) || list.iter().any(typed_expr_contains_aggregate)
        }
        TypedExprKind::Between {
            expr, low, high, ..
        } => {
            typed_expr_contains_aggregate(expr)
                || typed_expr_contains_aggregate(low)
                || typed_expr_contains_aggregate(high)
        }
        TypedExprKind::CaseWhen {
            conditions,
            results,
            else_result,
        } => {
            conditions.iter().any(typed_expr_contains_aggregate)
                || results.iter().any(typed_expr_contains_aggregate)
                || else_result
                    .as_ref()
                    .is_some_and(|expr| typed_expr_contains_aggregate(expr))
        }
        TypedExprKind::Coalesce { args }
        | TypedExprKind::ScalarFunction { args, .. }
        | TypedExprKind::ArrayConstruct { elements: args }
        | TypedExprKind::UserFunction { args, .. }
        | TypedExprKind::WindowFunction { args, .. } => {
            args.iter().any(typed_expr_contains_aggregate)
        }
        TypedExprKind::InSubquery { expr, .. } => typed_expr_contains_aggregate(expr),
        TypedExprKind::Literal(_)
        | TypedExprKind::ColumnRef { .. }
        | TypedExprKind::OuterColumnRef { .. }
        | TypedExprKind::NextValue { .. }
        | TypedExprKind::ScalarSubquery { .. }
        | TypedExprKind::ArraySubquery { .. }
        | TypedExprKind::ExistsSubquery { .. } => false,
    }
}

pub(super) fn merge_outer_scope_columns(
    mut inherited: Vec<ColumnDescriptor>,
    extra: Vec<ColumnDescriptor>,
) -> Vec<ColumnDescriptor> {
    for column in extra {
        if !inherited
            .iter()
            .any(|existing| existing.name.eq_ignore_ascii_case(&column.name))
        {
            inherited.push(column);
        }
    }
    inherited
}

pub(super) fn build_join_outer_scope_columns(
    primary_relation: Option<&TableDescriptor>,
    primary_alias: Option<&str>,
    joins: &[BoundJoin],
) -> Vec<ColumnDescriptor> {
    let mut columns = Vec::new();
    let mut alias_entries: Vec<(String, usize)> = Vec::new();
    let mut using_alias_entries: Vec<(String, Vec<ColumnDescriptor>)> = Vec::new();

    if let Some(relation) = primary_relation {
        let relation = compat_relation_with_system_columns(relation);
        let relation_name = relation.name.object_name().to_owned();
        alias_entries.push((relation_name.clone(), columns.len()));
        if let Some(alias) = primary_alias.map(str::to_owned) {
            if !alias.eq_ignore_ascii_case(&relation_name) {
                alias_entries.push((alias, columns.len()));
            }
        }
        append_join_scope_columns(&mut columns, &relation.columns);
    }

    for join in joins {
        let relation = compat_relation_with_system_columns(&join.relation);
        if let Some(using_alias) = &join.using_alias {
            let using_columns = join
                .using_columns
                .iter()
                .filter_map(|column_name| {
                    columns
                        .iter()
                        .find(|column| column.name.eq_ignore_ascii_case(column_name))
                        .cloned()
                })
                .collect::<Vec<_>>();
            if !using_columns.is_empty() {
                using_alias_entries.push((using_alias.clone(), using_columns));
            }
        }
        let relation_name = relation.name.object_name().to_owned();
        let join_has_explicit_alias = join
            .alias
            .as_ref()
            .is_some_and(|alias| !alias.eq_ignore_ascii_case(&relation_name));
        if !join_has_explicit_alias {
            alias_entries.push((relation_name.clone(), columns.len()));
        }
        if let Some(alias) = join.alias.clone() {
            if !alias.eq_ignore_ascii_case(&relation_name) {
                alias_entries.push((alias, columns.len()));
            }
        }
        append_join_scope_columns(&mut columns, &relation.columns);
    }

    let base_len = columns.len();
    for (idx, (alias, start)) in alias_entries.iter().enumerate() {
        let end = alias_entries
            .iter()
            .skip(idx + 1)
            .find(|(_, next_start)| *next_start > *start)
            .map_or(base_len, |(_, next_start)| *next_start);
        let qualified_columns: Vec<ColumnDescriptor> = columns[*start..end]
            .iter()
            .map(|col| {
                let bare_name = col.name.rsplit('\0').next().unwrap_or(&col.name);
                ColumnDescriptor {
                    column_id: col.column_id,
                    name: format!("{alias}\x00{bare_name}"),
                    data_type: col.data_type.clone(),
                    raw_type_name: None,
                    text_type_modifier: col.text_type_modifier,
                    nullable: col.nullable,
                    ordinal_position: col.ordinal_position,
                    default_value: col.default_value.clone(),
                }
            })
            .collect();
        columns.extend(qualified_columns);
    }

    append_using_alias_columns(&mut columns, using_alias_entries);

    columns
}

pub(super) fn append_join_scope_columns(
    out: &mut Vec<ColumnDescriptor>,
    source: &[ColumnDescriptor],
) {
    for col in source {
        out.push(ColumnDescriptor {
            column_id: col.column_id,
            name: col.name.clone(),
            data_type: col.data_type.clone(),
            raw_type_name: None,
            text_type_modifier: col.text_type_modifier,
            nullable: col.nullable,
            ordinal_position: u32::try_from(out.len().saturating_add(1)).unwrap_or(u32::MAX),
            default_value: col.default_value.clone(),
        });
    }
}

pub(super) fn append_using_alias_columns(
    out: &mut Vec<ColumnDescriptor>,
    using_alias_entries: Vec<(String, Vec<ColumnDescriptor>)>,
) {
    for (alias, columns) in using_alias_entries {
        for col in columns {
            let bare_name = col.name.rsplit('\0').next().unwrap_or(&col.name);
            out.push(ColumnDescriptor {
                column_id: col.column_id,
                name: format!("{alias}\x00{bare_name}"),
                data_type: col.data_type.clone(),
                raw_type_name: None,
                text_type_modifier: col.text_type_modifier,
                nullable: col.nullable,
                ordinal_position: col.ordinal_position,
                default_value: col.default_value.clone(),
            });
        }
    }
}

pub(super) fn branch_output_expr(branch: &TypedSetBranch, index: usize) -> Option<&TypedExpr> {
    match branch {
        TypedSetBranch::Select(select) => select.outputs.get(index).map(|output| &output.expr),
        TypedSetBranch::SetOperation(_) => None,
        TypedSetBranch::Insert(insert) => insert.returning.get(index).map(|output| &output.expr),
        TypedSetBranch::Update(update) => update.returning.get(index).map(|output| &output.expr),
        TypedSetBranch::Delete(delete) => delete.returning.get(index).map(|output| &output.expr),
    }
}

pub(super) fn branch_output_unknown(branch: &TypedSetBranch, index: usize) -> bool {
    match branch {
        TypedSetBranch::Select(select) => select.outputs.get(index).is_some_and(|output| {
            matches!(
                output.expr.kind,
                TypedExprKind::Literal(Value::Null | Value::Text(_))
            )
        }),
        TypedSetBranch::SetOperation(set_op) => {
            set_op
                .output_fields
                .get(index)
                .is_some_and(|field| matches!(field.data_type, DataType::Text))
                && branch_output_unknown(set_op.left.as_ref(), index)
                && branch_output_unknown(set_op.right.as_ref(), index)
        }
        TypedSetBranch::Insert(insert) => insert.returning.get(index).is_some_and(|output| {
            matches!(
                output.expr.kind,
                TypedExprKind::Literal(Value::Null | Value::Text(_))
            )
        }),
        TypedSetBranch::Update(update) => update.returning.get(index).is_some_and(|output| {
            matches!(
                output.expr.kind,
                TypedExprKind::Literal(Value::Null | Value::Text(_))
            )
        }),
        TypedSetBranch::Delete(delete) => delete.returning.get(index).is_some_and(|output| {
            matches!(
                output.expr.kind,
                TypedExprKind::Literal(Value::Null | Value::Text(_))
            )
        }),
    }
}

/// Type-check a standalone SQL expression (no relation context, no subqueries,
/// no user functions, no parameters). Used by the executor for evaluating
/// user-defined function bodies after parameter substitution.
pub fn type_check_expression(expr: &Expr, _param_types: &mut Vec<DataType>) -> DbResult<TypedExpr> {
    let mut params = params::ParameterTypes::default();
    infer_expr(expr, None, &mut params, None, None)
}

/// Type-check a SQL expression against a relation context. Parameter names
/// in the expression resolve to `ColumnRef` nodes whose ordinals match the
/// column positions in `relation`. This is used for user-defined function
/// and trigger evaluation so that we can substitute values via a `Row`
/// instead of fragile text substitution.
pub fn type_check_expression_with_relation(
    expr: &Expr,
    relation: &TableDescriptor,
) -> DbResult<TypedExpr> {
    let mut params = params::ParameterTypes::default();
    infer_expr(expr, Some(relation), &mut params, None, None)
}

pub fn type_check_expression_with_relation_and_session_context(
    expr: &Expr,
    relation: &TableDescriptor,
    current_user: Option<String>,
    session_user: Option<String>,
    current_schema: Option<String>,
    current_database: Option<String>,
) -> DbResult<TypedExpr> {
    let search_path_schemas = current_search_path_schemas();
    let mut params = params::ParameterTypes::with_session_context(params::SessionVariableContext {
        current_user,
        session_user,
        current_schema,
        current_database,
        search_path_schemas,
    });
    infer_expr(expr, Some(relation), &mut params, None, None)
}

/// Expand structured GROUP BY items into a list of concrete grouping set
/// combinations.  Each inner `Vec<usize>` contains indices into the flat
/// `group_by` list indicating which columns participate in that grouping
/// set.  Returns an empty vec when the query uses plain GROUP BY (no
/// ROLLUP/CUBE/GROUPING SETS), meaning a single implicit grouping set
/// containing all columns.
pub(super) fn expand_grouping_sets(group_by: &[Expr], items: &[GroupByItem]) -> Vec<Vec<usize>> {
    // Quick check: if all items are Plain, there are no grouping sets.
    let has_set_constructors = items
        .iter()
        .any(|item| !matches!(item, GroupByItem::Plain(_)));
    if !has_set_constructors && !items.iter().any(|item| matches!(item, GroupByItem::Empty)) {
        return Vec::new();
    }

    // Build a lookup from expression to index in the flat group_by list.
    let expr_index =
        |expr: &Expr| -> usize { group_by.iter().position(|e| e == expr).unwrap_or(0) };

    // Each top-level GROUP BY item contributes a list of grouping set
    // components.  The final result is the cross-product of all components.
    let mut components: Vec<Vec<Vec<usize>>> = Vec::new();

    for item in items {
        match item {
            GroupByItem::Plain(expr) => {
                // A plain expression contributes a single component with one set.
                let idx = expr_index(expr);
                components.push(vec![vec![idx]]);
            }
            GroupByItem::Rollup(col_sets) => {
                components.push(expand_rollup(col_sets, &expr_index));
            }
            GroupByItem::Cube(col_sets) => {
                components.push(expand_cube(col_sets, &expr_index));
            }
            GroupByItem::GroupingSets(sets) => {
                let expanded = expand_grouping_sets_body(sets, group_by, &expr_index);
                components.push(expanded);
            }
            GroupByItem::Empty => {
                // An empty grouping set contributes a component with one empty set.
                components.push(vec![vec![]]);
            }
        }
    }

    if components.is_empty() {
        return vec![vec![]];
    }

    // Cross-product all components: merge the index sets from each component.
    let mut result = components[0].clone();
    for component in &components[1..] {
        let mut new_result = Vec::new();
        for existing in &result {
            for addition in component {
                let mut merged = existing.clone();
                for &idx in addition {
                    if !merged.contains(&idx) {
                        merged.push(idx);
                    }
                }
                new_result.push(merged);
            }
        }
        result = new_result;
    }

    result
}

/// Expand ROLLUP(A, B, C) into grouping sets:
/// (A, B, C), (A, B), (A), ()
pub(super) fn expand_rollup(
    col_sets: &[Vec<Expr>],
    expr_index: &dyn Fn(&Expr) -> usize,
) -> Vec<Vec<usize>> {
    let mut sets = Vec::new();
    // Full set, then progressively drop from the end.
    for prefix_len in (0..=col_sets.len()).rev() {
        let mut indices = Vec::new();
        for col_set in &col_sets[..prefix_len] {
            for expr in col_set {
                let idx = expr_index(expr);
                if !indices.contains(&idx) {
                    indices.push(idx);
                }
            }
        }
        sets.push(indices);
    }
    sets
}

/// Expand CUBE(A, B, C) into all 2^n subsets.
pub(super) fn expand_cube(
    col_sets: &[Vec<Expr>],
    expr_index: &dyn Fn(&Expr) -> usize,
) -> Vec<Vec<usize>> {
    let n = col_sets.len();
    if n >= usize::try_from(u64::BITS).unwrap_or(usize::MAX) {
        // Bitmask-based expansion cannot represent >= 64 dimensions.
        return vec![Vec::new()];
    }
    let mut sets = Vec::new();
    let total_masks = 1u64 << n;
    // Iterate from all-bits-set down to 0 (most columns first).
    for mask in (0..total_masks).rev() {
        let mut indices = Vec::new();
        for (i, col_set) in col_sets.iter().enumerate() {
            if mask & (1u64 << (n - 1 - i)) != 0 {
                for expr in col_set {
                    let idx = expr_index(expr);
                    if !indices.contains(&idx) {
                        indices.push(idx);
                    }
                }
            }
        }
        sets.push(indices);
    }
    sets
}

/// Expand the body of GROUPING SETS(...).
#[allow(clippy::only_used_in_recursion)]
pub(super) fn expand_grouping_sets_body(
    sets: &[GroupBySet],
    group_by: &[Expr],
    expr_index: &dyn Fn(&Expr) -> usize,
) -> Vec<Vec<usize>> {
    let mut result = Vec::new();
    for set in sets {
        match set {
            GroupBySet::Exprs(exprs) => {
                let mut indices = Vec::new();
                for expr in exprs {
                    let idx = expr_index(expr);
                    if !indices.contains(&idx) {
                        indices.push(idx);
                    }
                }
                result.push(indices);
            }
            GroupBySet::Empty => {
                result.push(vec![]);
            }
            GroupBySet::Rollup(col_sets) => {
                result.extend(expand_rollup(col_sets, expr_index));
            }
            GroupBySet::Cube(col_sets) => {
                result.extend(expand_cube(col_sets, expr_index));
            }
            GroupBySet::Nested(inner) => {
                result.extend(expand_grouping_sets_body(inner, group_by, expr_index));
            }
        }
    }
    result
}
