use super::expr::infer_expr;
use super::expr_helpers::infer_expr_with_expected;
use super::*;

use aiondb_plan::WindowFunctionKind;

/// Infer the type of a window function expression.
///
/// Resolves `func(...) OVER (PARTITION BY ... ORDER BY ...)` by:
/// 1. Identifying the window function kind (`row_number`, `rank`, `sum`, etc.)
/// 2. Type-checking the arguments
/// 3. Type-checking partition-by and order-by expressions
/// 4. Producing a `TypedExprKind::WindowFunction`
pub(super) fn infer_window_function(
    function: &Expr,
    partition_by: &[Expr],
    order_by: &[aiondb_parser::OrderByItem],
    relation: Option<&TableDescriptor>,
    params: &mut ParameterTypes,
    sq: Option<SubqueryResolver<'_>>,
    uf: Option<UserFunctionResolver<'_>>,
    span: aiondb_parser::Span,
) -> DbResult<TypedExpr> {
    let Expr::FunctionCall { name, args, .. } = function else {
        return Err(DbError::Bind(Box::new(
            ErrorReport::new(
                SqlState::SyntaxError,
                "OVER clause requires a function call",
            )
            .with_position(span.start + 1),
        )));
    };

    let func_name = name.parts.last().map_or("", |segment| segment.as_str());
    let (kind, typed_args, result_type, nullable) =
        resolve_window_kind(func_name, args, relation, params, sq, uf, span)?;

    let typed_partition_by = partition_by
        .iter()
        .map(|e| infer_expr(e, relation, params, sq, uf))
        .collect::<DbResult<Vec<_>>>()?;

    let typed_order_by = order_by
        .iter()
        .map(|item| {
            let typed = infer_expr(&item.expr, relation, params, sq, uf)?;
            ensure_orderable_sort_expr(&typed)?;
            Ok(SortExpr {
                expr: typed,
                descending: item.descending,
                nulls_first: item.nulls_first,
            })
        })
        .collect::<DbResult<Vec<_>>>()?;

    Ok(TypedExpr {
        kind: TypedExprKind::WindowFunction {
            func: kind,
            args: typed_args,
            partition_by: typed_partition_by,
            order_by: typed_order_by,
        },
        data_type: result_type,
        nullable,
    })
}

fn resolve_window_kind(
    func_name: &str,
    args: &[Expr],
    relation: Option<&TableDescriptor>,
    params: &mut ParameterTypes,
    sq: Option<SubqueryResolver<'_>>,
    uf: Option<UserFunctionResolver<'_>>,
    span: aiondb_parser::Span,
) -> DbResult<(WindowFunctionKind, Vec<TypedExpr>, DataType, bool)> {
    match func_name.to_ascii_lowercase().as_str() {
        "row_number" => {
            expect_no_args(func_name, args, span)?;
            Ok((
                WindowFunctionKind::RowNumber,
                vec![],
                DataType::BigInt,
                false,
            ))
        }
        "rank" => {
            expect_no_args(func_name, args, span)?;
            Ok((WindowFunctionKind::Rank, vec![], DataType::BigInt, false))
        }
        "dense_rank" => {
            expect_no_args(func_name, args, span)?;
            Ok((
                WindowFunctionKind::DenseRank,
                vec![],
                DataType::BigInt,
                false,
            ))
        }
        "percent_rank" => {
            expect_no_args(func_name, args, span)?;
            Ok((
                WindowFunctionKind::PercentRank,
                vec![],
                DataType::Double,
                false,
            ))
        }
        "cume_dist" => {
            expect_no_args(func_name, args, span)?;
            Ok((
                WindowFunctionKind::CumeDist,
                vec![],
                DataType::Double,
                false,
            ))
        }
        "ntile" => {
            expect_one_arg(func_name, args, span)?;
            let bucket_count = infer_expr_with_expected(
                &args[0],
                relation,
                &DataType::BigInt,
                false,
                params,
                sq,
                uf,
            )?;
            validate_assignment_expr(&bucket_count, &DataType::BigInt, false, false, "NTILE")?;
            Ok((
                WindowFunctionKind::Ntile,
                vec![bucket_count],
                DataType::BigInt,
                false,
            ))
        }
        "lag" => resolve_offset_window_kind(
            WindowFunctionKind::Lag,
            func_name,
            args,
            relation,
            params,
            sq,
            uf,
            span,
        ),
        "lead" => resolve_offset_window_kind(
            WindowFunctionKind::Lead,
            func_name,
            args,
            relation,
            params,
            sq,
            uf,
            span,
        ),
        "first_value" => {
            expect_one_arg(func_name, args, span)?;
            let typed_args = infer_window_args(args, relation, params, sq, uf)?;
            let dt = typed_args[0].data_type.clone();
            Ok((WindowFunctionKind::FirstValue, typed_args, dt.clone(), true))
        }
        "last_value" => {
            expect_one_arg(func_name, args, span)?;
            let typed_args = infer_window_args(args, relation, params, sq, uf)?;
            let dt = typed_args[0].data_type.clone();
            Ok((WindowFunctionKind::LastValue, typed_args, dt.clone(), true))
        }
        "nth_value" => {
            if args.len() != 2 {
                return Err(DbError::Bind(Box::new(
                    ErrorReport::new(
                        SqlState::SyntaxError,
                        format!("{func_name}() requires exactly two arguments"),
                    )
                    .with_position(span.start + 1),
                )));
            }
            let value_expr = infer_expr(&args[0], relation, params, sq, uf)?;
            let dt = value_expr.data_type.clone();
            let nth_expr = infer_expr_with_expected(
                &args[1],
                relation,
                &DataType::BigInt,
                false,
                params,
                sq,
                uf,
            )?;
            validate_assignment_expr(&nth_expr, &DataType::BigInt, false, false, "NTH_VALUE")?;
            Ok((
                WindowFunctionKind::NthValue,
                vec![value_expr, nth_expr],
                dt,
                true,
            ))
        }
        "count" => {
            let is_star = args.len() == 1
                && matches!(&args[0], Expr::Identifier(n) if n.parts.len() == 1 && n.parts[0] == "*");
            let typed_args = if is_star {
                vec![]
            } else {
                infer_window_args(args, relation, params, sq, uf)?
            };
            Ok((
                WindowFunctionKind::Count,
                typed_args,
                DataType::BigInt,
                false,
            ))
        }
        "sum" => {
            expect_one_arg(func_name, args, span)?;
            let typed_args = infer_window_args(args, relation, params, sq, uf)?;
            let dt = typed_args[0].data_type.clone();
            Ok((WindowFunctionKind::Sum, typed_args, dt, true))
        }
        "avg" => {
            expect_one_arg(func_name, args, span)?;
            let typed_args = infer_window_args(args, relation, params, sq, uf)?;
            Ok((WindowFunctionKind::Avg, typed_args, DataType::Double, true))
        }
        "min" => {
            expect_one_arg(func_name, args, span)?;
            let typed_args = infer_window_args(args, relation, params, sq, uf)?;
            let dt = typed_args[0].data_type.clone();
            Ok((WindowFunctionKind::Min, typed_args, dt, true))
        }
        "max" => {
            expect_one_arg(func_name, args, span)?;
            let typed_args = infer_window_args(args, relation, params, sq, uf)?;
            let dt = typed_args[0].data_type.clone();
            Ok((WindowFunctionKind::Max, typed_args, dt, true))
        }
        "var_pop" => {
            expect_one_arg(func_name, args, span)?;
            let typed_args = infer_window_args(args, relation, params, sq, uf)?;
            Ok((
                WindowFunctionKind::VarPop,
                typed_args,
                DataType::Double,
                true,
            ))
        }
        "var_samp" | "variance" => {
            expect_one_arg(func_name, args, span)?;
            let typed_args = infer_window_args(args, relation, params, sq, uf)?;
            Ok((
                WindowFunctionKind::VarSamp,
                typed_args,
                DataType::Double,
                true,
            ))
        }
        "stddev_pop" => {
            expect_one_arg(func_name, args, span)?;
            let typed_args = infer_window_args(args, relation, params, sq, uf)?;
            Ok((
                WindowFunctionKind::StddevPop,
                typed_args,
                DataType::Double,
                true,
            ))
        }
        "stddev_samp" | "stddev" => {
            expect_one_arg(func_name, args, span)?;
            let typed_args = infer_window_args(args, relation, params, sq, uf)?;
            Ok((
                WindowFunctionKind::StddevSamp,
                typed_args,
                DataType::Double,
                true,
            ))
        }
        _ => Err(DbError::Bind(Box::new(
            ErrorReport::new(
                SqlState::SyntaxError,
                format!("\"{func_name}\" is not a window function"),
            )
            .with_position(span.start + 1),
        ))),
    }
}

fn infer_window_args(
    args: &[Expr],
    relation: Option<&TableDescriptor>,
    params: &mut ParameterTypes,
    sq: Option<SubqueryResolver<'_>>,
    uf: Option<UserFunctionResolver<'_>>,
) -> DbResult<Vec<TypedExpr>> {
    args.iter()
        .map(|a| infer_expr(a, relation, params, sq, uf))
        .collect()
}

fn resolve_offset_window_kind(
    kind: WindowFunctionKind,
    func_name: &str,
    args: &[Expr],
    relation: Option<&TableDescriptor>,
    params: &mut ParameterTypes,
    sq: Option<SubqueryResolver<'_>>,
    uf: Option<UserFunctionResolver<'_>>,
    span: aiondb_parser::Span,
) -> DbResult<(WindowFunctionKind, Vec<TypedExpr>, DataType, bool)> {
    if args.is_empty() || args.len() > 3 {
        return Err(DbError::Bind(Box::new(
            ErrorReport::new(
                SqlState::SyntaxError,
                format!("{func_name}() requires between 1 and 3 arguments"),
            )
            .with_position(span.start + 1),
        )));
    }

    let value_expr = infer_expr(&args[0], relation, params, sq, uf)?;
    let result_type = value_expr.data_type.clone();
    let mut typed_args = vec![value_expr.clone()];

    if let Some(offset_expr) = args.get(1) {
        let typed_offset = infer_expr_with_expected(
            offset_expr,
            relation,
            &DataType::BigInt,
            false,
            params,
            sq,
            uf,
        )?;
        validate_assignment_expr(&typed_offset, &DataType::BigInt, false, false, func_name)?;
        typed_args.push(typed_offset);
    }

    let nullable = if let Some(default_expr) = args.get(2) {
        let typed_default =
            infer_expr_with_expected(default_expr, relation, &result_type, true, params, sq, uf)?;
        validate_assignment_expr(&typed_default, &result_type, true, false, func_name)?;
        let nullable = value_expr.nullable || typed_default.nullable;
        typed_args.push(typed_default);
        nullable
    } else {
        true
    };

    Ok((kind, typed_args, result_type, nullable))
}

fn expect_no_args(func_name: &str, args: &[Expr], span: aiondb_parser::Span) -> DbResult<()> {
    if !args.is_empty() {
        return Err(DbError::Bind(Box::new(
            ErrorReport::new(
                SqlState::SyntaxError,
                format!("{func_name}() takes no arguments"),
            )
            .with_position(span.start + 1),
        )));
    }
    Ok(())
}

fn expect_one_arg(func_name: &str, args: &[Expr], span: aiondb_parser::Span) -> DbResult<()> {
    if args.len() != 1 {
        return Err(DbError::Bind(Box::new(
            ErrorReport::new(
                SqlState::SyntaxError,
                format!("{func_name}() requires exactly one argument"),
            )
            .with_position(span.start + 1),
        )));
    }
    Ok(())
}
