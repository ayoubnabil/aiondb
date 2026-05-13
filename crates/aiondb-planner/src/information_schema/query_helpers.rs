use std::cmp::Ordering;

use aiondb_core::{DbError, DbResult, SqlState, Value};
use aiondb_eval::compare_runtime_values;
use aiondb_parser::{BinaryOperator, Expr, Literal};
use aiondb_plan::{ResultField, TypedExpr};

pub(crate) fn evaluate_binary_value(
    op: BinaryOperator,
    left: Value,
    right: Value,
) -> DbResult<Value> {
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

pub(crate) fn find_column_index(fields: &[ResultField], name: &str) -> DbResult<usize> {
    fields
        .iter()
        .position(|field| field.name.eq_ignore_ascii_case(name))
        .ok_or_else(|| {
            DbError::bind_error(
                SqlState::UndefinedColumn,
                format!("column \"{name}\" does not exist"),
            )
        })
}

pub(crate) fn literal_to_value(literal: &Literal) -> Value {
    match literal {
        Literal::Integer(value) => {
            if let Ok(i) = i32::try_from(*value) {
                Value::Int(i)
            } else {
                Value::BigInt(*value)
            }
        }
        Literal::NumericLit(value) => {
            if let Ok(numeric) = value.parse::<aiondb_core::NumericValue>() {
                Value::Numeric(numeric)
            } else if let Ok(double) = value.parse::<f64>() {
                Value::Double(double)
            } else {
                Value::Null
            }
        }
        Literal::String(value) => Value::Text(value.clone()),
        Literal::Boolean(value) => Value::Boolean(*value),
        Literal::Null => Value::Null,
    }
}

pub(crate) fn rows_to_typed(fields: &[ResultField], rows: Vec<Vec<Value>>) -> Vec<Vec<TypedExpr>> {
    rows.into_iter()
        .map(|row| {
            row.into_iter()
                .zip(fields.iter())
                .map(|(value, field)| {
                    TypedExpr::literal(value, field.data_type.clone(), field.nullable)
                })
                .collect()
        })
        .collect()
}

pub(crate) fn is_star_expr(expr: &Expr) -> bool {
    matches!(expr, Expr::Identifier(name) if name.parts.len() == 1 && name.parts[0] == "*")
}

/// Walk `expr` looking for a `count(...)` aggregate call. Both the
/// `information_schema` and `pg_catalog` virtual-query paths only support
/// `count` over their in-memory rows, so they share this scan.
pub(crate) fn expr_contains_count_aggregate(expr: &Expr) -> bool {
    let mut stack = vec![expr];
    while let Some(expr) = stack.pop() {
        match expr {
            Expr::FunctionCall {
                name, args, filter, ..
            } => {
                if name
                    .parts
                    .last()
                    .is_some_and(|name| name.eq_ignore_ascii_case("count"))
                {
                    return true;
                }
                stack.extend(args);
                if let Some(filter) = filter {
                    stack.push(filter);
                }
            }
            Expr::BinaryOp { left, right, .. } | Expr::IsDistinctFrom { left, right, .. } => {
                stack.push(right);
                stack.push(left);
            }
            Expr::Cast { expr, .. } | Expr::UnaryOp { expr, .. } | Expr::IsNull { expr, .. } => {
                stack.push(expr);
            }
            Expr::Like { expr, pattern, .. } => {
                stack.push(pattern);
                stack.push(expr);
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
                if let Some(else_result) = else_result {
                    stack.push(else_result);
                }
                stack.extend(results);
                stack.extend(conditions);
                if let Some(operand) = operand {
                    stack.push(operand);
                }
            }
            Expr::Array { elements, .. } => stack.extend(elements),
            Expr::WindowFunction { function, .. } => stack.push(function),
            Expr::Literal(_, _)
            | Expr::Identifier(_)
            | Expr::Parameter { .. }
            | Expr::Default { .. }
            | Expr::ArraySubquery { .. }
            | Expr::Subquery { .. }
            | Expr::InSubquery { .. }
            | Expr::Exists { .. }
            | Expr::CypherExists { .. }
            | Expr::CypherPatternComprehension { .. } => {}
        }
    }
    false
}
