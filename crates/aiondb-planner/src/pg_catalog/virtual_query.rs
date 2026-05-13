use std::cmp::Ordering;

use aiondb_core::compat_setting_value;
use aiondb_eval::{
    compare_runtime_values, compat_display_type_name, normalize_compat_type_name,
    with_current_session_context, CompatCastMethod,
};
use aiondb_parser::{BinaryOperator, Expr, Literal, OrderByItem, SelectItem, SelectStatement};

#[path = "virtual_query_aggregate_support.rs"]
mod aggregate_support;

use self::aggregate_support::resolve_aggregate_value;
use super::virtual_query_helpers::{
    build_dynamic_order_by, build_dynamic_projection_outputs, dynamic_virtual_relation_descriptor,
    expr_contains_aggregate_typed, is_star_expr, literal_to_value, order_by_position_to_index,
    project_output_fields, rows_to_typed, virtual_expr_to_typed,
};
use super::*;
use crate::information_schema::query_helpers::{
    expr_contains_count_aggregate as expr_contains_aggregate, find_column_index,
};

#[inline]
fn process_id_i32_saturating() -> i32 {
    i32::try_from(std::process::id()).unwrap_or(i32::MAX)
}

pub(crate) fn build_select_plan(
    catalog: &Arc<dyn CatalogReader>,
    txn_id: TxnId,
    select: &SelectStatement,
    default_schema: Option<&str>,
    session_user: Option<&str>,
    database_name: Option<&str>,
) -> DbResult<Option<LogicalPlan>> {
    let Some(table_name) = extract_table_name(select) else {
        return Ok(None);
    };
    if !is_supported_select_shape(select) {
        // Complex query shapes (JOINs, CTEs, GROUP BY, etc.) fall through to
        // the normal binder which can resolve pg_catalog tables via
        // `resolve_virtual_relation`.
        return Ok(None);
    }

    let Some(base_plan) = build_base_plan(
        catalog,
        txn_id,
        table_name,
        default_schema,
        session_user,
        database_name,
    )?
    else {
        return Ok(None);
    };

    let fast_path_plan: DbResult<LogicalPlan> = (|| {
        if table_name.eq_ignore_ascii_case(PG_SETTINGS) {
            return build_dynamic_pg_settings_select_plan(select, base_plan);
        }

        let LogicalPlan::ProjectValues {
            output_fields: base_fields,
            rows,
            ..
        } = base_plan
        else {
            return Err(DbError::internal(
                "pg_catalog virtual plans must be ProjectValues plans",
            ));
        };

        let base_rows = typed_rows_to_values(rows)?;
        let filtered_rows = apply_selection(&base_fields, base_rows, select.selection.as_ref())?;
        let (output_fields, projected_rows) =
            project_rows(&base_fields, filtered_rows.clone(), &select.items)?;
        let projected_rows = sort_projected_rows(
            &base_fields,
            &output_fields,
            filtered_rows,
            projected_rows,
            &select.order_by,
        )?;
        let typed_limit = select
            .limit
            .as_ref()
            .map(virtual_expr_to_typed)
            .transpose()?;
        let typed_offset = select
            .offset
            .as_ref()
            .map(virtual_expr_to_typed)
            .transpose()?;

        Ok(LogicalPlan::ProjectValues {
            output_fields: output_fields.clone(),
            rows: rows_to_typed(&output_fields, projected_rows),
            order_by: Vec::new(),
            limit: typed_limit,
            offset: typed_offset,
        })
    })();

    match fast_path_plan {
        Ok(plan) => Ok(Some(plan)),
        Err(err) if should_fallback_to_general_pg_catalog_binder(&err) => Ok(None),
        Err(err) => Err(err),
    }
}

fn extract_table_name(select: &SelectStatement) -> Option<&str> {
    let from = select.from.as_ref()?;
    match from.parts.as_slice() {
        [schema, table] if is_pg_catalog(schema) => Some(table),
        [table] if is_pg_catalog_table(table) => Some(table),
        _ => None,
    }
}

fn is_supported_select_shape(select: &SelectStatement) -> bool {
    select.ctes.is_empty()
        && select.joins.is_empty()
        && select.group_by.is_empty()
        && select.having.is_none()
        && matches!(select.distinct, aiondb_parser::DistinctKind::All)
        && !select_uses_context_sensitive_pg_catalog_functions(select)
}

fn select_uses_context_sensitive_pg_catalog_functions(select: &SelectStatement) -> bool {
    select
        .items
        .iter()
        .any(|item| expr_uses_context_sensitive_pg_catalog_function(&item.expr))
        || select
            .selection
            .as_ref()
            .is_some_and(expr_uses_context_sensitive_pg_catalog_function)
        || select
            .having
            .as_ref()
            .is_some_and(expr_uses_context_sensitive_pg_catalog_function)
        || select
            .group_by
            .iter()
            .any(expr_uses_context_sensitive_pg_catalog_function)
        || select
            .order_by
            .iter()
            .any(|item| expr_uses_context_sensitive_pg_catalog_function(&item.expr))
}

fn expr_uses_context_sensitive_pg_catalog_function(expr: &Expr) -> bool {
    match expr {
        Expr::Identifier(_)
        | Expr::Literal(_, _)
        | Expr::Parameter { .. }
        | Expr::Default { .. } => false,
        Expr::FunctionCall {
            name, args, filter, ..
        } => {
            let function_name = name
                .parts
                .last()
                .map_or("", String::as_str)
                .to_ascii_lowercase();
            matches!(
                function_name.as_str(),
                "pg_relation_size" | "pg_table_size" | "pg_total_relation_size" | "pg_indexes_size"
            ) || args
                .iter()
                .any(expr_uses_context_sensitive_pg_catalog_function)
                || filter
                    .as_ref()
                    .is_some_and(|expr| expr_uses_context_sensitive_pg_catalog_function(expr))
        }
        Expr::Cast { expr, .. } | Expr::UnaryOp { expr, .. } | Expr::IsNull { expr, .. } => {
            expr_uses_context_sensitive_pg_catalog_function(expr)
        }
        Expr::BinaryOp { left, right, .. }
        | Expr::Like {
            expr: left,
            pattern: right,
            ..
        }
        | Expr::IsDistinctFrom { left, right, .. } => {
            expr_uses_context_sensitive_pg_catalog_function(left)
                || expr_uses_context_sensitive_pg_catalog_function(right)
        }
        Expr::InList { expr, list, .. } => {
            expr_uses_context_sensitive_pg_catalog_function(expr)
                || list
                    .iter()
                    .any(expr_uses_context_sensitive_pg_catalog_function)
        }
        Expr::Between {
            expr, low, high, ..
        } => {
            expr_uses_context_sensitive_pg_catalog_function(expr)
                || expr_uses_context_sensitive_pg_catalog_function(low)
                || expr_uses_context_sensitive_pg_catalog_function(high)
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
                .is_some_and(|expr| expr_uses_context_sensitive_pg_catalog_function(expr))
                || conditions
                    .iter()
                    .any(expr_uses_context_sensitive_pg_catalog_function)
                || results
                    .iter()
                    .any(expr_uses_context_sensitive_pg_catalog_function)
                || else_result
                    .as_ref()
                    .is_some_and(|expr| expr_uses_context_sensitive_pg_catalog_function(expr))
        }
        Expr::Array { elements, .. } => elements
            .iter()
            .any(expr_uses_context_sensitive_pg_catalog_function),
        Expr::ArraySubquery { .. }
        | Expr::Subquery { .. }
        | Expr::InSubquery { .. }
        | Expr::Exists { .. }
        | Expr::CypherExists { .. }
        | Expr::CypherPatternComprehension { .. }
        | Expr::WindowFunction { .. } => true,
    }
}

fn typed_rows_to_values(rows: Vec<Vec<TypedExpr>>) -> DbResult<Vec<Vec<Value>>> {
    rows.into_iter()
        .map(|row| {
            row.into_iter()
                .map(|expr| match expr.kind {
                    aiondb_plan::TypedExprKind::Literal(value) => Ok(value),
                    _ => Err(DbError::internal(
                        "pg_catalog virtual rows must contain only literal expressions",
                    )),
                })
                .collect()
        })
        .collect()
}

fn build_dynamic_pg_settings_select_plan(
    select: &SelectStatement,
    base_plan: LogicalPlan,
) -> DbResult<LogicalPlan> {
    let base_fields = match &base_plan {
        LogicalPlan::ProjectValues { output_fields, .. } => output_fields.clone(),
        _ => {
            return Err(DbError::internal(
                "pg_settings base plan must be a ProjectValues plan",
            ));
        }
    };
    let relation = dynamic_virtual_relation_descriptor(PG_SETTINGS, select.from_alias.as_deref())?;
    let outputs = build_dynamic_projection_outputs(&base_fields, &relation, &select.items)?;
    let filter = select
        .selection
        .as_ref()
        .map(|expr| crate::type_check_expression_with_relation(expr, &relation))
        .transpose()?;
    let order_by = build_dynamic_order_by(&relation, &outputs, &select.order_by)?;
    let limit = select
        .limit
        .as_ref()
        .map(virtual_expr_to_typed)
        .transpose()?;
    let offset = select
        .offset
        .as_ref()
        .map(virtual_expr_to_typed)
        .transpose()?;
    let has_aggregates = outputs
        .iter()
        .any(|projection| expr_contains_aggregate_typed(&projection.expr));

    Ok(if has_aggregates {
        LogicalPlan::AggregateSource {
            source: Box::new(base_plan),
            group_by: Vec::new(),
            grouping_sets: Vec::new(),
            aggregates: outputs,
            having: None,
            filter,
            order_by,
            limit,
            offset,
            distinct: false,
            distinct_on: Vec::new(),
        }
    } else {
        LogicalPlan::ProjectSource {
            source: Box::new(base_plan),
            outputs,
            filter,
            order_by,
            limit,
            offset,
            distinct: false,
            distinct_on: Vec::new(),
        }
    })
}

fn apply_selection(
    fields: &[ResultField],
    rows: Vec<Vec<Value>>,
    selection: Option<&Expr>,
) -> DbResult<Vec<Vec<Value>>> {
    rows.into_iter()
        .filter_map(|row| match row_matches_selection(fields, &row, selection) {
            Ok(true) => Some(Ok(row)),
            Ok(false) => None,
            Err(err) => Some(Err(err)),
        })
        .collect()
}

fn row_matches_selection(
    fields: &[ResultField],
    row: &[Value],
    selection: Option<&Expr>,
) -> DbResult<bool> {
    let Some(selection) = selection else {
        return Ok(true);
    };
    match selection {
        Expr::BinaryOp {
            left,
            op: BinaryOperator::Eq,
            right,
            ..
        } => {
            let left = resolve_value(fields, row, left)?;
            let right = resolve_value(fields, row, right)?;
            Ok(compare_runtime_values(&left, &right)?
                .is_some_and(|ordering| ordering == Ordering::Equal))
        }
        Expr::BinaryOp {
            left,
            op: BinaryOperator::And,
            right,
            ..
        } => Ok(row_matches_selection(fields, row, Some(left))?
            && row_matches_selection(fields, row, Some(right))?),
        Expr::BinaryOp {
            left,
            op: BinaryOperator::Or,
            right,
            ..
        } => Ok(row_matches_selection(fields, row, Some(left))?
            || row_matches_selection(fields, row, Some(right))?),
        Expr::BinaryOp {
            left,
            op: BinaryOperator::Ne,
            right,
            ..
        } => {
            let left = resolve_value(fields, row, left)?;
            let right = resolve_value(fields, row, right)?;
            Ok(compare_runtime_values(&left, &right)?
                .is_some_and(|ordering| ordering != Ordering::Equal))
        }
        Expr::BinaryOp {
            left,
            op:
                op @ (BinaryOperator::Lt | BinaryOperator::Le | BinaryOperator::Gt | BinaryOperator::Ge),
            right,
            ..
        } => {
            let left = resolve_value(fields, row, left)?;
            let right = resolve_value(fields, row, right)?;
            let ordering = compare_runtime_values(&left, &right)?;
            Ok(match op {
                BinaryOperator::Lt => ordering.is_some_and(|ordering| ordering == Ordering::Less),
                BinaryOperator::Le => {
                    ordering.is_some_and(|ordering| ordering != Ordering::Greater)
                }
                BinaryOperator::Gt => {
                    ordering.is_some_and(|ordering| ordering == Ordering::Greater)
                }
                BinaryOperator::Ge => ordering.is_some_and(|ordering| ordering != Ordering::Less),
                _ => unreachable!(),
            })
        }
        Expr::IsNull { expr, negated, .. } => {
            let is_null = matches!(resolve_value(fields, row, expr)?, Value::Null);
            Ok(if *negated { !is_null } else { is_null })
        }
        Expr::Like {
            expr,
            pattern,
            negated,
            case_insensitive,
            ..
        } => {
            let value = resolve_value(fields, row, expr)?;
            let pattern = resolve_value(fields, row, pattern)?;
            let matches = match (value, pattern) {
                (Value::Text(value), Value::Text(pattern)) => {
                    like_matches_pattern(&value, &pattern, *case_insensitive)
                }
                _ => false,
            };
            Ok(if *negated { !matches } else { matches })
        }
        Expr::UnaryOp {
            op: aiondb_parser::UnaryOperator::Not,
            expr,
            ..
        } => Ok(!row_matches_selection(fields, row, Some(expr))?),
        Expr::InList {
            expr,
            list,
            negated,
            ..
        } => {
            let value = resolve_value(fields, row, expr)?;
            let found = list.iter().any(|item| {
                resolve_value(fields, row, item)
                    .and_then(|candidate| compare_runtime_values(&candidate, &value))
                    .map(|ordering| ordering.is_some_and(|ordering| ordering == Ordering::Equal))
                    .unwrap_or(false)
            });
            Ok(if *negated { !found } else { found })
        }
        Expr::Between {
            expr,
            low,
            high,
            negated,
            ..
        } => {
            let value = resolve_value(fields, row, expr)?;
            let low = resolve_value(fields, row, low)?;
            let high = resolve_value(fields, row, high)?;
            let lower_ok = compare_runtime_values(&value, &low)?
                .is_some_and(|ordering| ordering != Ordering::Less);
            let upper_ok = compare_runtime_values(&value, &high)?
                .is_some_and(|ordering| ordering != Ordering::Greater);
            let within = lower_ok && upper_ok;
            Ok(if *negated { !within } else { within })
        }
        Expr::BinaryOp {
            left,
            op:
                op @ (BinaryOperator::RegexMatch
                | BinaryOperator::RegexMatchInsensitive
                | BinaryOperator::NotRegexMatch
                | BinaryOperator::NotRegexMatchInsensitive),
            right,
            ..
        } => {
            let left_val = resolve_value(fields, row, left)?;
            let right_val = resolve_value(fields, row, right)?;
            let matched = match (left_val, right_val) {
                (Value::Text(value), Value::Text(pattern)) => {
                    let case_insensitive = matches!(
                        op,
                        BinaryOperator::RegexMatchInsensitive
                            | BinaryOperator::NotRegexMatchInsensitive
                    );
                    let regex_pattern = if case_insensitive {
                        format!("(?i){pattern}")
                    } else {
                        pattern
                    };
                    regex::Regex::new(&regex_pattern)
                        .map(|re| re.is_match(&value))
                        .unwrap_or(false)
                }
                _ => false,
            };
            let negated = matches!(
                op,
                BinaryOperator::NotRegexMatch | BinaryOperator::NotRegexMatchInsensitive
            );
            Ok(if negated { !matched } else { matched })
        }
        _ => Err(DbError::bind_error(
            SqlState::FeatureNotSupported,
            "unsupported WHERE clause expression on pg_catalog virtual table",
        )),
    }
}

fn resolve_value(fields: &[ResultField], row: &[Value], expr: &Expr) -> DbResult<Value> {
    match expr {
        Expr::Identifier(name) => {
            let column_name = name.parts.last().ok_or_else(|| {
                DbError::bind_error(SqlState::UndefinedColumn, "empty identifier is not allowed")
            })?;
            let index = find_column_index(fields, column_name)?;
            Ok(row[index].clone())
        }
        Expr::Literal(literal, _) => Ok(literal_to_value(literal)),
        Expr::Cast {
            expr, data_type, ..
        } => {
            let value = resolve_value(fields, row, expr)?;
            match data_type {
                DataType::Text if expr_is_type_hint(expr, "regrole") => {
                    resolve_regrole_text_value(value)
                }
                DataType::Text => Ok(Value::Text(value.to_string())),
                _ => Ok(value),
            }
        }
        Expr::BinaryOp {
            left, op, right, ..
        } => {
            let left = resolve_value(fields, row, left)?;
            let right = resolve_value(fields, row, right)?;
            evaluate_binary_value(*op, left, right)
        }
        Expr::FunctionCall { name, args, .. } => {
            if name
                .parts
                .last()
                .is_some_and(|part| part.eq_ignore_ascii_case("__aiondb_type_hint"))
            {
                return resolve_type_hint_value(fields, row, args);
            }
            if name
                .parts
                .last()
                .is_some_and(|part| part.eq_ignore_ascii_case("__aiondb_char_pad_length"))
            {
                return args
                    .first()
                    .map_or(Ok(Value::Null), |expr| resolve_value(fields, row, expr));
            }
            resolve_function_value(fields, row, name, args)
        }
        _ => Err(DbError::bind_error(
            SqlState::SyntaxError,
            "unsupported expression on pg_catalog virtual table",
        )),
    }
}

fn resolve_type_hint_value(
    fields: &[ResultField],
    row: &[Value],
    args: &[Expr],
) -> DbResult<Value> {
    let Some(expr) = args.first() else {
        return Ok(Value::Null);
    };
    let Some(Expr::Literal(Literal::String(type_name), _)) = args.get(1) else {
        return resolve_value(fields, row, expr);
    };
    if type_name.eq_ignore_ascii_case("regtype") {
        let value = match expr {
            Expr::Cast { expr, .. } => resolve_value(fields, row, expr)?,
            _ => resolve_value(fields, row, expr)?,
        };
        let text = match value {
            Value::Null => return Ok(Value::Null),
            ref other => resolve_regtype_name_value(other)
                .ok_or_else(|| DbError::invalid_input_syntax("regtype", &other.to_string()))?,
        };
        return Ok(Value::Text(text));
    }
    if type_name.eq_ignore_ascii_case("regclass") {
        let value = match expr {
            Expr::Cast { expr, .. } => resolve_value(fields, row, expr)?,
            _ => resolve_value(fields, row, expr)?,
        };
        let text = match value {
            Value::Null => return Ok(Value::Null),
            Value::Text(text) => text,
            other => other.to_string(),
        };
        return resolve_regclass_oid(&text).map(Value::Int).ok_or_else(|| {
            DbError::bind_error(
                SqlState::UndefinedTable,
                format!("relation \"{text}\" does not exist"),
            )
        });
    }
    if type_name.eq_ignore_ascii_case("regrole") {
        let value = match expr {
            Expr::Cast { expr, .. } => resolve_value(fields, row, expr)?,
            _ => resolve_value(fields, row, expr)?,
        };
        return resolve_regrole_oid_value(value);
    }
    resolve_value(fields, row, expr)
}

fn resolve_regrole_oid_value(value: Value) -> DbResult<Value> {
    match value {
        Value::Null => Ok(Value::Null),
        Value::Int(oid) => Ok(Value::Int(oid)),
        Value::BigInt(oid) => {
            let oid = i32::try_from(oid).map_err(|_| {
                DbError::bind_error(
                    SqlState::NumericValueOutOfRange,
                    format!("OID value {oid} is out of range"),
                )
            })?;
            Ok(Value::Int(oid))
        }
        other => {
            let role_name = parse_non_qualified_reg_name(&other.to_string())?;
            let role_oid = with_current_session_context(|context| {
                context
                    .role_names_by_oid
                    .iter()
                    .find_map(|(oid, candidate)| (candidate == &role_name).then_some(*oid))
            });
            role_oid.map(Value::Int).ok_or_else(|| {
                DbError::bind_error(
                    SqlState::UndefinedObject,
                    format!("role \"{role_name}\" does not exist"),
                )
            })
        }
    }
}

fn parse_non_qualified_reg_name(input: &str) -> DbResult<String> {
    let trimmed = input.trim();
    if trimmed.contains('.') {
        return Err(DbError::bind_error(
            SqlState::InvalidTextRepresentation,
            "invalid name syntax",
        ));
    }
    if let Some(inner) = trimmed
        .strip_prefix('"')
        .and_then(|value| value.strip_suffix('"'))
    {
        return Ok(inner.replace("\"\"", "\""));
    }
    if trimmed.contains('"') {
        return Err(DbError::bind_error(
            SqlState::InvalidTextRepresentation,
            "invalid name syntax",
        ));
    }
    Ok(trimmed.to_ascii_lowercase())
}

fn resolve_regtype_name_value(value: &Value) -> Option<String> {
    match value {
        Value::Null => None,
        Value::Int(oid) => regtype_name_for_oid(*oid),
        Value::BigInt(oid) => i32::try_from(*oid).ok().and_then(regtype_name_for_oid),
        Value::Text(text) => resolve_regtype_oid(text).and_then(regtype_name_for_oid),
        other => {
            let text = other.to_string();
            text.parse::<i32>()
                .ok()
                .and_then(regtype_name_for_oid)
                .or_else(|| resolve_regtype_oid(&text).and_then(regtype_name_for_oid))
        }
    }
}

fn regtype_name_for_oid(oid: i32) -> Option<String> {
    PG_TYPE_ENTRIES
        .iter()
        .find(|entry| entry.oid == oid)
        .map(|entry| entry.name.to_owned())
        .or_else(|| {
            with_current_session_context(|context| {
                context
                    .compat_user_types
                    .iter()
                    .find(|entry| entry.oid == oid)
                    .map(|entry| entry.name.clone())
            })
        })
}

fn evaluate_binary_value(op: BinaryOperator, left: Value, right: Value) -> DbResult<Value> {
    if matches!(left, Value::Null) || matches!(right, Value::Null) {
        return Ok(Value::Null);
    }

    let ordering = compare_runtime_values(&left, &right)?;
    let result = match op {
        BinaryOperator::Eq => ordering.map(|ordering| ordering == Ordering::Equal),
        BinaryOperator::Ne => ordering.map(|ordering| ordering != Ordering::Equal),
        BinaryOperator::Lt => ordering.map(|ordering| ordering == Ordering::Less),
        BinaryOperator::Le => ordering.map(|ordering| ordering != Ordering::Greater),
        BinaryOperator::Gt => ordering.map(|ordering| ordering == Ordering::Greater),
        BinaryOperator::Ge => ordering.map(|ordering| ordering != Ordering::Less),
        _ => None,
    };

    result
        .map(Value::Boolean)
        .ok_or_else(|| DbError::bind_error(SqlState::SyntaxError, "unsupported binary expression"))
}

fn like_matches_pattern(value: &str, pattern: &str, case_insensitive: bool) -> bool {
    let value = if case_insensitive {
        value.to_ascii_lowercase()
    } else {
        value.to_owned()
    };
    let pattern = if case_insensitive {
        pattern.to_ascii_lowercase()
    } else {
        pattern.to_owned()
    };
    like_match_bytes(value.as_bytes(), pattern.as_bytes())
}

fn like_match_bytes(value: &[u8], pattern: &[u8]) -> bool {
    if pattern.is_empty() {
        return value.is_empty();
    }
    match pattern[0] {
        b'%' => {
            like_match_bytes(value, &pattern[1..])
                || (!value.is_empty() && like_match_bytes(&value[1..], pattern))
        }
        b'_' => !value.is_empty() && like_match_bytes(&value[1..], &pattern[1..]),
        byte => {
            !value.is_empty() && value[0] == byte && like_match_bytes(&value[1..], &pattern[1..])
        }
    }
}

fn resolve_function_value(
    fields: &[ResultField],
    row: &[Value],
    name: &aiondb_parser::ObjectName,
    args: &[Expr],
) -> DbResult<Value> {
    let function_name = name
        .parts
        .last()
        .map_or("", String::as_str)
        .to_ascii_lowercase();
    let arg_values = args
        .iter()
        .map(|arg| resolve_value(fields, row, arg))
        .collect::<DbResult<Vec<_>>>()?;
    match function_name.as_str() {
        "to_regclass" | "regclass" => match arg_values.first() {
            Some(Value::Null) | None => Ok(Value::Null),
            Some(Value::Text(name)) => {
                Ok(resolve_regclass_oid(name).map_or(Value::Null, Value::Int))
            }
            Some(value) => {
                Ok(resolve_regclass_oid(&value.to_string()).map_or(Value::Null, Value::Int))
            }
        },
        "to_regnamespace" | "regnamespace" => match arg_values.first() {
            Some(Value::Null) | None => Ok(Value::Null),
            Some(Value::Text(name)) => {
                Ok(resolve_regnamespace_oid(name).map_or(Value::Null, Value::Int))
            }
            Some(value) => {
                Ok(resolve_regnamespace_oid(&value.to_string()).map_or(Value::Null, Value::Int))
            }
        },
        "to_regtype" | "regtype" => match arg_values.first() {
            Some(Value::Null) | None => Ok(Value::Null),
            Some(Value::Text(name)) => {
                Ok(resolve_regtype_oid(name).map_or(Value::Null, Value::Int))
            }
            Some(value) => {
                Ok(resolve_regtype_oid(&value.to_string()).map_or(Value::Null, Value::Int))
            }
        },
        "to_regoperator" | "regoperator" => match arg_values.first() {
            Some(Value::Null) | None => Ok(Value::Null),
            Some(Value::Text(name)) => {
                Ok(resolve_regoperator_oid(name).map_or(Value::Null, Value::Int))
            }
            Some(value) => {
                Ok(resolve_regoperator_oid(&value.to_string()).map_or(Value::Null, Value::Int))
            }
        },
        "to_regprocedure" | "regprocedure" => match arg_values.first() {
            Some(Value::Null) | None => Ok(Value::Null),
            Some(Value::Text(name)) => {
                Ok(resolve_regprocedure_oid(name).map_or(Value::Null, Value::Int))
            }
            Some(value) => {
                Ok(resolve_regprocedure_oid(&value.to_string()).map_or(Value::Null, Value::Int))
            }
        },
        "pg_log_backend_memory_contexts" => match arg_values.first() {
            Some(Value::Null) | None => Ok(Value::Null),
            Some(Value::Int(_) | Value::BigInt(_)) => Ok(Value::Boolean(true)),
            _ => Ok(Value::Boolean(false)),
        },
        "current_setting" => match arg_values.first() {
            Some(Value::Text(name)) => {
                Ok(current_setting_value(name).map_or(Value::Null, Value::Text))
            }
            _ => Ok(Value::Null),
        },
        "pg_describe_object" => resolve_pg_describe_object_value(&arg_values),
        "pg_backend_pid" => Ok(Value::Int(process_id_i32_saturating())),
        _ => Err(DbError::bind_error(
            SqlState::FeatureNotSupported,
            format!("function \"{function_name}\" is not supported in pg_catalog virtual filters"),
        )),
    }
}

const COMPAT_PG_TYPE_CLASSID: i32 = 60_004;
const COMPAT_PG_PROC_CLASSID: i32 = 60_019;
const COMPAT_PG_CAST_CLASSID: i32 = 60_042;

fn resolve_pg_describe_object_value(args: &[Value]) -> DbResult<Value> {
    if args.iter().any(Value::is_null) || args.len() < 3 {
        return Ok(Value::Null);
    }
    let classid = value_as_i32(&args[0]);
    let objid = value_as_i32(&args[1]);
    let objsubid = value_as_i32(&args[2]).unwrap_or_default();
    if objsubid != 0 {
        return Ok(Value::Text(String::new()));
    }
    let Some(classid) = classid else {
        return Ok(Value::Text(String::new()));
    };
    let Some(objid) = objid else {
        return Ok(Value::Text(String::new()));
    };
    if let Some(description) = with_current_session_context(|context| {
        if classid == COMPAT_PG_CAST_CLASSID {
            if let Some(cast) = context
                .compat_user_casts
                .iter()
                .find(|entry| entry.oid == objid)
            {
                return Some(format!(
                    "cast from {} to {}",
                    compat_display_type_name(&cast.source_type),
                    cast.target_type
                ));
            }
        }
        if classid == COMPAT_PG_TYPE_CLASSID {
            if let Some(user_type) = context
                .compat_user_types
                .iter()
                .find(|entry| entry.oid == objid)
            {
                return Some(format!("type {}", user_type.name));
            }
        }
        if classid == COMPAT_PG_PROC_CLASSID {
            if let Some(cast) = context.compat_user_casts.iter().find(|entry| {
                matches!(
                    &entry.method,
                    CompatCastMethod::Function { function_oid, .. } if *function_oid == objid
                )
            }) {
                if let CompatCastMethod::Function { function_name, .. } = &cast.method {
                    return Some(format!(
                        "function {}({})",
                        function_name,
                        compat_display_type_name(&cast.source_type)
                    ));
                }
            }
        }
        None
    }) {
        return Ok(Value::Text(description));
    }

    Ok(Value::Text(String::new()))
}

fn value_as_i32(value: &Value) -> Option<i32> {
    match value {
        Value::Int(value) => Some(*value),
        Value::BigInt(value) => i32::try_from(*value).ok(),
        Value::Text(value) => value.parse::<i32>().ok(),
        _ => None,
    }
}

fn resolve_regclass_oid(input: &str) -> Option<i32> {
    let normalized = input.trim_matches('"').to_ascii_lowercase();
    let normalized = normalized
        .strip_prefix("pg_catalog.")
        .or_else(|| normalized.strip_prefix("information_schema."))
        .unwrap_or(&normalized);
    let oid = synthetic_table_id(normalized)?;
    i32::try_from(oid).ok()
}

fn resolve_regnamespace_oid(input: &str) -> Option<i32> {
    let normalized = input.trim_matches('"').to_ascii_lowercase();
    match normalized.as_str() {
        "public" => Some(PUBLIC_NAMESPACE_OID),
        "pg_catalog" => Some(PG_CATALOG_NAMESPACE_OID),
        "information_schema" => Some(INFORMATION_SCHEMA_NAMESPACE_OID),
        _ => None,
    }
}

pub(crate) fn resolve_regtype_oid(input: &str) -> Option<i32> {
    let normalized = normalize_compat_type_name(input);
    if let Some(base) = normalized.strip_suffix("[]") {
        let builtin_array = match base {
            "bool" => Some(1000),
            "bytea" => Some(1001),
            "int8" => Some(1016),
            "int4" => Some(1007),
            "text" => Some(1009),
            "float4" => Some(1021),
            "float8" => Some(1022),
            "date" => Some(1182),
            "time" => Some(1183),
            "timestamp" => Some(1115),
            "timestamptz" => Some(1185),
            "timetz" => Some(1270),
            "interval" => Some(1187),
            "numeric" => Some(1231),
            "uuid" => Some(2951),
            "jsonb" => Some(3807),
            "tid" => Some(1010),
            "pg_lsn" => Some(3221),
            "bit" => Some(COMPAT_PG_BIT_ARRAY_OID),
            "varbit" => Some(COMPAT_PG_VARBIT_ARRAY_OID),
            "vector" => Some(COMPAT_PGVECTOR_VECTOR_ARRAY_OID),
            "halfvec" => Some(COMPAT_PGVECTOR_HALFVEC_ARRAY_OID),
            "sparsevec" => Some(COMPAT_PGVECTOR_SPARSEVEC_ARRAY_OID),
            _ => None,
        };
        if builtin_array.is_some() {
            return builtin_array;
        }
        return with_current_session_context(|context| {
            context
                .compat_user_types
                .iter()
                .find(|entry| entry.name == base)
                .map(|entry| entry.oid)
        });
    }
    let canonical = match normalized.as_str() {
        "bool" | "boolean" => "bool",
        "bytea" | "blob" => "bytea",
        "int8" | "bigint" => "int8",
        "int" | "int4" | "integer" => "int4",
        "text" | "varchar" | "character varying" | "char" | "character" | "name" => "text",
        "float4" | "real" => "float4",
        "float8" | "double precision" | "double" => "float8",
        "date" => "date",
        "time" | "time without time zone" => "time",
        "timetz" | "time with time zone" => "timetz",
        "timestamp" | "timestamp without time zone" => "timestamp",
        "timestamptz" | "timestamp with time zone" => "timestamptz",
        "interval" => "interval",
        "numeric" | "decimal" => "numeric",
        "uuid" => "uuid",
        "jsonb" | "json" => "jsonb",
        "bit" => "bit",
        "varbit" | "bit varying" => "varbit",
        "vector" => "vector",
        "halfvec" => "halfvec",
        "sparsevec" => "sparsevec",
        _ => {
            return with_current_session_context(|context| {
                context
                    .compat_user_types
                    .iter()
                    .find(|entry| entry.name == normalized)
                    .map(|entry| entry.oid)
            })
        }
    };
    PG_TYPE_ENTRIES
        .iter()
        .find(|entry| entry.name == canonical)
        .map(|entry| entry.oid)
}

fn resolve_regoperator_oid(name: &str) -> Option<i32> {
    let compact = name
        .trim()
        .trim_matches('"')
        .replace('"', "")
        .to_ascii_lowercase()
        .chars()
        .filter(|ch| !ch.is_whitespace())
        .collect::<String>();
    let signature = compact.strip_prefix("pg_catalog.").unwrap_or(&compact);
    let (operator, args) = signature.split_once('(')?;
    let args = args.strip_suffix(')')?;
    let (left, right) = args.split_once(',')?;
    let left_oid = resolve_regtype_oid(left)?;
    let right_oid = resolve_regtype_oid(right)?;
    if left_oid != right_oid {
        return None;
    }
    let vector_operator = matches!(operator, "<->" | "<#>" | "<=>" | "<+>")
        && matches!(
            left_oid,
            COMPAT_PGVECTOR_VECTOR_OID
                | COMPAT_PGVECTOR_HALFVEC_OID
                | COMPAT_PGVECTOR_SPARSEVEC_OID
        );
    let bit_operator = matches!(operator, "<~>" | "<%>") && left_oid == COMPAT_PG_BIT_OID;
    (vector_operator || bit_operator).then(|| compat_pgvector_operator_oid(left_oid, operator))
}

fn resolve_regprocedure_oid(name: &str) -> Option<i32> {
    let compact = name
        .trim()
        .trim_matches('"')
        .replace('"', "")
        .to_ascii_lowercase()
        .chars()
        .filter(|ch| !ch.is_whitespace())
        .collect::<String>();
    let signature = compact.strip_prefix("pg_catalog.").unwrap_or(&compact);
    let (proc_name, args) = signature.split_once('(')?;
    let args = args.strip_suffix(')')?;
    let arg_oids = if args.is_empty() {
        Vec::new()
    } else {
        args.split(',')
            .map(resolve_pgvector_regprocedure_arg_oid)
            .collect::<Option<Vec<_>>>()?
    };
    if !pgvector_regprocedure_signature_supported(proc_name, &arg_oids) {
        return None;
    }
    let argtypes = arg_oids
        .iter()
        .map(i32::to_string)
        .collect::<Vec<_>>()
        .join(" ");
    Some(compat_pgvector_function_oid(proc_name, &argtypes))
}

fn resolve_pgvector_regprocedure_arg_oid(name: &str) -> Option<i32> {
    match name {
        "cstring" => Some(2275),
        "integer" => Some(23),
        _ => resolve_regtype_oid(name),
    }
}

fn pgvector_regprocedure_signature_supported(proc_name: &str, arg_oids: &[i32]) -> bool {
    let vector_array_cast = matches!(arg_oids, [1007 | 1021 | 1022 | 1231, 23, 16])
        && matches!(proc_name, "array_to_vector" | "array_to_sparsevec");
    let vector_cast = matches!(
        (proc_name, arg_oids),
        (
            "vector_to_float4" | "vector_to_halfvec" | "vector_to_sparsevec" | "binary_quantize",
            [COMPAT_PGVECTOR_VECTOR_OID, 23, 16]
        ) | ("halfvec_to_vector", [COMPAT_PGVECTOR_HALFVEC_OID, 23, 16])
    );
    let vector_binary = matches!(
        arg_oids,
        [COMPAT_PGVECTOR_VECTOR_OID, COMPAT_PGVECTOR_VECTOR_OID]
            | [COMPAT_PGVECTOR_HALFVEC_OID, COMPAT_PGVECTOR_HALFVEC_OID]
            | [COMPAT_PGVECTOR_SPARSEVEC_OID, COMPAT_PGVECTOR_SPARSEVEC_OID]
    ) && matches!(
        proc_name,
        "l2_distance"
            | "cosine_distance"
            | "inner_product"
            | "negative_inner_product"
            | "l1_distance"
    );
    let vector_unary = matches!(arg_oids, [COMPAT_PGVECTOR_VECTOR_OID])
        && matches!(
            proc_name,
            "vector_dims"
                | "vector_norm"
                | "l2_norm"
                | "l2_normalize"
                | "binary_quantize"
                | "sum"
                | "avg"
        );
    let halfvec_unary = matches!(arg_oids, [COMPAT_PGVECTOR_HALFVEC_OID])
        && matches!(
            proc_name,
            "vector_dims" | "l2_norm" | "l2_normalize" | "binary_quantize" | "sum" | "avg"
        );
    vector_array_cast
        || vector_cast
        || vector_binary
        || vector_unary
        || halfvec_unary
        || matches!(
            (proc_name, arg_oids),
            ("vector_in" | "halfvec_in" | "sparsevec_in", [2275])
                | ("vector_out", [COMPAT_PGVECTOR_VECTOR_OID])
                | ("halfvec_out", [COMPAT_PGVECTOR_HALFVEC_OID])
                | ("sparsevec_out", [COMPAT_PGVECTOR_SPARSEVEC_OID])
                | (
                    "subvector",
                    [
                        COMPAT_PGVECTOR_VECTOR_OID | COMPAT_PGVECTOR_HALFVEC_OID,
                        23,
                        23
                    ]
                )
                | (
                    "hamming_distance" | "jaccard_distance",
                    [COMPAT_PG_BIT_OID, COMPAT_PG_BIT_OID]
                )
        )
}

fn current_setting_value(name: &str) -> Option<String> {
    compat_setting_value(name).map(std::borrow::Cow::into_owned)
}

fn should_fallback_to_general_pg_catalog_binder(err: &DbError) -> bool {
    matches!(err, DbError::Parse(_) | DbError::Bind(_))
}

fn project_rows(
    fields: &[ResultField],
    rows: Vec<Vec<Value>>,
    items: &[SelectItem],
) -> DbResult<(Vec<ResultField>, Vec<Vec<Value>>)> {
    let output_fields = project_output_fields(fields, items)?;
    if items.len() == 1 && is_star_expr(&items[0].expr) {
        return Ok((output_fields, rows));
    }

    if items.iter().any(|item| expr_contains_aggregate(&item.expr)) {
        let aggregate_row = items
            .iter()
            .map(|item| resolve_aggregate_value(fields, &rows, &item.expr))
            .collect::<DbResult<Vec<_>>>()?;
        return Ok((output_fields, vec![aggregate_row]));
    }

    let projected_rows = rows
        .into_iter()
        .map(|row| {
            items
                .iter()
                .map(|item| resolve_projected_value(fields, &row, &item.expr))
                .collect::<DbResult<Vec<_>>>()
        })
        .collect::<DbResult<Vec<_>>>()?;
    Ok((output_fields, projected_rows))
}

fn resolve_projected_value(fields: &[ResultField], row: &[Value], expr: &Expr) -> DbResult<Value> {
    let value = resolve_value(fields, row, expr)?;
    let Expr::FunctionCall { name, args, .. } = expr else {
        return Ok(value);
    };
    if name.parts.last().map(|part| part.as_str()) != Some("__aiondb_type_hint") || args.len() < 2 {
        return Ok(value);
    }
    let Some(Expr::Literal(Literal::String(type_name), _)) = args.get(1) else {
        return Ok(value);
    };
    if !type_name.eq_ignore_ascii_case("regrole") {
        return Ok(value);
    }
    resolve_regrole_text_value(value)
}

fn expr_is_type_hint(expr: &Expr, expected_type_name: &str) -> bool {
    let Expr::FunctionCall { name, args, .. } = expr else {
        return false;
    };
    if !name
        .parts
        .last()
        .is_some_and(|part| part.eq_ignore_ascii_case("__aiondb_type_hint"))
    {
        return false;
    }
    matches!(
        args.get(1),
        Some(Expr::Literal(Literal::String(type_name), _))
            if type_name.eq_ignore_ascii_case(expected_type_name)
    )
}

fn resolve_regrole_text_value(value: Value) -> DbResult<Value> {
    Ok(match value {
        Value::Int(oid) => with_current_session_context(|context| {
            context
                .role_names_by_oid
                .get(&oid)
                .cloned()
                .map_or_else(|| Value::Text(oid.to_string()), Value::Text)
        }),
        Value::BigInt(oid) => {
            let Some(oid32) = i32::try_from(oid).ok() else {
                return Ok(Value::Text(oid.to_string()));
            };
            with_current_session_context(|context| {
                context
                    .role_names_by_oid
                    .get(&oid32)
                    .cloned()
                    .map_or_else(|| Value::Text(oid32.to_string()), Value::Text)
            })
        }
        other => other,
    })
}

fn sort_projected_rows(
    base_fields: &[ResultField],
    output_fields: &[ResultField],
    base_rows: Vec<Vec<Value>>,
    projected_rows: Vec<Vec<Value>>,
    order_by: &[OrderByItem],
) -> DbResult<Vec<Vec<Value>>> {
    if order_by.is_empty() {
        return Ok(projected_rows);
    }

    let mut pairs: Vec<(Vec<Value>, Vec<Value>)> =
        base_rows.into_iter().zip(projected_rows).collect();
    let mut sort_error = None;
    pairs.sort_by(
        |(left_base, left_projected), (right_base, right_projected)| {
            if sort_error.is_some() {
                return Ordering::Equal;
            }
            match compare_ordered_rows(
                base_fields,
                output_fields,
                left_base,
                left_projected,
                right_base,
                right_projected,
                order_by,
            ) {
                Ok(ordering) => ordering,
                Err(err) => {
                    sort_error = Some(err);
                    Ordering::Equal
                }
            }
        },
    );

    if let Some(err) = sort_error {
        return Err(err);
    }

    Ok(pairs
        .into_iter()
        .map(|(_, projected_row)| projected_row)
        .collect())
}

fn compare_ordered_rows(
    base_fields: &[ResultField],
    output_fields: &[ResultField],
    left_base: &[Value],
    left_projected: &[Value],
    right_base: &[Value],
    right_projected: &[Value],
    order_by: &[OrderByItem],
) -> DbResult<Ordering> {
    for item in order_by {
        let left = resolve_order_by_value(
            base_fields,
            output_fields,
            left_base,
            left_projected,
            &item.expr,
        )?;
        let right = resolve_order_by_value(
            base_fields,
            output_fields,
            right_base,
            right_projected,
            &item.expr,
        )?;
        let ordering = compare_order_by_values(&left, &right, item.descending, item.nulls_first)?;
        if ordering != Ordering::Equal {
            return Ok(ordering);
        }
    }
    Ok(Ordering::Equal)
}

fn resolve_order_by_value(
    base_fields: &[ResultField],
    output_fields: &[ResultField],
    base_row: &[Value],
    projected_row: &[Value],
    expr: &Expr,
) -> DbResult<Value> {
    match expr {
        Expr::Identifier(name) => {
            let column_name = name.parts.last().ok_or_else(|| {
                DbError::bind_error(SqlState::UndefinedColumn, "empty identifier is not allowed")
            })?;

            if let Ok(index) = find_column_index(output_fields, column_name) {
                return Ok(projected_row[index].clone());
            }

            let index = find_column_index(base_fields, column_name)?;
            Ok(base_row[index].clone())
        }
        Expr::Literal(Literal::Integer(position), _) => {
            let Some(index) = order_by_position_to_index(*position, output_fields.len()) else {
                return Err(DbError::bind_error(
                    SqlState::SyntaxError,
                    format!("ORDER BY position {position} is out of range"),
                ));
            };
            Ok(projected_row[index].clone())
        }
        Expr::Literal(literal, _) => Ok(literal_to_value(literal)),
        Expr::Cast { .. } => resolve_value(base_fields, base_row, expr),
        Expr::FunctionCall { name, args, .. }
            if name.parts.last().map(|s| s.as_str()) == Some("__aiondb_type_hint")
                && !args.is_empty() =>
        {
            resolve_order_by_value(
                base_fields,
                output_fields,
                base_row,
                projected_row,
                &args[0],
            )
        }
        Expr::FunctionCall { name, args, .. }
            if name.parts.last().map(|s| s.as_str()) == Some("__aiondb_char_pad_length")
                && !args.is_empty() =>
        {
            resolve_order_by_value(
                base_fields,
                output_fields,
                base_row,
                projected_row,
                &args[0],
            )
        }
        _ => Err(DbError::bind_error(
            SqlState::SyntaxError,
            "unsupported ORDER BY on pg_catalog virtual table",
        )),
    }
}

fn compare_order_by_values(
    left: &Value,
    right: &Value,
    descending: bool,
    nulls_first: Option<bool>,
) -> DbResult<Ordering> {
    let nulls_first = nulls_first.unwrap_or(descending);
    match (left.is_null(), right.is_null()) {
        (true, true) => Ok(Ordering::Equal),
        (true, false) => Ok(if nulls_first {
            Ordering::Less
        } else {
            Ordering::Greater
        }),
        (false, true) => Ok(if nulls_first {
            Ordering::Greater
        } else {
            Ordering::Less
        }),
        (false, false) => {
            let ordering = compare_runtime_values(left, right)?.unwrap_or(Ordering::Equal);
            Ok(if descending {
                ordering.reverse()
            } else {
                ordering
            })
        }
    }
}
