use std::cmp::Ordering;

use aiondb_core::TextTypeModifier;
use aiondb_eval::compare_runtime_values;
use aiondb_parser::{BinaryOperator, Expr, Literal, OrderByItem, SelectItem, SelectStatement};

use super::query_helpers::{
    evaluate_binary_value, expr_contains_count_aggregate, find_column_index, is_star_expr,
    literal_to_value,
};
use super::*;

const MAX_VIRTUAL_AGGREGATE_EXPR_DEPTH: usize = 256;

pub(crate) fn build_select_plan(
    catalog: &Arc<dyn CatalogReader>,
    txn_id: TxnId,
    select: &SelectStatement,
    default_schema: Option<&str>,
    database_name: Option<&str>,
) -> DbResult<Option<LogicalPlan>> {
    let Some(table_name) = extract_table_name(select) else {
        return Ok(None);
    };
    if !is_supported_select_shape(select) {
        return Ok(None);
    }

    let (base_fields, base_rows) = match table_name.to_ascii_lowercase().as_str() {
        TABLES_TABLE => (
            tables_output_fields(),
            build_tables_rows(catalog, txn_id, default_schema, database_name)?,
        ),
        COLUMNS_TABLE => (
            columns_output_fields(),
            build_columns_rows(catalog, txn_id, default_schema, database_name)?,
        ),
        SCHEMATA_TABLE => (
            schemata_output_fields(),
            build_schemata_rows(catalog, txn_id, default_schema, database_name)?,
        ),
        VIEWS_TABLE => (
            views_output_fields(),
            build_views_rows(catalog, txn_id, default_schema, database_name)?,
        ),
        SEQUENCES_TABLE => (
            sequences_output_fields(),
            build_sequences_rows(catalog, txn_id, default_schema, database_name)?,
        ),
        TRIGGERS_TABLE => (
            triggers::triggers_output_fields(),
            triggers::build_triggers_rows(catalog, txn_id, default_schema, database_name)?,
        ),
        _ => return Ok(None),
    };

    let fast_path_plan: DbResult<LogicalPlan> = (|| {
        let filtered_rows =
            filtering::apply_selection(&base_fields, base_rows, select.selection.as_ref())?;
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
            .map(info_schema_expr_to_typed)
            .transpose()?;
        let typed_offset = select
            .offset
            .as_ref()
            .map(info_schema_expr_to_typed)
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
        Err(err) if should_fallback_to_general_information_schema_binder(&err) => Ok(None),
        Err(err) => Err(err),
    }
}

fn is_supported_select_shape(select: &SelectStatement) -> bool {
    select.ctes.is_empty()
        && select.joins.is_empty()
        && select.group_by.is_empty()
        && select.having.is_none()
        && matches!(select.distinct, aiondb_parser::DistinctKind::All)
}

fn should_fallback_to_general_information_schema_binder(err: &DbError) -> bool {
    matches!(err, DbError::Parse(_) | DbError::Bind(_))
}

fn extract_table_name(select: &SelectStatement) -> Option<&str> {
    let from = select.from.as_ref()?;
    match from.parts.as_slice() {
        [schema, table] if is_information_schema(schema) => Some(table),
        _ => None,
    }
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

    if items
        .iter()
        .any(|item| expr_contains_count_aggregate(&item.expr))
    {
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
                .map(|item| filtering::resolve_value(fields, &row, &item.expr))
                .collect::<DbResult<Vec<_>>>()
        })
        .collect::<DbResult<Vec<_>>>()?;
    Ok((output_fields, projected_rows))
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
            let position_usize = usize::try_from(*position).unwrap_or(usize::MAX);
            if *position <= 0 || position_usize > output_fields.len() {
                return Err(DbError::bind_error(
                    SqlState::SyntaxError,
                    format!("ORDER BY position {} is out of range", position),
                ));
            }
            Ok(projected_row[position_usize - 1].clone())
        }
        Expr::Literal(literal, _) => Ok(literal_to_value(literal)),
        Expr::Cast { expr, .. } => {
            resolve_order_by_value(base_fields, output_fields, base_row, projected_row, expr)
        }
        Expr::FunctionCall { name, args, .. }
            if name.parts.last().is_some_and(|part| {
                part.eq_ignore_ascii_case("__aiondb_type_hint")
                    || part.eq_ignore_ascii_case("__aiondb_char_pad_length")
            }) && !args.is_empty() =>
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
            "unsupported ORDER BY on information_schema virtual table",
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

fn project_output_fields(
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
                    name: item.alias.clone().unwrap_or_else(|| "?column?".to_owned()),
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
                Expr::Literal(Literal::Integer(_), _) => Ok(ResultField {
                    name: item.alias.clone().unwrap_or_else(|| "?column?".to_owned()),
                    data_type: DataType::Int,
                    text_type_modifier: None,
                    nullable: false,
                }),
                Expr::Literal(Literal::Boolean(_), _) => Ok(ResultField {
                    name: item.alias.clone().unwrap_or_else(|| "?column?".to_owned()),
                    data_type: DataType::Boolean,
                    text_type_modifier: None,
                    nullable: false,
                }),
                Expr::Literal(Literal::NumericLit(raw), _) => Ok(ResultField {
                    name: item.alias.clone().unwrap_or_else(|| "?column?".to_owned()),
                    data_type: if raw.parse::<aiondb_core::NumericValue>().is_ok() {
                        DataType::Numeric
                    } else {
                        DataType::Double
                    },
                    text_type_modifier: None,
                    nullable: false,
                }),
                Expr::Literal(_, _) => Ok(ResultField {
                    name: item.alias.clone().unwrap_or_else(|| "?column?".to_owned()),
                    data_type: DataType::Text,
                    text_type_modifier: None,
                    nullable: true,
                }),
                Expr::FunctionCall { name, .. }
                    if name
                        .parts
                        .last()
                        .is_some_and(|name| name.eq_ignore_ascii_case("count")) =>
                {
                    Ok(ResultField {
                        name: item.alias.clone().unwrap_or_else(|| {
                            name.parts
                                .last()
                                .cloned()
                                .unwrap_or_else(|| "?column?".to_owned())
                        }),
                        data_type: DataType::BigInt,
                        text_type_modifier: None,
                        nullable: false,
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
                    "unsupported projection on information_schema virtual table",
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

fn resolve_aggregate_value(
    fields: &[ResultField],
    rows: &[Vec<Value>],
    expr: &Expr,
) -> DbResult<Value> {
    resolve_aggregate_value_depth(fields, rows, expr, 0)
}

fn resolve_aggregate_value_depth(
    fields: &[ResultField],
    rows: &[Vec<Value>],
    expr: &Expr,
    depth: usize,
) -> DbResult<Value> {
    if depth > MAX_VIRTUAL_AGGREGATE_EXPR_DEPTH {
        return Err(DbError::program_limit(
            "information_schema aggregate projection expression is too deeply nested",
        ));
    }
    match expr {
        Expr::Literal(literal, _) => Ok(literal_to_value(literal)),
        Expr::Cast {
            expr, data_type, ..
        } => {
            let value = resolve_aggregate_value_depth(fields, rows, expr, depth + 1)?;
            match data_type {
                DataType::Text => Ok(Value::Text(value.to_string())),
                _ => Ok(value),
            }
        }
        Expr::BinaryOp {
            left, op, right, ..
        } => {
            let left = resolve_aggregate_value_depth(fields, rows, left, depth + 1)?;
            let right = resolve_aggregate_value_depth(fields, rows, right, depth + 1)?;
            evaluate_binary_value(*op, left, right)
        }
        Expr::FunctionCall {
            name,
            args,
            distinct,
            filter,
            ..
        } => {
            resolve_aggregate_function_value(fields, rows, name, args, *distinct, filter.as_deref())
        }
        _ => Err(DbError::bind_error(
            SqlState::SyntaxError,
            "unsupported aggregate projection on information_schema virtual table",
        )),
    }
}

fn resolve_aggregate_function_value(
    fields: &[ResultField],
    rows: &[Vec<Value>],
    name: &aiondb_parser::ObjectName,
    args: &[Expr],
    distinct: bool,
    filter: Option<&Expr>,
) -> DbResult<Value> {
    let function_name = name
        .parts
        .last()
        .map_or("", String::as_str)
        .to_ascii_lowercase();
    match function_name.as_str() {
        "count" => {
            let mut count = 0_i64;
            let mut distinct_values = Vec::new();
            let count_star = args.len() == 1 && is_star_expr(&args[0]);

            for row in rows {
                if let Some(filter) = filter {
                    if !filtering::row_matches_selection(fields, row, Some(filter))? {
                        continue;
                    }
                }

                if count_star {
                    count += 1;
                    continue;
                }

                let Some(arg) = args.first() else {
                    continue;
                };
                let value = filtering::resolve_value(fields, row, arg)?;
                if matches!(value, Value::Null) {
                    continue;
                }
                if distinct {
                    if distinct_values.iter().any(|existing| existing == &value) {
                        continue;
                    }
                    distinct_values.push(value);
                }
                count += 1;
            }

            Ok(Value::BigInt(count))
        }
        _ => Err(DbError::bind_error(
            SqlState::FeatureNotSupported,
            format!(
                "function \"{function_name}\" is not supported in information_schema virtual aggregates"
            ),
        )),
    }
}

fn info_schema_expr_to_typed(expr: &Expr) -> DbResult<TypedExpr> {
    match expr {
        Expr::Literal(Literal::Integer(n), _) => Ok(TypedExpr::literal(
            Value::BigInt(*n),
            DataType::BigInt,
            false,
        )),
        _ => Err(DbError::bind_error(
            SqlState::SyntaxError,
            "only integer literals are supported for LIMIT/OFFSET on information_schema queries",
        )),
    }
}
