use aiondb_catalog::{QualifiedName, TableDescriptor};
use aiondb_core::TextTypeModifier;
use aiondb_parser::{BinaryOperator, Expr, Literal, OrderByItem, SelectItem};

use super::*;
use crate::information_schema::query_helpers::find_column_index;

#[inline]
pub(super) fn order_by_position_to_index(position: i64, len: usize) -> Option<usize> {
    let one_based = usize::try_from(position).ok()?;
    if one_based == 0 || one_based > len {
        None
    } else {
        Some(one_based - 1)
    }
}

pub(super) fn project_output_fields(
    fields: &[ResultField],
    items: &[SelectItem],
) -> DbResult<Vec<ResultField>> {
    if items.len() == 1 && is_star_expr(&items[0].expr) {
        return Ok(fields.to_vec());
    }

    items
        .iter()
        .map(|item| {
            if let Some((data_type, text_type_modifier)) = explicit_cast_metadata(&item.expr) {
                return Ok(ResultField {
                    name: item.alias.clone().unwrap_or_else(|| match &item.expr {
                        Expr::FunctionCall { name, args, .. }
                            if (name.parts.last().map(|s| s.as_str())
                                == Some("__aiondb_type_hint")
                                || name.parts.last().map(|s| s.as_str())
                                    == Some("__aiondb_char_pad_length"))
                                && !args.is_empty() =>
                        {
                            innermost_ident_through_type_hints(&args[0], true)
                                .unwrap_or_else(|| "?column?".to_owned())
                        }
                        Expr::Cast { expr, .. } => innermost_ident_through_casts(expr)
                            .unwrap_or_else(|| "?column?".to_owned()),
                        _ => "?column?".to_owned(),
                    }),
                    data_type,
                    text_type_modifier,
                    nullable: true,
                });
            }

            match &item.expr {
                Expr::Identifier(name) => {
                    let column_name = name.parts.last().ok_or_else(|| {
                        DbError::bind_error(
                            SqlState::UndefinedColumn,
                            "empty identifier is not allowed",
                        )
                    })?;
                    let field = &fields[find_column_index(fields, column_name)?];
                    Ok(ResultField {
                        name: item.alias.clone().unwrap_or_else(|| field.name.clone()),
                        data_type: field.data_type.clone(),
                        text_type_modifier: field.text_type_modifier,
                        nullable: field.nullable,
                    })
                }
                Expr::Cast { .. } | Expr::FunctionCall { .. } | Expr::Literal(_, _) => {
                    Ok(ResultField {
                        name: item.alias.clone().unwrap_or_else(|| match &item.expr {
                            Expr::FunctionCall { name, args, .. }
                                if name.parts.last().map(|s| s.as_str())
                                    == Some("__aiondb_type_hint")
                                    && !args.is_empty() =>
                            {
                                // __aiondb_type_hint wraps a cast expression;
                                // derive the column name from the inner expression.
                                innermost_ident_through_type_hints(&args[0], false)
                                    .unwrap_or_else(|| "?column?".to_owned())
                            }
                            Expr::FunctionCall { name, .. } => name
                                .parts
                                .last()
                                .cloned()
                                .unwrap_or_else(|| "?column?".to_owned()),
                            Expr::Cast { expr, .. } => {
                                // For cast expressions, use the innermost identifier
                                // name (e.g. attrelid::regclass -> "attrelid").
                                innermost_ident_through_casts(expr)
                                    .unwrap_or_else(|| "?column?".to_owned())
                            }
                            _ => "?column?".to_owned(),
                        }),
                        data_type: match &item.expr {
                            Expr::Literal(Literal::Integer(_), _) => DataType::Int,
                            Expr::Literal(Literal::Boolean(_), _) => DataType::Boolean,
                            Expr::Literal(Literal::NumericLit(raw), _) => {
                                if raw.parse::<aiondb_core::NumericValue>().is_ok() {
                                    DataType::Numeric
                                } else {
                                    DataType::Double
                                }
                            }
                            Expr::FunctionCall { name, .. }
                                if name.parts.last().is_some_and(|name| {
                                    name.eq_ignore_ascii_case("pg_log_backend_memory_contexts")
                                }) =>
                            {
                                DataType::Boolean
                            }
                            Expr::FunctionCall { name, .. }
                                if name
                                    .parts
                                    .last()
                                    .is_some_and(|name| name.eq_ignore_ascii_case("count")) =>
                            {
                                DataType::BigInt
                            }
                            _ => DataType::Text,
                        },
                        text_type_modifier: None,
                        nullable: true,
                    })
                }
                Expr::BinaryOp {
                    op:
                        BinaryOperator::Eq
                        | BinaryOperator::Ne
                        | BinaryOperator::Lt
                        | BinaryOperator::Le
                        | BinaryOperator::Gt
                        | BinaryOperator::Ge,
                    ..
                } => Ok(ResultField {
                    name: item.alias.clone().unwrap_or_else(|| "?column?".to_owned()),
                    data_type: DataType::Boolean,
                    text_type_modifier: None,
                    nullable: true,
                }),
                _ => Err(DbError::bind_error(
                    SqlState::SyntaxError,
                    "unsupported projection on pg_catalog virtual table",
                )),
            }
        })
        .collect()
}

fn explicit_cast_metadata(expr: &Expr) -> Option<(DataType, Option<TextTypeModifier>)> {
    let mut current = expr;
    for _ in 0..=256 {
        match current {
            Expr::Cast { data_type, .. } => {
                return Some((
                    data_type.clone(),
                    crate::type_check::cast_text_type_modifier(expr),
                ));
            }
            Expr::FunctionCall { name, args, .. }
                if name.parts.last().is_some_and(|part| {
                    part.eq_ignore_ascii_case("__aiondb_type_hint")
                        || part.eq_ignore_ascii_case("__aiondb_char_pad_length")
                }) =>
            {
                current = args.first()?;
            }
            _ => return None,
        }
    }
    None
}

fn innermost_ident_through_type_hints(
    mut expr: &Expr,
    include_char_pad_length: bool,
) -> Option<String> {
    for _ in 0..=256 {
        match expr {
            Expr::Cast { expr: inner, .. } => expr = inner,
            Expr::FunctionCall { name, args, .. }
                if !args.is_empty()
                    && name.parts.last().is_some_and(|part| {
                        part.eq_ignore_ascii_case("__aiondb_type_hint")
                            || (include_char_pad_length
                                && part.eq_ignore_ascii_case("__aiondb_char_pad_length"))
                    }) =>
            {
                expr = &args[0];
            }
            Expr::Identifier(name) => return name.parts.last().cloned(),
            _ => return None,
        }
    }
    None
}

fn innermost_ident_through_casts(mut expr: &Expr) -> Option<String> {
    for _ in 0..=256 {
        match expr {
            Expr::Cast { expr: inner, .. } => expr = inner,
            Expr::Identifier(name) => return name.parts.last().cloned(),
            _ => return None,
        }
    }
    None
}

pub(super) fn literal_to_value(literal: &Literal) -> Value {
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

pub(super) fn rows_to_typed(fields: &[ResultField], rows: Vec<Vec<Value>>) -> Vec<Vec<TypedExpr>> {
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

pub(super) use crate::information_schema::query_helpers::is_star_expr;

pub(super) fn virtual_expr_to_typed(expr: &Expr) -> DbResult<TypedExpr> {
    match expr {
        Expr::Literal(Literal::Integer(value), _) => {
            if let Ok(i) = i32::try_from(*value) {
                Ok(TypedExpr::literal(Value::Int(i), DataType::Int, false))
            } else {
                Ok(TypedExpr::literal(
                    Value::BigInt(*value),
                    DataType::BigInt,
                    false,
                ))
            }
        }
        _ => Err(DbError::bind_error(
            SqlState::SyntaxError,
            "only integer literals are supported for LIMIT/OFFSET on pg_catalog queries",
        )),
    }
}

pub(super) fn dynamic_virtual_relation_descriptor(
    table_name: &str,
    from_alias: Option<&str>,
) -> DbResult<TableDescriptor> {
    let mut relation = build_table_descriptor(table_name).ok_or_else(|| {
        DbError::bind_error(
            SqlState::UndefinedTable,
            format!("pg_catalog relation \"{table_name}\" does not exist"),
        )
    })?;
    if let Some(alias) = from_alias {
        relation.name = QualifiedName::unqualified(alias);
    }
    Ok(relation)
}

pub(super) fn build_dynamic_projection_outputs(
    fields: &[ResultField],
    relation: &TableDescriptor,
    items: &[SelectItem],
) -> DbResult<Vec<aiondb_plan::ProjectionExpr>> {
    if items.len() == 1 && is_star_expr(&items[0].expr) {
        return Ok(fields
            .iter()
            .enumerate()
            .map(|(index, field)| aiondb_plan::ProjectionExpr {
                field: field.clone(),
                expr: TypedExpr::column_ref(
                    field.name.clone(),
                    index,
                    field.data_type.clone(),
                    field.nullable,
                ),
            })
            .collect());
    }

    let projected_fields = project_output_fields(fields, items)?;
    items
        .iter()
        .zip(projected_fields)
        .map(|(item, field)| {
            let expr = crate::type_check_expression_with_relation(&item.expr, relation)?;
            Ok(aiondb_plan::ProjectionExpr {
                field: ResultField {
                    name: field.name,
                    data_type: expr.data_type.clone(),
                    text_type_modifier: field.text_type_modifier,
                    nullable: expr.nullable,
                },
                expr,
            })
        })
        .collect()
}

pub(super) fn build_dynamic_order_by(
    relation: &TableDescriptor,
    outputs: &[aiondb_plan::ProjectionExpr],
    order_by: &[OrderByItem],
) -> DbResult<Vec<aiondb_plan::SortExpr>> {
    order_by
        .iter()
        .map(|item| {
            let expr = match &item.expr {
                Expr::Literal(Literal::Integer(position), _) => {
                    let Some(index) = order_by_position_to_index(*position, outputs.len()) else {
                        return Err(DbError::bind_error(
                            SqlState::SyntaxError,
                            format!("ORDER BY position {position} is out of range"),
                        ));
                    };
                    outputs[index].expr.clone()
                }
                Expr::Identifier(name) if name.parts.len() == 1 => {
                    let order_name = name.parts[0].clone();
                    outputs
                        .iter()
                        .find(|projection| projection.field.name.eq_ignore_ascii_case(&order_name))
                        .map(|projection| projection.expr.clone())
                        .unwrap_or(crate::type_check_expression_with_relation(
                            &item.expr, relation,
                        )?)
                }
                _ => crate::type_check_expression_with_relation(&item.expr, relation)?,
            };
            Ok(aiondb_plan::SortExpr {
                expr,
                descending: item.descending,
                nulls_first: item.nulls_first,
            })
        })
        .collect()
}

pub(super) fn expr_contains_aggregate_typed(expr: &TypedExpr) -> bool {
    let mut stack = vec![expr];
    while let Some(expr) = stack.pop() {
        match &expr.kind {
            aiondb_plan::TypedExprKind::AggCount { .. }
            | aiondb_plan::TypedExprKind::AggSum { .. }
            | aiondb_plan::TypedExprKind::AggAvg { .. }
            | aiondb_plan::TypedExprKind::AggAnyValue { .. }
            | aiondb_plan::TypedExprKind::AggMin { .. }
            | aiondb_plan::TypedExprKind::AggMax { .. }
            | aiondb_plan::TypedExprKind::AggStringAgg { .. }
            | aiondb_plan::TypedExprKind::AggArrayAgg { .. }
            | aiondb_plan::TypedExprKind::AggBoolAnd { .. }
            | aiondb_plan::TypedExprKind::AggBoolOr { .. }
            | aiondb_plan::TypedExprKind::AggStddevPop { .. }
            | aiondb_plan::TypedExprKind::AggStddevSamp { .. }
            | aiondb_plan::TypedExprKind::AggVarPop { .. }
            | aiondb_plan::TypedExprKind::AggVarSamp { .. } => return true,
            aiondb_plan::TypedExprKind::BinaryEq { left, right }
            | aiondb_plan::TypedExprKind::BinaryNe { left, right }
            | aiondb_plan::TypedExprKind::BinaryGt { left, right }
            | aiondb_plan::TypedExprKind::BinaryGe { left, right }
            | aiondb_plan::TypedExprKind::BinaryLt { left, right }
            | aiondb_plan::TypedExprKind::BinaryLe { left, right }
            | aiondb_plan::TypedExprKind::LogicalAnd { left, right }
            | aiondb_plan::TypedExprKind::LogicalOr { left, right }
            | aiondb_plan::TypedExprKind::ArithAdd { left, right }
            | aiondb_plan::TypedExprKind::ArithSub { left, right }
            | aiondb_plan::TypedExprKind::ArithMul { left, right }
            | aiondb_plan::TypedExprKind::ArithDiv { left, right }
            | aiondb_plan::TypedExprKind::ArithMod { left, right }
            | aiondb_plan::TypedExprKind::Concat { left, right }
            | aiondb_plan::TypedExprKind::JsonGet { left, right }
            | aiondb_plan::TypedExprKind::JsonGetText { left, right }
            | aiondb_plan::TypedExprKind::JsonPathGet { left, right }
            | aiondb_plan::TypedExprKind::JsonPathGetText { left, right }
            | aiondb_plan::TypedExprKind::JsonContains { left, right }
            | aiondb_plan::TypedExprKind::JsonContainedBy { left, right }
            | aiondb_plan::TypedExprKind::JsonKeyExists { left, right }
            | aiondb_plan::TypedExprKind::JsonAnyKeyExists { left, right }
            | aiondb_plan::TypedExprKind::JsonAllKeysExist { left, right }
            | aiondb_plan::TypedExprKind::ArrayConcat { left, right }
            | aiondb_plan::TypedExprKind::ArrayContains { left, right }
            | aiondb_plan::TypedExprKind::ArrayContainedBy { left, right }
            | aiondb_plan::TypedExprKind::ArrayOverlap { left, right }
            | aiondb_plan::TypedExprKind::Nullif { left, right }
            | aiondb_plan::TypedExprKind::IsDistinctFrom { left, right, .. } => {
                stack.push(right);
                stack.push(left);
            }
            aiondb_plan::TypedExprKind::LogicalNot { expr }
            | aiondb_plan::TypedExprKind::Negate { expr }
            | aiondb_plan::TypedExprKind::IsNull { expr, .. }
            | aiondb_plan::TypedExprKind::Cast { expr, .. } => stack.push(expr),
            aiondb_plan::TypedExprKind::Like { expr, pattern, .. } => {
                stack.push(pattern);
                stack.push(expr);
            }
            aiondb_plan::TypedExprKind::Between {
                expr, low, high, ..
            } => {
                stack.push(high);
                stack.push(low);
                stack.push(expr);
            }
            aiondb_plan::TypedExprKind::CaseWhen {
                conditions,
                results,
                else_result,
            } => {
                if let Some(else_result) = else_result {
                    stack.push(else_result);
                }
                stack.extend(results);
                stack.extend(conditions);
            }
            aiondb_plan::TypedExprKind::Coalesce { args } => stack.extend(args),
            aiondb_plan::TypedExprKind::ArrayConstruct { elements } => stack.extend(elements),
            aiondb_plan::TypedExprKind::ScalarFunction { args, .. }
            | aiondb_plan::TypedExprKind::UserFunction { args, .. } => stack.extend(args),
            aiondb_plan::TypedExprKind::InList { expr, list, .. } => {
                stack.extend(list);
                stack.push(expr);
            }
            aiondb_plan::TypedExprKind::Literal(_)
            | aiondb_plan::TypedExprKind::ColumnRef { .. }
            | aiondb_plan::TypedExprKind::OuterColumnRef { .. }
            | aiondb_plan::TypedExprKind::NextValue { .. }
            | aiondb_plan::TypedExprKind::ArraySubquery { .. }
            | aiondb_plan::TypedExprKind::ScalarSubquery { .. }
            | aiondb_plan::TypedExprKind::InSubquery { .. }
            | aiondb_plan::TypedExprKind::ExistsSubquery { .. }
            | aiondb_plan::TypedExprKind::WindowFunction { .. } => {}
        }
    }
    false
}
