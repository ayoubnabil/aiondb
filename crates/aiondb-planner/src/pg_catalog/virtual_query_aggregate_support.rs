use super::*;

const MAX_VIRTUAL_AGGREGATE_EXPR_DEPTH: usize = 256;

pub(super) fn resolve_aggregate_value(
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
            "pg_catalog aggregate projection expression is too deeply nested",
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
            "unsupported aggregate projection on pg_catalog virtual table",
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
                    if !row_matches_selection(fields, row, Some(filter))? {
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
                let value = resolve_value(fields, row, arg)?;
                if matches!(value, Value::Null) {
                    continue;
                }
                if distinct {
                    if distinct_values.contains(&value) {
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
                "function \"{function_name}\" is not supported in pg_catalog virtual aggregates"
            ),
        )),
    }
}
