use super::*;

#[derive(Clone)]
struct PairStatsExprs {
    count: TypedExpr,
    count_as_double: TypedExpr,
    avg_x: TypedExpr,
    avg_y: TypedExpr,
    sxx: TypedExpr,
    syy: TypedExpr,
    sxy: TypedExpr,
}

fn json_object_key_for_row_field(arg: &TypedExpr, ordinal: usize) -> String {
    if let TypedExprKind::ColumnRef { name, .. } = &arg.kind {
        return name
            .rsplit('\0')
            .next()
            .map_or_else(|| name.clone(), ToOwned::to_owned);
    }
    format!("f{}", ordinal + 1)
}

fn rewrite_json_agg_whole_row_arg(expr: TypedExpr) -> TypedExpr {
    let TypedExpr {
        kind,
        data_type,
        nullable,
    } = expr;

    match kind {
        TypedExprKind::ScalarFunction {
            func: aiondb_plan::ScalarFunction::Row,
            args,
        } => {
            let mut object_args = Vec::with_capacity(args.len().saturating_mul(2));
            for (ordinal, arg) in args.into_iter().enumerate() {
                let key = json_object_key_for_row_field(&arg, ordinal);
                object_args.push(TypedExpr::literal(Value::Text(key), DataType::Text, false));
                object_args.push(rewrite_json_agg_whole_row_arg(arg));
            }
            TypedExpr::scalar_function(
                aiondb_plan::ScalarFunction::JsonbBuildObject,
                object_args,
                DataType::Jsonb,
                nullable,
            )
        }
        TypedExprKind::ScalarFunction { func, args } => TypedExpr {
            kind: TypedExprKind::ScalarFunction {
                func,
                args: args
                    .into_iter()
                    .map(rewrite_json_agg_whole_row_arg)
                    .collect(),
            },
            data_type,
            nullable,
        },
        TypedExprKind::ArrayConstruct { elements } => TypedExpr {
            kind: TypedExprKind::ArrayConstruct {
                elements: elements
                    .into_iter()
                    .map(rewrite_json_agg_whole_row_arg)
                    .collect(),
            },
            data_type,
            nullable,
        },
        TypedExprKind::Cast { expr, target_type } => TypedExpr {
            kind: TypedExprKind::Cast {
                expr: Box::new(rewrite_json_agg_whole_row_arg(*expr)),
                target_type,
            },
            data_type,
            nullable,
        },
        _ => TypedExpr {
            kind,
            data_type,
            nullable,
        },
    }
}

pub(super) fn try_resolve_aggregate(
    name: &str,
    args: &[Expr],
    distinct: bool,
    filter: Option<&Expr>,
    relation: Option<&TableDescriptor>,
    params: &mut ParameterTypes,
    span: aiondb_parser::Span,
    sq: Option<SubqueryResolver<'_>>,
    uf: Option<UserFunctionResolver<'_>>,
) -> DbResult<Option<TypedExpr>> {
    fn require_n(name: &str, args: &[Expr], n: usize, pos: usize) -> DbResult<()> {
        if args.len() != n {
            return Err(DbError::Bind(Box::new(
                ErrorReport::new(
                    SqlState::SyntaxError,
                    format!("{name} requires exactly {n} argument(s)"),
                )
                .with_position(pos),
            )));
        }
        Ok(())
    }
    let pos = span.start + 1;

    let typed_filter = if let Some(f) = filter {
        let typed = infer_expr(f, relation, params, sq, uf)?;
        // PostgreSQL surfaces this with a FILTER-specific message, not the
        // generic "aggregate function calls cannot be nested" used for
        // aggregate arguments. Mirror the wording so SQLSTATE+text both
        // match in pg_regress.
        ensure_no_nested_aggregate(
            &typed,
            "aggregate functions are not allowed in FILTER",
            f.span().start + 1,
        )?;
        Some(typed)
    } else {
        None
    };

    if name.eq_ignore_ascii_case("count") {
        require_n("COUNT", args, 1, pos)?;
        let is_star =
            matches!(&args[0], Expr::Identifier(n) if n.parts.len() == 1 && n.parts[0] == "*");
        if is_star {
            return Ok(Some(TypedExpr::agg_count_ext(None, distinct, typed_filter)));
        }
        let inner = infer_aggregate_arg(&args[0], relation, params, sq, uf)?;
        return Ok(Some(TypedExpr::agg_count_ext(
            Some(inner),
            distinct,
            typed_filter,
        )));
    }
    if name.eq_ignore_ascii_case("newavg") {
        require_n("NEWAVG", args, 1, pos)?;
        let inner = infer_aggregate_arg(&args[0], relation, params, sq, uf)?;
        return Ok(Some(TypedExpr::agg_avg_ext(inner, distinct, typed_filter)));
    }
    if name.eq_ignore_ascii_case("newsum") {
        require_n("NEWSUM", args, 1, pos)?;
        let inner = infer_aggregate_arg(&args[0], relation, params, sq, uf)?;
        return Ok(Some(TypedExpr::agg_sum_ext(inner, distinct, typed_filter)));
    }
    if name.eq_ignore_ascii_case("newcnt") || name.eq_ignore_ascii_case("oldcnt") {
        require_n("NEWCNT", args, 1, pos)?;
        let is_star =
            matches!(&args[0], Expr::Identifier(n) if n.parts.len() == 1 && n.parts[0] == "*");
        if is_star {
            return Ok(Some(TypedExpr::agg_count_ext(None, distinct, typed_filter)));
        }
        let inner = infer_aggregate_arg(&args[0], relation, params, sq, uf)?;
        return Ok(Some(TypedExpr::agg_count_ext(
            Some(inner),
            distinct,
            typed_filter,
        )));
    }
    if name.eq_ignore_ascii_case("sum2") || name.eq_ignore_ascii_case("mysum2") {
        require_n("SUM2", args, 2, pos)?;
        let left = infer_aggregate_arg(&args[0], relation, params, sq, uf)?;
        let right = infer_aggregate_arg(&args[1], relation, params, sq, uf)?;
        let data_type = resolve_arithmetic_type(&left.data_type, &right.data_type)?;
        let nullable = left.nullable || right.nullable;
        let combined = TypedExpr::arith_add(left, right, data_type, nullable);
        return Ok(Some(TypedExpr::agg_sum_ext(
            combined,
            distinct,
            typed_filter,
        )));
    }
    if name.eq_ignore_ascii_case("sum") {
        require_n("SUM", args, 1, pos)?;
        let inner = infer_aggregate_arg(&args[0], relation, params, sq, uf)?;
        return Ok(Some(TypedExpr::agg_sum_ext(inner, distinct, typed_filter)));
    }
    if name.eq_ignore_ascii_case("avg") {
        require_n("AVG", args, 1, pos)?;
        let inner = infer_aggregate_arg(&args[0], relation, params, sq, uf)?;
        return Ok(Some(TypedExpr::agg_avg_ext(inner, distinct, typed_filter)));
    }
    if name.eq_ignore_ascii_case("min") {
        require_n("MIN", args, 1, pos)?;
        let inner = infer_aggregate_arg(&args[0], relation, params, sq, uf)?;
        return Ok(Some(TypedExpr::agg_min_ext(inner, typed_filter)));
    }
    if name.eq_ignore_ascii_case("any_value") {
        require_n("ANY_VALUE", args, 1, pos)?;
        let inner = infer_aggregate_arg(&args[0], relation, params, sq, uf)?;
        return Ok(Some(TypedExpr::agg_any_value_ext(inner, typed_filter)));
    }
    if name.eq_ignore_ascii_case("max") {
        require_n("MAX", args, 1, pos)?;
        if aggregate_arg_is_pseudo_anyarray(&args[0]) {
            return Err(DbError::Bind(Box::new(
                ErrorReport::new(
                    SqlState::DatatypeMismatch,
                    "cannot compare arrays of different element types",
                )
                .with_position(args[0].span().start + 1),
            )));
        }
        let inner = infer_aggregate_arg(&args[0], relation, params, sq, uf)?;
        return Ok(Some(TypedExpr::agg_max_ext(inner, typed_filter)));
    }
    if name.eq_ignore_ascii_case("string_agg") {
        require_n("STRING_AGG", args, 2, pos)?;
        let expr = infer_aggregate_arg(&args[0], relation, params, sq, uf)?;
        let delim = infer_aggregate_arg(&args[1], relation, params, sq, uf)?;
        return Ok(Some(TypedExpr::agg_string_agg_ext(
            expr,
            delim,
            distinct,
            typed_filter,
        )));
    }
    if name.eq_ignore_ascii_case("array_agg") {
        require_n("ARRAY_AGG", args, 1, pos)?;
        let inner = infer_aggregate_arg(&args[0], relation, params, sq, uf)?;
        return Ok(Some(TypedExpr::agg_array_agg_ext(
            inner,
            distinct,
            typed_filter,
            None,
        )));
    }
    if name.eq_ignore_ascii_case("__aiondb_array_agg_ordered_desc")
        || name.eq_ignore_ascii_case("__aiondb_array_agg_ordered_asc")
    {
        require_n("ARRAY_AGG", args, 1, pos)?;
        let inner = infer_aggregate_arg(&args[0], relation, params, sq, uf)?;
        let order_descending = Some(name.eq_ignore_ascii_case("__aiondb_array_agg_ordered_desc"));
        return Ok(Some(TypedExpr::agg_array_agg_ext(
            inner,
            distinct,
            typed_filter,
            order_descending,
        )));
    }
    if name.eq_ignore_ascii_case("json_agg") || name.eq_ignore_ascii_case("jsonb_agg") {
        require_n("JSONB_AGG", args, 1, pos)?;
        let inner = rewrite_json_agg_whole_row_arg(infer_aggregate_arg(
            &args[0], relation, params, sq, uf,
        )?);
        let array_agg = TypedExpr::agg_array_agg_ext(inner, distinct, typed_filter, None);
        return Ok(Some(TypedExpr::scalar_function(
            aiondb_plan::ScalarFunction::Generic("__aiondb_jsonb_agg_finalize".to_owned()),
            vec![array_agg],
            DataType::Jsonb,
            true,
        )));
    }
    if name.eq_ignore_ascii_case("__aiondb_jsonb_agg_ordered_desc")
        || name.eq_ignore_ascii_case("__aiondb_jsonb_agg_ordered_asc")
        || name.eq_ignore_ascii_case("__aiondb_json_agg_ordered_desc")
        || name.eq_ignore_ascii_case("__aiondb_json_agg_ordered_asc")
    {
        require_n("JSONB_AGG", args, 1, pos)?;
        let inner = rewrite_json_agg_whole_row_arg(infer_aggregate_arg(
            &args[0], relation, params, sq, uf,
        )?);
        let order_descending = Some(
            name.eq_ignore_ascii_case("__aiondb_jsonb_agg_ordered_desc")
                || name.eq_ignore_ascii_case("__aiondb_json_agg_ordered_desc"),
        );
        let array_agg =
            TypedExpr::agg_array_agg_ext(inner, distinct, typed_filter, order_descending);
        return Ok(Some(TypedExpr::scalar_function(
            aiondb_plan::ScalarFunction::Generic("__aiondb_jsonb_agg_ordered_finalize".to_owned()),
            vec![array_agg],
            DataType::Jsonb,
            true,
        )));
    }
    if name.eq_ignore_ascii_case("json_object_agg") || name.eq_ignore_ascii_case("jsonb_object_agg")
    {
        require_n("JSONB_OBJECT_AGG", args, 2, pos)?;
        let key = infer_aggregate_arg(&args[0], relation, params, sq, uf)?;
        let value = infer_aggregate_arg(&args[1], relation, params, sq, uf)?;
        let pair = TypedExpr::scalar_function(
            aiondb_plan::ScalarFunction::JsonbBuildArray,
            vec![key, value],
            DataType::Jsonb,
            true,
        );
        let pairs_agg = TypedExpr::agg_array_agg_ext(pair, distinct, typed_filter, None);
        return Ok(Some(TypedExpr::scalar_function(
            aiondb_plan::ScalarFunction::Generic("__aiondb_jsonb_object_agg_finalize".to_owned()),
            vec![pairs_agg],
            DataType::Jsonb,
            true,
        )));
    }
    if name.eq_ignore_ascii_case("bool_and") || name.eq_ignore_ascii_case("every") {
        require_n("BOOL_AND", args, 1, pos)?;
        let inner = infer_aggregate_arg(&args[0], relation, params, sq, uf)?;
        return Ok(Some(TypedExpr::agg_bool_and_ext(inner, typed_filter)));
    }
    if name.eq_ignore_ascii_case("stddev_pop") {
        require_n("STDDEV_POP", args, 1, pos)?;
        let inner = infer_aggregate_arg(&args[0], relation, params, sq, uf)?;
        return Ok(Some(TypedExpr::agg_stddev_pop_ext(inner, typed_filter)));
    }
    if name.eq_ignore_ascii_case("stddev_samp") || name.eq_ignore_ascii_case("stddev") {
        require_n("STDDEV_SAMP", args, 1, pos)?;
        let inner = infer_aggregate_arg(&args[0], relation, params, sq, uf)?;
        return Ok(Some(TypedExpr::agg_stddev_samp_ext(inner, typed_filter)));
    }
    if name.eq_ignore_ascii_case("var_pop") {
        require_n("VAR_POP", args, 1, pos)?;
        let inner = infer_aggregate_arg(&args[0], relation, params, sq, uf)?;
        return Ok(Some(TypedExpr::agg_var_pop_ext(inner, typed_filter)));
    }
    if name.eq_ignore_ascii_case("var_samp") || name.eq_ignore_ascii_case("variance") {
        require_n("VAR_SAMP", args, 1, pos)?;
        let inner = infer_aggregate_arg(&args[0], relation, params, sq, uf)?;
        return Ok(Some(TypedExpr::agg_var_samp_ext(inner, typed_filter)));
    }
    if name.eq_ignore_ascii_case("bool_or") {
        require_n("BOOL_OR", args, 1, pos)?;
        let inner = infer_aggregate_arg(&args[0], relation, params, sq, uf)?;
        return Ok(Some(TypedExpr::agg_bool_or_ext(inner, typed_filter)));
    }
    if matches!(
        ascii_lower(name).as_str(),
        "corr"
            | "covar_pop"
            | "covar_samp"
            | "regr_avgx"
            | "regr_avgy"
            | "regr_count"
            | "regr_intercept"
            | "regr_r2"
            | "regr_slope"
            | "regr_sxx"
            | "regr_sxy"
            | "regr_syy"
    ) {
        return Ok(Some(resolve_statistical_aggregate(
            name,
            args,
            distinct,
            typed_filter,
            relation,
            params,
            span,
            sq,
            uf,
        )?));
    }
    Ok(None)
}

pub(super) fn is_aggregate_function_name(name: &str) -> bool {
    matches!(
        ascii_lower(name).as_str(),
        "count"
            | "newavg"
            | "newsum"
            | "newcnt"
            | "oldcnt"
            | "sum2"
            | "mysum2"
            | "sum"
            | "avg"
            | "min"
            | "any_value"
            | "max"
            | "string_agg"
            | "array_agg"
            | "__aiondb_array_agg_ordered_desc"
            | "__aiondb_array_agg_ordered_asc"
            | "json_agg"
            | "jsonb_agg"
            | "__aiondb_json_agg_ordered_desc"
            | "__aiondb_json_agg_ordered_asc"
            | "__aiondb_jsonb_agg_ordered_desc"
            | "__aiondb_jsonb_agg_ordered_asc"
            | "json_object_agg"
            | "jsonb_object_agg"
            | "bool_and"
            | "every"
            | "stddev_pop"
            | "stddev_samp"
            | "stddev"
            | "var_pop"
            | "var_samp"
            | "variance"
            | "bool_or"
            | "corr"
            | "covar_pop"
            | "covar_samp"
            | "regr_avgx"
            | "regr_avgy"
            | "regr_count"
            | "regr_intercept"
            | "regr_r2"
            | "regr_slope"
            | "regr_sxx"
            | "regr_sxy"
            | "regr_syy"
    )
}

fn resolve_statistical_aggregate(
    name: &str,
    args: &[Expr],
    distinct: bool,
    typed_filter: Option<TypedExpr>,
    relation: Option<&TableDescriptor>,
    params: &mut ParameterTypes,
    span: aiondb_parser::Span,
    sq: Option<SubqueryResolver<'_>>,
    uf: Option<UserFunctionResolver<'_>>,
) -> DbResult<TypedExpr> {
    if distinct {
        return Err(DbError::feature_not_supported(format!(
            "DISTINCT is not supported for statistical aggregate {name}"
        )));
    }

    let stats = build_pair_stats(args, typed_filter, relation, params, span, sq, uf)?;
    let one_count = TypedExpr::binary_eq(stats.count.clone(), bigint_lit(1));
    let zero_double = double_lit(0.0);
    let one_double = double_lit(1.0);
    let null_double = null_lit(DataType::Double);
    let zero_x = TypedExpr::binary_eq(stats.sxx.clone(), zero_double.clone());
    let zero_y = TypedExpr::binary_eq(stats.syy.clone(), zero_double.clone());

    let expr = match ascii_lower(name).as_str() {
        "regr_count" => stats.count,
        "regr_avgx" => TypedExpr::case_when(
            vec![TypedExpr::binary_eq(stats.count.clone(), bigint_lit(0))],
            vec![null_double.clone()],
            Some(stats.avg_x),
            DataType::Double,
            true,
        ),
        "regr_avgy" => TypedExpr::case_when(
            vec![TypedExpr::binary_eq(stats.count.clone(), bigint_lit(0))],
            vec![null_double.clone()],
            Some(stats.avg_y),
            DataType::Double,
            true,
        ),
        "regr_sxx" => stats.sxx,
        "regr_syy" => stats.syy,
        "regr_sxy" => stats.sxy,
        "covar_pop" => TypedExpr::case_when(
            vec![TypedExpr::binary_eq(stats.count.clone(), bigint_lit(0))],
            vec![null_double.clone()],
            Some(div_double(stats.sxy, stats.count_as_double)),
            DataType::Double,
            true,
        ),
        "covar_samp" => TypedExpr::case_when(
            vec![TypedExpr::logical_or(
                TypedExpr::binary_eq(stats.count.clone(), bigint_lit(0)),
                one_count,
            )],
            vec![null_double.clone()],
            Some(div_double(
                stats.sxy,
                sub_double(stats.count_as_double, one_double.clone()),
            )),
            DataType::Double,
            true,
        ),
        "corr" => TypedExpr::case_when(
            vec![TypedExpr::logical_or(
                TypedExpr::logical_or(
                    TypedExpr::binary_eq(stats.count.clone(), bigint_lit(0)),
                    zero_x.clone(),
                ),
                zero_y.clone(),
            )],
            vec![null_double.clone()],
            Some(div_double(
                stats.sxy,
                mul_double(sqrt_double(stats.sxx), sqrt_double(stats.syy)),
            )),
            DataType::Double,
            true,
        ),
        "regr_slope" => TypedExpr::case_when(
            vec![TypedExpr::logical_or(
                TypedExpr::binary_eq(stats.count.clone(), bigint_lit(0)),
                zero_x.clone(),
            )],
            vec![null_double.clone()],
            Some(div_double(stats.sxy, stats.sxx)),
            DataType::Double,
            true,
        ),
        "regr_intercept" => TypedExpr::case_when(
            vec![TypedExpr::logical_or(
                TypedExpr::binary_eq(stats.count.clone(), bigint_lit(0)),
                zero_x.clone(),
            )],
            vec![null_double.clone()],
            Some(sub_double(
                stats.avg_y,
                mul_double(div_double(stats.sxy, stats.sxx), stats.avg_x),
            )),
            DataType::Double,
            true,
        ),
        "regr_r2" => TypedExpr::case_when(
            vec![
                TypedExpr::binary_eq(stats.count.clone(), bigint_lit(0)),
                zero_x,
                zero_y,
            ],
            vec![null_double.clone(), null_double.clone(), one_double.clone()],
            Some(div_double(
                mul_double(stats.sxy.clone(), stats.sxy),
                mul_double(stats.sxx, stats.syy),
            )),
            DataType::Double,
            true,
        ),
        other => {
            return Err(DbError::internal(format!(
                "unexpected statistical aggregate resolver for {other}"
            )));
        }
    };

    Ok(expr)
}

fn aggregate_arg_is_pseudo_anyarray(expr: &Expr) -> bool {
    let Expr::Identifier(identifier) = expr else {
        return false;
    };
    identifier
        .parts
        .last()
        .is_some_and(|part| part.eq_ignore_ascii_case("histogram_bounds"))
}

fn build_pair_stats(
    args: &[Expr],
    typed_filter: Option<TypedExpr>,
    relation: Option<&TableDescriptor>,
    params: &mut ParameterTypes,
    span: aiondb_parser::Span,
    sq: Option<SubqueryResolver<'_>>,
    uf: Option<UserFunctionResolver<'_>>,
) -> DbResult<PairStatsExprs> {
    if args.len() != 2 {
        return Err(DbError::Bind(Box::new(
            ErrorReport::new(
                SqlState::SyntaxError,
                "statistical aggregates require exactly 2 argument(s)",
            )
            .with_position(span.start + 1),
        )));
    }

    let y = cast_double(infer_expr(&args[0], relation, params, sq, uf)?);
    let x = cast_double(infer_expr(&args[1], relation, params, sq, uf)?);
    ensure_no_nested_aggregate(
        &y,
        "aggregate function calls cannot be nested",
        args[0].span().start + 1,
    )?;
    ensure_no_nested_aggregate(
        &x,
        "aggregate function calls cannot be nested",
        args[1].span().start + 1,
    )?;

    let pair_filter = combine_filters(
        typed_filter,
        TypedExpr::logical_and(
            TypedExpr::is_null(y.clone(), true),
            TypedExpr::is_null(x.clone(), true),
        ),
    );

    let count = TypedExpr::agg_count_ext(None, false, pair_filter.clone());
    let count_as_double = cast_double(count.clone());
    let sum_x = TypedExpr::agg_sum_ext(x.clone(), false, pair_filter.clone());
    let sum_y = TypedExpr::agg_sum_ext(y.clone(), false, pair_filter.clone());
    let avg_x = TypedExpr::agg_avg_ext(x.clone(), false, pair_filter.clone());
    let avg_y = TypedExpr::agg_avg_ext(y.clone(), false, pair_filter.clone());
    let sum_xx =
        TypedExpr::agg_sum_ext(mul_double(x.clone(), x.clone()), false, pair_filter.clone());
    let sum_yy =
        TypedExpr::agg_sum_ext(mul_double(y.clone(), y.clone()), false, pair_filter.clone());
    let sum_xy = TypedExpr::agg_sum_ext(mul_double(x.clone(), y.clone()), false, pair_filter);

    let sxx_core = sub_double(
        sum_xx,
        div_double(
            mul_double(sum_x.clone(), sum_x.clone()),
            count_as_double.clone(),
        ),
    );
    let syy_core = sub_double(
        sum_yy,
        div_double(
            mul_double(sum_y.clone(), sum_y.clone()),
            count_as_double.clone(),
        ),
    );
    let sxy_core = sub_double(
        sum_xy,
        div_double(
            mul_double(sum_x.clone(), sum_y.clone()),
            count_as_double.clone(),
        ),
    );
    let count_is_zero = TypedExpr::binary_eq(count.clone(), bigint_lit(0));
    let null_double = null_lit(DataType::Double);

    Ok(PairStatsExprs {
        count: count.clone(),
        count_as_double,
        avg_x,
        avg_y,
        sxx: TypedExpr::case_when(
            vec![count_is_zero.clone()],
            vec![null_double.clone()],
            Some(sxx_core),
            DataType::Double,
            true,
        ),
        syy: TypedExpr::case_when(
            vec![count_is_zero.clone()],
            vec![null_double.clone()],
            Some(syy_core),
            DataType::Double,
            true,
        ),
        sxy: TypedExpr::case_when(
            vec![count_is_zero],
            vec![null_double],
            Some(sxy_core),
            DataType::Double,
            true,
        ),
    })
}

fn combine_filters(left: Option<TypedExpr>, right: TypedExpr) -> Option<TypedExpr> {
    Some(match left {
        Some(existing) => TypedExpr::logical_and(existing, right),
        None => right,
    })
}

fn ascii_lower(name: &str) -> String {
    name.to_ascii_lowercase()
}

fn null_lit(data_type: DataType) -> TypedExpr {
    TypedExpr::literal(Value::Null, data_type, true)
}

fn bigint_lit(value: i64) -> TypedExpr {
    TypedExpr::literal(Value::BigInt(value), DataType::BigInt, false)
}

fn double_lit(value: f64) -> TypedExpr {
    TypedExpr::literal(Value::Double(value), DataType::Double, false)
}

fn cast_double(expr: TypedExpr) -> TypedExpr {
    if expr.data_type == DataType::Double {
        expr
    } else {
        TypedExpr::cast(expr, DataType::Double)
    }
}

fn sub_double(left: TypedExpr, right: TypedExpr) -> TypedExpr {
    TypedExpr::arith_sub(left, right, DataType::Double, true)
}

fn mul_double(left: TypedExpr, right: TypedExpr) -> TypedExpr {
    TypedExpr::arith_mul(left, right, DataType::Double, true)
}

fn div_double(left: TypedExpr, right: TypedExpr) -> TypedExpr {
    TypedExpr::arith_div(left, right, DataType::Double, true)
}

fn sqrt_double(expr: TypedExpr) -> TypedExpr {
    // Use the typed `Sqrt` variant rather than `Generic("sqrt")`. The optimizer
    // rejects `Generic("sqrt")` (it has no costing entry), which previously
    // caused `corr()` to fail with SQLSTATE 0A000 even though every other
    // statistical aggregate (`covar_samp`, `regr_slope`, ...) succeeded.
    TypedExpr::scalar_function(
        aiondb_plan::ScalarFunction::Sqrt,
        vec![expr],
        DataType::Double,
        true,
    )
}

fn infer_aggregate_arg(
    expr: &Expr,
    relation: Option<&TableDescriptor>,
    params: &mut ParameterTypes,
    sq: Option<SubqueryResolver<'_>>,
    uf: Option<UserFunctionResolver<'_>>,
) -> DbResult<TypedExpr> {
    let typed = infer_expr(expr, relation, params, sq, uf)?;
    ensure_no_nested_aggregate(
        &typed,
        "aggregate function calls cannot be nested",
        expr.span().start + 1,
    )?;
    Ok(typed)
}

fn ensure_no_nested_aggregate(expr: &TypedExpr, message: &str, position: usize) -> DbResult<()> {
    if contains_aggregate(expr) {
        return Err(DbError::Bind(Box::new(
            ErrorReport::new(SqlState::SyntaxError, message).with_position(position),
        )));
    }
    Ok(())
}

fn contains_aggregate(expr: &TypedExpr) -> bool {
    let mut stack = vec![expr];
    while let Some(expr) = stack.pop() {
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
            | TypedExprKind::AggVarSamp { .. } => return true,
            TypedExprKind::BinaryEq { left, right }
            | TypedExprKind::BinaryNe { left, right }
            | TypedExprKind::BinaryGt { left, right }
            | TypedExprKind::BinaryGe { left, right }
            | TypedExprKind::BinaryLt { left, right }
            | TypedExprKind::BinaryLe { left, right }
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
                stack.push(right);
                stack.push(left);
            }
            TypedExprKind::LogicalNot { expr }
            | TypedExprKind::Negate { expr }
            | TypedExprKind::IsNull { expr, .. }
            | TypedExprKind::Cast { expr, .. }
            | TypedExprKind::InSubquery { expr, .. } => stack.push(expr),
            TypedExprKind::Like { expr, pattern, .. } => {
                stack.push(pattern);
                stack.push(expr);
            }
            TypedExprKind::InList { expr, list, .. } => {
                stack.extend(list);
                stack.push(expr);
            }
            TypedExprKind::Between {
                expr, low, high, ..
            } => {
                stack.push(high);
                stack.push(low);
                stack.push(expr);
            }
            TypedExprKind::CaseWhen {
                conditions,
                results,
                else_result,
            } => {
                if let Some(expr) = else_result {
                    stack.push(expr);
                }
                stack.extend(results);
                stack.extend(conditions);
            }
            TypedExprKind::Coalesce { args }
            | TypedExprKind::ScalarFunction { args, .. }
            | TypedExprKind::ArrayConstruct { elements: args }
            | TypedExprKind::UserFunction { args, .. } => stack.extend(args),
            TypedExprKind::WindowFunction {
                args,
                partition_by,
                order_by,
                ..
            } => {
                for sort in order_by {
                    stack.push(&sort.expr);
                }
                stack.extend(partition_by);
                stack.extend(args);
            }
            TypedExprKind::Literal(_)
            | TypedExprKind::ColumnRef { .. }
            | TypedExprKind::OuterColumnRef { .. }
            | TypedExprKind::NextValue { .. }
            | TypedExprKind::ArraySubquery { .. }
            | TypedExprKind::ScalarSubquery { .. }
            | TypedExprKind::ExistsSubquery { .. } => {}
        }
    }
    false
}
