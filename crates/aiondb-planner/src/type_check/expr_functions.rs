#![allow(
    clippy::uninlined_format_args,
    clippy::doc_markdown,
    clippy::doc_lazy_continuation
)]

use super::aggregates::try_resolve_aggregate;
use super::support::is_numeric_without_money;
use super::*;

use aiondb_core::TextTypeModifier;
use aiondb_eval::{
    compat_display_type_name, normalize_compat_type_name, with_current_session_context,
};
use aiondb_plan::ScalarFunction;

use super::expr::infer_expr;
use super::expr_fn_helpers::*;

fn infer_geometric_source_type_from_relation(
    source_expr: &Expr,
    relation: Option<&TableDescriptor>,
) -> Option<String> {
    let relation = relation?;
    let identifier = match unwrap_type_hint_expr(source_expr) {
        Expr::Identifier(name) => name,
        _ => return None,
    };
    let column_name = identifier.parts.last()?;
    let bare_column = column_name
        .rsplit('\0')
        .next()
        .unwrap_or(column_name.as_str());
    let column = relation.columns.iter().find(|column| {
        column.name.eq_ignore_ascii_case(column_name)
            || column
                .name
                .rsplit('\0')
                .next()
                .is_some_and(|name| name.eq_ignore_ascii_case(bare_column))
    })?;

    let bare_column_name = column
        .name
        .rsplit('\0')
        .next()
        .unwrap_or(column.name.as_str());
    let quoted = aiondb_parser::identifier::quote_identifier(bare_column_name);
    let compat_prefix = format!("__aiondb_compat_cast({quoted}, 'text', '");
    for constraint in &relation.check_constraints {
        if let Some(start) = constraint.expression.find(&compat_prefix) {
            let geom_start = start + compat_prefix.len();
            if let Some(end_rel) = constraint.expression[geom_start..].find("')") {
                let geom = &constraint.expression[geom_start..geom_start + end_rel];
                if matches!(
                    geom,
                    "point" | "box" | "line" | "lseg" | "path" | "polygon" | "circle"
                ) {
                    return Some(geom.to_owned());
                }
            }
        }
    }
    None
}

fn json_record_mode_for_data_type(data_type: &DataType) -> &'static str {
    match data_type {
        DataType::Jsonb => "json",
        DataType::Array(_) => "array",
        _ => "scalar",
    }
}

fn json_record_metadata_from_type_hint(expr: &Expr) -> Option<(Vec<String>, Vec<String>)> {
    let hinted = type_hint_name(expr)?;
    let normalized = normalize_compat_type_name(hinted);
    with_current_session_context(|ctx| {
        let mut lookup = normalized;
        while let Some(domain) = ctx.domain_def(&lookup) {
            lookup = normalize_compat_type_name(&domain.base_type);
        }
        let user_type = ctx.compat_user_type(&lookup)?;
        if user_type.composite_fields.is_empty() {
            return None;
        }
        let keys = user_type
            .composite_fields
            .iter()
            .map(|field| field.name.clone())
            .collect::<Vec<_>>();
        let modes = user_type
            .composite_fields
            .iter()
            .map(|field| json_record_mode_for_data_type(&field.data_type).to_owned())
            .collect::<Vec<_>>();
        Some((keys, modes))
    })
}

fn json_record_metadata_from_row_expr(
    typed_expr: &TypedExpr,
) -> Option<(Vec<String>, Vec<String>)> {
    let TypedExprKind::ScalarFunction {
        func: ScalarFunction::Row,
        args,
    } = &typed_expr.kind
    else {
        return None;
    };
    let keys = (1..=args.len())
        .map(|index| format!("f{index}"))
        .collect::<Vec<_>>();
    let modes = vec!["scalar".to_owned(); args.len()];
    Some((keys, modes))
}

fn typed_text_array_literal(items: Vec<String>) -> TypedExpr {
    TypedExpr::literal(
        Value::Array(items.into_iter().map(Value::Text).collect()),
        DataType::Array(Box::new(DataType::Text)),
        false,
    )
}

pub(super) fn infer_function_call(
    name: &aiondb_parser::ObjectName,
    args: &[Expr],
    agg_distinct: bool,
    agg_filter: Option<&Expr>,
    relation: Option<&TableDescriptor>,
    params: &mut ParameterTypes,
    span: aiondb_parser::Span,
    sq: Option<SubqueryResolver<'_>>,
    uf: Option<UserFunctionResolver<'_>>,
) -> DbResult<TypedExpr> {
    let function_name = name.parts.last().map_or("", String::as_str);
    let user_function_name = if name.parts.is_empty() {
        function_name.to_owned()
    } else {
        name.parts.join(".")
    };
    if function_name.eq_ignore_ascii_case("pg_collation_for") {
        if args.len() != 1 {
            return Err(DbError::Bind(Box::new(
                ErrorReport::new(
                    SqlState::SyntaxError,
                    format!(
                        "pg_collation_for() expects exactly one argument, got {}",
                        args.len()
                    ),
                )
                .with_position(span.start + 1),
            )));
        }
        // Ordered-set aggregates embedded in pg_collation_for(...) are
        // normalized by compat SQL rewrites at top-level SELECT only. Keep
        // this call bindable by short-circuiting collation inference here.
        if let Expr::FunctionCall {
            name: inner_name, ..
        } = &args[0]
        {
            if inner_name.parts.last().is_some_and(|part| {
                matches!(
                    part.to_ascii_lowercase().as_str(),
                    "percentile_disc"
                        | "test_percentile_disc"
                        | "percentile_cont"
                        | "mode"
                        | "rank"
                        | "test_rank"
                        | "dense_rank"
                        | "percent_rank"
                        | "cume_dist"
                )
            }) {
                return Ok(TypedExpr::agg_any_value_ext(
                    TypedExpr::literal(Value::Text("default".to_owned()), DataType::Text, false),
                    None,
                ));
            }
        }
        let _ = infer_expr(&args[0], relation, params, sq, uf)?;
        return Ok(TypedExpr::literal(
            Value::Text("default".to_owned()),
            DataType::Text,
            false,
        ));
    }
    if function_name.eq_ignore_ascii_case("__aiondb_type_hint") {
        if let (Some(expr), Some(Expr::Literal(Literal::String(type_name), _))) =
            (args.first(), args.get(1))
        {
            let normalized_type_name = normalize_compat_type_name(type_name);
            if type_name.eq_ignore_ascii_case("regtype") {
                let source_expr = match unwrap_type_hint_expr(expr) {
                    Expr::Cast { expr: source, .. } => source.as_ref(),
                    other => other,
                };
                let typed_source = super::expr_helpers::infer_expr_with_expected(
                    source_expr,
                    relation,
                    &DataType::Text,
                    true,
                    params,
                    sq,
                    uf,
                )?;
                let nullable = typed_source.nullable;
                return Ok(TypedExpr::scalar_function(
                    ScalarFunction::Generic("__aiondb_regtype_cast".to_owned()),
                    vec![typed_source],
                    DataType::Int,
                    nullable,
                ));
            }
            if type_name.eq_ignore_ascii_case("regrole") {
                if let Some(source_expr) = regclass_lookup_source(expr) {
                    let typed_source = super::expr_helpers::infer_expr_with_expected(
                        source_expr,
                        relation,
                        &DataType::Text,
                        true,
                        params,
                        sq,
                        uf,
                    )?;
                    let nullable = typed_source.nullable;
                    return Ok(TypedExpr::scalar_function(
                        ScalarFunction::Generic("__aiondb_regrole_cast".to_owned()),
                        vec![typed_source],
                        DataType::Int,
                        nullable,
                    ));
                }
                if let Expr::Cast { expr: source, .. } = unwrap_type_hint_expr(expr) {
                    let typed_source = infer_expr(source, relation, params, sq, uf)?;
                    if matches!(typed_source.data_type, DataType::Int | DataType::BigInt) {
                        let nullable = typed_source.nullable;
                        return Ok(TypedExpr::scalar_function(
                            ScalarFunction::Generic("__aiondb_regrole_out".to_owned()),
                            vec![typed_source],
                            DataType::Text,
                            nullable,
                        ));
                    }
                }
            }
            if type_name.eq_ignore_ascii_case("regproc")
                || type_name.eq_ignore_ascii_case("regprocedure")
            {
                let cast_function = if type_name.eq_ignore_ascii_case("regproc") {
                    "__aiondb_regproc_cast"
                } else {
                    "__aiondb_regprocedure_cast"
                };
                let out_function = if type_name.eq_ignore_ascii_case("regproc") {
                    "__aiondb_regproc_out"
                } else {
                    "__aiondb_regprocedure_out"
                };
                if let Some(source_expr) = regclass_lookup_source(expr) {
                    let typed_source = super::expr_helpers::infer_expr_with_expected(
                        source_expr,
                        relation,
                        &DataType::Text,
                        true,
                        params,
                        sq,
                        uf,
                    )?;
                    let nullable = typed_source.nullable;
                    return Ok(TypedExpr::scalar_function(
                        ScalarFunction::Generic(cast_function.to_owned()),
                        vec![typed_source],
                        DataType::Int,
                        nullable,
                    ));
                }
                if let Expr::Cast { expr: source, .. } = unwrap_type_hint_expr(expr) {
                    let typed_source = infer_expr(source, relation, params, sq, uf)?;
                    if matches!(typed_source.data_type, DataType::Int | DataType::BigInt) {
                        let nullable = typed_source.nullable;
                        return Ok(TypedExpr::scalar_function(
                            ScalarFunction::Generic(out_function.to_owned()),
                            vec![typed_source],
                            DataType::Text,
                            nullable,
                        ));
                    }
                }
            }
            if type_name.eq_ignore_ascii_case("regclass") {
                let source_expr = match unwrap_type_hint_expr(expr) {
                    Expr::Cast { expr: source, .. } => source.as_ref(),
                    other => other,
                };
                if !matches!(source_expr, Expr::Parameter { .. }) {
                    let typed_source = infer_expr(source_expr, relation, params, sq, uf)?;
                    if matches!(typed_source.data_type, DataType::Int | DataType::BigInt) {
                        let nullable = typed_source.nullable;
                        return Ok(TypedExpr::scalar_function(
                            ScalarFunction::Generic("__aiondb_regclass_out".to_owned()),
                            vec![typed_source],
                            DataType::Text,
                            nullable,
                        ));
                    }
                }
                let typed_source = super::expr_helpers::infer_expr_with_expected(
                    source_expr,
                    relation,
                    &DataType::Text,
                    true,
                    params,
                    sq,
                    uf,
                )?;
                let nullable = typed_source.nullable;
                return Ok(TypedExpr::scalar_function(
                    ScalarFunction::Generic("__aiondb_regclass_cast".to_owned()),
                    vec![typed_source],
                    DataType::Int,
                    nullable,
                ));
            }
            if type_name.eq_ignore_ascii_case("xid") {
                let source_expr = match unwrap_type_hint_expr(expr) {
                    Expr::Cast { expr: source, .. } => source.as_ref(),
                    other => other,
                };
                let typed_source = super::expr_helpers::infer_expr_with_expected(
                    source_expr,
                    relation,
                    &DataType::Text,
                    true,
                    params,
                    sq,
                    uf,
                )?;
                let nullable = typed_source.nullable;
                return Ok(TypedExpr::scalar_function(
                    ScalarFunction::Generic("__aiondb_xid_cast".to_owned()),
                    vec![typed_source],
                    DataType::BigInt,
                    nullable,
                ));
            }
            if type_name.eq_ignore_ascii_case("xid8") {
                let source_expr = match unwrap_type_hint_expr(expr) {
                    Expr::Cast { expr: source, .. } => source.as_ref(),
                    other => other,
                };
                let typed_source = super::expr_helpers::infer_expr_with_expected(
                    source_expr,
                    relation,
                    &DataType::Text,
                    true,
                    params,
                    sq,
                    uf,
                )?;
                let nullable = typed_source.nullable;
                return Ok(TypedExpr::scalar_function(
                    ScalarFunction::Generic("__aiondb_xid8_cast".to_owned()),
                    vec![typed_source],
                    DataType::Numeric,
                    nullable,
                ));
            }
            if type_name.eq_ignore_ascii_case("pg_snapshot")
                || type_name.eq_ignore_ascii_case("txid_snapshot")
            {
                let source_expr = match unwrap_type_hint_expr(expr) {
                    Expr::Cast { expr: source, .. } => source.as_ref(),
                    other => other,
                };
                let typed_source = super::expr_helpers::infer_expr_with_expected(
                    source_expr,
                    relation,
                    &DataType::Text,
                    true,
                    params,
                    sq,
                    uf,
                )?;
                let nullable = typed_source.nullable;
                return Ok(TypedExpr::scalar_function(
                    ScalarFunction::Generic("__aiondb_pg_snapshot_cast".to_owned()),
                    vec![typed_source],
                    DataType::Text,
                    nullable,
                ));
            }
            if type_name.eq_ignore_ascii_case("jsonpath") {
                let typed_source = infer_expr(expr, relation, params, sq, uf)?;
                let nullable = typed_source.nullable;
                let typed_expr = TypedExpr::scalar_function(
                    ScalarFunction::Generic("__aiondb_jsonpath_cast".to_owned()),
                    vec![typed_source],
                    DataType::Text,
                    nullable,
                );
                if super::expr_cases::is_const_foldable_expr(&typed_expr) {
                    let value = aiondb_eval::ExpressionEvaluator.evaluate(&typed_expr)?;
                    let is_null = value.is_null();
                    return Ok(TypedExpr::literal(value, DataType::Text, is_null));
                }
                return Ok(typed_expr);
            }
            let target_is_compat_user_type = is_compat_user_type_name(&normalized_type_name);
            let target_is_multirange = normalized_type_name.ends_with("multirange");
            let target_is_geometric_builtin = matches!(
                normalized_type_name.as_str(),
                "point" | "box" | "line" | "lseg" | "path" | "polygon" | "circle"
            );
            if target_is_compat_user_type || target_is_multirange || target_is_geometric_builtin {
                if let Expr::Cast {
                    expr: source_expr, ..
                } = unwrap_type_hint_expr(expr)
                {
                    let type_position = source_expr.span().end.saturating_add(1);
                    let typed_source = infer_expr(source_expr, relation, params, sq, uf)?;
                    let mut source_type_name = expr_type_name(source_expr, &typed_source);
                    if target_is_geometric_builtin
                        && normalize_compat_type_name(&source_type_name) == "text"
                    {
                        if let Some(inferred) =
                            infer_geometric_source_type_from_relation(source_expr, relation)
                        {
                            source_type_name = inferred;
                        }
                    }
                    let normalized_source_type = normalize_compat_type_name(&source_type_name);
                    let cast = find_compat_cast(&source_type_name, &normalized_type_name, false);
                    // When the source is a text literal (e.g. '42'::int8alias1),
                    // PostgreSQL uses the target type's input function to parse
                    // the string.  AionDB stores user-defined types as text, so
                    // we can treat text-to-user-type as a binary pass-through
                    // when no explicit cast is registered.
                    if target_is_compat_user_type
                        && cast.is_none()
                        && normalized_source_type != normalized_type_name
                        && normalized_source_type != "text"
                        && !is_domain_cast_compatible(
                            &normalized_source_type,
                            &normalized_type_name,
                        )
                    {
                        return Err(DbError::Bind(Box::new(
                            ErrorReport::new(
                                SqlState::DatatypeMismatch,
                                format!(
                                    "cannot cast type {} to {}",
                                    compat_display_type_name(&source_type_name),
                                    normalized_type_name
                                ),
                            )
                            .with_position(type_position),
                        )));
                    }
                    return match cast.as_ref().map(|c| &c.method) {
                        Some(aiondb_eval::CompatCastMethod::Function {
                            function_name: ref cast_function,
                            ..
                        }) => {
                            let Some(uf_resolver) = uf else {
                                return Err(DbError::internal(
                                    "user function resolver unavailable for compat cast",
                                ));
                            };
                            let Some(func_desc) =
                                find_unary_user_function_overload(uf_resolver, cast_function)?
                            else {
                                return Err(DbError::Bind(Box::new(
                                    ErrorReport::new(
                                        SqlState::UndefinedObject,
                                        format!(
                                            "function {cast_function}({}) does not exist",
                                            compat_display_type_name(&source_type_name)
                                        ),
                                    )
                                    .with_position(span.start + 1),
                                )));
                            };
                            let param_pairs: Vec<(String, DataType)> = func_desc
                                .params
                                .into_iter()
                                .map(|param| (param.name, param.data_type))
                                .collect();
                            Ok(TypedExpr::user_function(
                                func_desc.name,
                                vec![typed_source],
                                func_desc.body,
                                param_pairs,
                                func_desc.return_type,
                                func_desc.language,
                            ))
                        }
                        _ => Ok(compat_cast_expr(
                            typed_source,
                            &source_type_name,
                            &normalized_type_name,
                        )),
                    };
                }
            }
            // When casting to VARCHAR (character varying), strip trailing
            // CHAR(n) padding from the source if it comes from a bpchar column.
            // In PostgreSQL, CAST(bpchar_val AS varchar) removes blank padding.
            if type_name.eq_ignore_ascii_case("character varying") {
                if let Some(cast_source) = extract_cast_source(expr) {
                    if argument_uses_bpchar_padding(cast_source, relation) {
                        let typed_inner = infer_expr(expr, relation, params, sq, uf)?;
                        let nullable = typed_inner.nullable;
                        return Ok(TypedExpr::scalar_function(
                            ScalarFunction::Rtrim,
                            vec![typed_inner],
                            DataType::Text,
                            nullable,
                        ));
                    }
                }
            }
        }
        if let Some(expr) = args.first() {
            return infer_expr(expr, relation, params, sq, uf);
        }
        return Err(DbError::internal(
            "__aiondb_type_hint() requires an expression argument",
        ));
    }
    if function_name.eq_ignore_ascii_case("__aiondb_temporal_precision") {
        if args.len() != 2 {
            return Err(DbError::internal(
                "__aiondb_temporal_precision() requires exactly two arguments",
            ));
        }
        let value = infer_expr(&args[0], relation, params, sq, uf)?;
        if !matches!(
            value.data_type,
            DataType::Time | DataType::TimeTz | DataType::Timestamp | DataType::TimestampTz
        ) {
            return Err(DbError::internal(
                "__aiondb_temporal_precision() first argument must be temporal",
            ));
        }
        let precision = super::expr_helpers::infer_expr_with_expected(
            &args[1],
            relation,
            &DataType::Int,
            false,
            params,
            sq,
            uf,
        )?;
        let return_type = value.data_type.clone();
        let nullable = value.nullable || precision.nullable;
        let all_literals = matches!(value.kind, TypedExprKind::Literal(_))
            && matches!(precision.kind, TypedExprKind::Literal(_));
        let typed_expr = TypedExpr::scalar_function(
            ScalarFunction::Generic("__aiondb_temporal_precision".to_owned()),
            vec![value, precision],
            return_type,
            nullable,
        );
        if all_literals {
            if let Ok(value) = aiondb_eval::ExpressionEvaluator.evaluate(&typed_expr) {
                let is_null = value.is_null();
                return Ok(TypedExpr::literal(value, typed_expr.data_type, is_null));
            }
        }
        return Ok(typed_expr);
    }
    if function_name.eq_ignore_ascii_case("__aiondb_char_pad_length") {
        if args.len() != 2 {
            return Err(DbError::internal(
                "__aiondb_char_pad_length() requires exactly two arguments",
            ));
        }
        let value = infer_expr(&args[0], relation, params, sq, uf)?;
        let length = super::expr_helpers::infer_expr_with_expected(
            &args[1],
            relation,
            &DataType::Int,
            false,
            params,
            sq,
            uf,
        )?;
        let nullable = value.nullable || length.nullable;
        let all_literals = matches!(value.kind, TypedExprKind::Literal(_))
            && matches!(length.kind, TypedExprKind::Literal(_));
        let typed_expr = TypedExpr::scalar_function(
            ScalarFunction::Generic("__aiondb_char_pad_length".to_owned()),
            vec![value, length],
            DataType::Text,
            nullable,
        );
        if all_literals {
            if let Ok(val) = aiondb_eval::ExpressionEvaluator.evaluate(&typed_expr) {
                let is_null = val.is_null();
                return Ok(TypedExpr::literal(val, DataType::Text, is_null));
            }
        }
        return Ok(typed_expr);
    }
    if function_name.eq_ignore_ascii_case("__aiondb_json_array_subquery") {
        if args.len() != 1 {
            return Err(DbError::Bind(Box::new(
                ErrorReport::new(
                    SqlState::SyntaxError,
                    "__aiondb_json_array_subquery() requires exactly one argument",
                )
                .with_position(span.start + 1),
            )));
        }
        let query = match &args[0] {
            Expr::ArraySubquery { query, .. } | Expr::Subquery { query, .. } => query.as_ref(),
            _ => {
                return Err(DbError::Bind(Box::new(
                    ErrorReport::new(
                        SqlState::SyntaxError,
                        "__aiondb_json_array_subquery() expects a subquery argument",
                    )
                    .with_position(span.start + 1),
                )));
            }
        };
        let Some(sq) = sq else {
            let empty_array = TypedExpr::literal(
                Value::Array(Vec::new()),
                DataType::Array(Box::new(DataType::Text)),
                false,
            );
            return Ok(TypedExpr::scalar_function(
                ScalarFunction::Generic("__aiondb_json_array_subquery".to_owned()),
                vec![empty_array],
                DataType::Jsonb,
                false,
            ));
        };
        let result = sq(query)?;
        params.merge_inferred(&result.param_types)?;
        if result.num_columns != 1 {
            return Err(DbError::Bind(Box::new(
                ErrorReport::new(
                    SqlState::SyntaxError,
                    "subquery must return only one column",
                )
                .with_position(span.start + 1),
            )));
        }
        let array_expr = TypedExpr::array_subquery(
            result.plan,
            DataType::Array(Box::new(result.output_type.clone())),
        );
        return Ok(TypedExpr::scalar_function(
            ScalarFunction::Generic("__aiondb_json_array_subquery".to_owned()),
            vec![array_expr],
            DataType::Jsonb,
            false,
        ));
    }
    if function_name.eq_ignore_ascii_case("coalesce") {
        if args.is_empty() {
            return Err(DbError::Bind(Box::new(
                ErrorReport::new(
                    SqlState::SyntaxError,
                    "COALESCE requires at least one argument",
                )
                .with_position(span.start + 1),
            )));
        }
        let typed_args: Vec<TypedExpr> = args
            .iter()
            .map(|arg| infer_expr(arg, relation, params, sq, uf))
            .collect::<DbResult<Vec<_>>>()?;
        let data_type = typed_args[0].data_type.clone();
        return Ok(TypedExpr::coalesce(typed_args, data_type));
    }
    if function_name.eq_ignore_ascii_case("nullif") {
        if args.len() != 2 {
            return Err(DbError::Bind(Box::new(
                ErrorReport::new(
                    SqlState::SyntaxError,
                    "NULLIF requires exactly two arguments",
                )
                .with_position(span.start + 1),
            )));
        }
        let left = infer_expr(&args[0], relation, params, sq, uf)?;
        let right = infer_expr(&args[1], relation, params, sq, uf)?;
        let data_type = left.data_type.clone();
        return Ok(TypedExpr::nullif(left, right, data_type));
    }
    // enum_range(NULL::enum_type) - resolve at plan time by extracting the
    // enum type name from the type hint and looking up the registered labels.
    if function_name.eq_ignore_ascii_case("enum_range") {
        if args.is_empty() || args.len() > 2 {
            return Err(DbError::Bind(Box::new(
                ErrorReport::new(
                    SqlState::SyntaxError,
                    "enum_range requires one or two arguments",
                )
                .with_position(span.start + 1),
            )));
        }
        // Extract the enum type name from the first argument's type hint.
        let enum_type_name = type_hint_name(&args[0]).map(normalize_compat_type_name);
        if let Some(ref type_name) = enum_type_name {
            if let Some(labels) = with_current_session_context(|ctx| {
                ctx.compat_user_type(type_name).and_then(|user_type| {
                    (!user_type.enum_labels.is_empty()).then(|| {
                        user_type
                            .enum_labels
                            .iter()
                            .map(|label| Value::Text(label.clone()))
                            .collect::<Vec<_>>()
                    })
                })
            }) {
                return Ok(TypedExpr::literal(
                    Value::Array(labels),
                    DataType::Array(Box::new(DataType::Text)),
                    false,
                ));
            }
        }
        // Fallback: resolve as a generic scalar function so the executor
        // can attempt evaluation at runtime.
        let typed_args: Vec<TypedExpr> = args
            .iter()
            .map(|arg| infer_expr(arg, relation, params, sq, uf))
            .collect::<DbResult<Vec<_>>>()?;
        return Ok(TypedExpr::scalar_function(
            ScalarFunction::Generic("enum_range".to_owned()),
            typed_args,
            DataType::Array(Box::new(DataType::Text)),
            false,
        ));
    }
    if let Some(agg) = try_resolve_aggregate(
        function_name,
        args,
        agg_distinct,
        agg_filter,
        relation,
        params,
        span,
        sq,
        uf,
    )? {
        return Ok(agg);
    }
    let mut normalized_args = Vec::with_capacity(args.len());
    let mut variadic_markers = Vec::with_capacity(args.len());
    for arg in args {
        let (inner, marked) = strip_variadic_marker_expr(arg);
        normalized_args.push(inner.clone());
        variadic_markers.push(marked);
    }
    // User-defined function lookup via the catalog.
    // This must happen before built-in registry lookup so user
    // functions are not shadowed by generic/stub built-ins.
    if let Some(uf_resolver) = uf {
        let overloads = uf_resolver(&user_function_name)?;
        if !overloads.is_empty() {
            let typed_args: Vec<TypedExpr> = normalized_args
                .iter()
                .map(|arg| infer_expr(arg, relation, params, sq, Some(uf_resolver)))
                .collect::<DbResult<Vec<_>>>()?;

            let mut resolved_matches: Vec<(aiondb_catalog::FunctionDescriptor, Vec<TypedExpr>)> =
                Vec::new();
            let mut has_arity_candidate = false;
            let mut compatible_default_omissions = 0usize;
            for func_desc in overloads {
                if overload_accepts_arity(&func_desc, normalized_args.len()) {
                    has_arity_candidate = true;
                }
                if let Some(resolved_args) = resolve_user_function_args_for_overload(
                    &func_desc,
                    &normalized_args,
                    &variadic_markers,
                    &typed_args,
                    uf_resolver,
                )? {
                    resolved_matches.push((func_desc, resolved_args));
                } else if overload_default_omission_prefix_matches(
                    &func_desc,
                    &normalized_args,
                    &variadic_markers,
                    &typed_args,
                    uf_resolver,
                )? {
                    compatible_default_omissions += 1;
                }
            }

            if resolved_matches.is_empty() {
                if compatible_default_omissions > 1 {
                    return Err(ambiguous_user_function_error(
                        &user_function_name,
                        &typed_args,
                        span,
                    ));
                }
                if has_arity_candidate {
                    return Err(undefined_user_function_error(
                        &user_function_name,
                        &typed_args,
                        span,
                    ));
                }
                if aiondb_eval::FunctionRegistry::lookup(function_name).is_none() {
                    return Err(undefined_user_function_error(
                        &user_function_name,
                        &typed_args,
                        span,
                    ));
                }
                // No user-defined overload can possibly match this arity.
                // Fall through so built-in function resolution can still win.
            } else {
                if resolved_matches.len() > 1 {
                    let has_non_variadic = resolved_matches
                        .iter()
                        .any(|(desc, _)| !desc.params.iter().any(|param| param.variadic));
                    if has_non_variadic {
                        resolved_matches
                            .retain(|(desc, _)| !desc.params.iter().any(|param| param.variadic));
                    }
                }
            }
            if resolved_matches.len() > 1
                || (resolved_matches.len() == 1 && compatible_default_omissions > 0)
            {
                return Err(ambiguous_user_function_error(
                    &user_function_name,
                    &typed_args,
                    span,
                ));
            }
            if let Some((func_desc, resolved_args)) = resolved_matches.pop() {
                if let Some(inlined) =
                    try_inline_short_circuit_sql_case_when(&func_desc, &resolved_args)
                {
                    return Ok(inlined);
                }
                let mut param_pairs: Vec<(String, DataType)> = func_desc
                    .params
                    .into_iter()
                    .map(|p| (p.name, p.data_type))
                    .collect();
                param_pairs.extend(
                    func_desc
                        .out_params
                        .into_iter()
                        .map(|p| (p.name, p.data_type)),
                );
                return Ok(TypedExpr::user_function(
                    func_desc.name,
                    resolved_args,
                    func_desc.body,
                    param_pairs,
                    func_desc.return_type,
                    func_desc.language,
                ));
            }
        }
    }
    // Scalar function lookup via the registry
    if normalized_args.is_empty()
        && matches!(
            function_name.to_ascii_lowercase().as_str(),
            "num_nulls" | "num_nonnulls"
        )
    {
        let resolved_name = if name.parts.is_empty() {
            function_name.to_owned()
        } else {
            name.parts.join(".")
        };
        return Err(DbError::Bind(Box::new(
            ErrorReport::new(
                SqlState::UndefinedObject,
                format!("function {resolved_name}() does not exist"),
            )
            .with_client_hint(
                "No function matches the given name and argument types. You might need to add explicit type casts.",
            )
            .with_position(span.start + 1),
        )));
    }
    if let Some(info) = aiondb_eval::FunctionRegistry::lookup(function_name) {
        if let Some(max) = info.max_args {
            if normalized_args.len() < info.min_args || normalized_args.len() > max {
                return Err(DbError::Bind(Box::new(
                    ErrorReport::new(
                        SqlState::SyntaxError,
                        format!(
                            "{function_name}() expects {min}..={max} argument(s), got {actual}",
                            min = info.min_args,
                            actual = normalized_args.len(),
                        ),
                    )
                    .with_position(span.start + 1),
                )));
            }
        } else if normalized_args.len() < info.min_args {
            return Err(DbError::Bind(Box::new(
                ErrorReport::new(
                    SqlState::SyntaxError,
                    format!(
                        "{function_name}() expects at least {min} argument(s), got {actual}",
                        min = info.min_args,
                        actual = normalized_args.len(),
                    ),
                )
                .with_position(span.start + 1),
            )));
        }
        // For extract/date_part, the first argument is a date-field
        // identifier (EPOCH, CENTURY, etc.) that the parser emits as a
        // ColumnRef.  Convert it to a string literal so the evaluator
        // receives a text value.
        let is_extract = matches!(
            function_name.to_ascii_lowercase().as_str(),
            "extract" | "date_part" | "date_trunc"
        );
        let function_name_lower = function_name.to_ascii_lowercase();
        let mut typed_args: Vec<TypedExpr> = normalized_args
            .iter()
            .enumerate()
            .map(|(i, arg)| {
                if is_extract && i == 0 {
                    if let Expr::Identifier(col) = arg {
                        let field_name =
                            col.parts.last().map_or(String::new(), |s| s.to_lowercase());
                        return Ok(TypedExpr::literal(
                            Value::Text(field_name),
                            DataType::Text,
                            false,
                        ));
                    }
                }
                if matches!(
                    function_name_lower.as_str(),
                    "to_regclass"
                        | "to_regtype"
                        | "to_regnamespace"
                        | "to_regrole"
                        | "to_regproc"
                        | "to_regprocedure"
                        | "to_regoper"
                        | "to_regoperator"
                        | "regclass"
                        | "regtype"
                ) && i == 0
                {
                    return super::expr_helpers::infer_expr_with_expected(
                        arg,
                        relation,
                        &DataType::Text,
                        true,
                        params,
                        sq,
                        uf,
                    );
                }
                if function_name_lower == "current_setting" {
                    let expected_type = if i == 0 {
                        Some(DataType::Text)
                    } else if i == 1 {
                        Some(DataType::Boolean)
                    } else {
                        None
                    };
                    if let Some(expected_type) = expected_type {
                        return super::expr_helpers::infer_expr_with_expected(
                            arg,
                            relation,
                            &expected_type,
                            true,
                            params,
                            sq,
                            uf,
                        );
                    }
                }
                if function_name_lower == "currval" && i == 0 {
                    let coerced_arg = unwrap_regclass_cast_literal(arg).unwrap_or(arg);
                    return super::expr_helpers::infer_expr_with_expected(
                        coerced_arg,
                        relation,
                        &DataType::Text,
                        true,
                        params,
                        sq,
                        uf,
                    );
                }
                if function_name_lower == "setval" {
                    let expected_type = if i == 0 {
                        Some(DataType::Text)
                    } else if i == 1 {
                        Some(DataType::BigInt)
                    } else if i == 2 {
                        Some(DataType::Boolean)
                    } else {
                        None
                    };
                    if let Some(expected_type) = expected_type {
                        let coerced_arg = if i == 0 {
                            unwrap_regclass_cast_literal(arg).unwrap_or(arg)
                        } else {
                            arg
                        };
                        return super::expr_helpers::infer_expr_with_expected(
                            coerced_arg,
                            relation,
                            &expected_type,
                            true,
                            params,
                            sq,
                            uf,
                        );
                    }
                }
                if matches!(
                    function_name_lower.as_str(),
                    "pg_get_indexdef" | "pg_catalog.pg_get_indexdef"
                ) && i == 0
                {
                    // PostgreSQL accepts either an index OID or an index name
                    // here. Forcing INT would reject valid text names before
                    // the executor can resolve them via search_path.
                    return infer_expr(arg, relation, params, sq, uf);
                }
                // General pg_catalog function argument type inference.
                // When a parameter ($N) is used as an argument to a known
                // function, try to infer its type from the function signature.
                if let Some(expected) = pg_func_arg_type(&function_name_lower, i) {
                    return super::expr_helpers::infer_expr_with_expected(
                        arg, relation, &expected, true, params, sq, uf,
                    );
                }
                let peer_index = usize::from(i == 0);
                if matches!(
                    info.func,
                    aiondb_plan::ScalarFunction::L2Distance
                        | aiondb_plan::ScalarFunction::CosineDistance
                        | aiondb_plan::ScalarFunction::InnerProduct
                        | aiondb_plan::ScalarFunction::ManhattanDistance
                        | aiondb_plan::ScalarFunction::NegativeInnerProduct
                ) && normalized_args.len() == 2
                    && matches!(arg, Expr::Parameter { .. })
                    && !matches!(normalized_args[peer_index], Expr::Parameter { .. })
                {
                    let peer_typed =
                        infer_expr(&normalized_args[peer_index], relation, params, sq, uf)?;
                    if matches!(peer_typed.data_type, DataType::Vector { .. }) {
                        return super::expr_helpers::infer_expr_with_expected(
                            arg,
                            relation,
                            &peer_typed.data_type,
                            true,
                            params,
                            sq,
                            uf,
                        );
                    }
                }
                infer_expr(arg, relation, params, sq, uf)
            })
            .collect::<DbResult<Vec<_>>>()?;

        if matches!(
            function_name_lower.as_str(),
            "jsonb_populate_record"
                | "json_populate_record"
                | "jsonb_populate_recordset"
                | "json_populate_recordset"
        ) {
            let min_with_metadata = 4;
            if typed_args.len() < min_with_metadata {
                let metadata = normalized_args
                    .first()
                    .and_then(|arg| json_record_metadata_from_type_hint(arg))
                    .or_else(|| {
                        typed_args
                            .first()
                            .and_then(json_record_metadata_from_row_expr)
                    });
                if let Some((keys, modes)) = metadata {
                    typed_args.push(typed_text_array_literal(keys));
                    typed_args.push(typed_text_array_literal(modes));
                }
            }
        }

        if function_name_lower == "__aiondb_is_json" {
            let input_type = &typed_args[0].data_type;
            if !matches!(
                input_type,
                DataType::Text | DataType::Blob | DataType::Jsonb
            ) {
                return Err(DbError::Bind(Box::new(
                    ErrorReport::new(
                        SqlState::DatatypeMismatch,
                        format!(
                            "cannot use type {} in IS JSON predicate",
                            input_type.pg_type_name()
                        ),
                    )
                    .with_position(normalized_args[0].span().start + 1),
                )));
            }
        }
        if matches!(
            info.func,
            aiondb_plan::ScalarFunction::L2Distance
                | aiondb_plan::ScalarFunction::CosineDistance
                | aiondb_plan::ScalarFunction::InnerProduct
                | aiondb_plan::ScalarFunction::ManhattanDistance
                | aiondb_plan::ScalarFunction::NegativeInnerProduct
        ) && typed_args.len() == 2
        {
            let left_type = typed_args[0].data_type.clone();
            let right_type = typed_args[1].data_type.clone();
            if matches!(left_type, DataType::Vector { .. })
                && matches!(right_type, DataType::Text)
                && matches!(
                    normalized_args[1],
                    Expr::Literal(Literal::String(_), _) | Expr::Parameter { .. }
                )
            {
                typed_args[1] = super::expr_helpers::infer_expr_with_expected(
                    &normalized_args[1],
                    relation,
                    &left_type,
                    true,
                    params,
                    sq,
                    uf,
                )?;
            } else if matches!(left_type, DataType::Text)
                && matches!(right_type, DataType::Vector { .. })
                && matches!(
                    normalized_args[0],
                    Expr::Literal(Literal::String(_), _) | Expr::Parameter { .. }
                )
            {
                typed_args[0] = super::expr_helpers::infer_expr_with_expected(
                    &normalized_args[0],
                    relation,
                    &right_type,
                    true,
                    params,
                    sq,
                    uf,
                )?;
            }
        }
        if matches!(function_name_lower.as_str(), "lower" | "upper")
            && typed_args.len() == 1
            && argument_uses_bpchar_padding(&normalized_args[0], relation)
        {
            let nullable = typed_args[0].nullable;
            typed_args[0] = TypedExpr::scalar_function(
                aiondb_plan::ScalarFunction::Rtrim,
                vec![typed_args[0].clone()],
                DataType::Text,
                nullable,
            );
        }
        let nullable = typed_args.iter().any(|a| a.nullable);
        let mut resolved_func = info.func.clone();
        if function_name_lower == "width_bucket"
            && typed_args.len() == 2
            && !width_bucket_array_types_compatible(
                &typed_args[0].data_type,
                &typed_args[1].data_type,
            )
        {
            let signature = typed_args
                .iter()
                .map(|arg| width_bucket_signature_type_name(&arg.data_type))
                .collect::<Vec<_>>()
                .join(", ");
            return Err(DbError::Bind(Box::new(
                ErrorReport::new(
                    SqlState::UndefinedObject,
                    format!("function width_bucket({signature}) does not exist"),
                )
                .with_client_hint(
                    "No function matches the given name and argument types. You might need to add explicit type casts.",
                )
                .with_position(span.start + 1),
            )));
        }
        if let Some(signature) = strict_text_function_signature(&function_name_lower, &typed_args) {
            return Err(DbError::Bind(Box::new(
                ErrorReport::new(
                    SqlState::UndefinedObject,
                    format!("function {function_name_lower}({signature}) does not exist"),
                )
                .with_client_hint(
                    "No function matches the given name and argument types. You might need to add explicit type casts.",
                )
                .with_position(span.start + 1),
            )));
        }
        if let Some(position) =
            invalid_variadic_argument_position(&function_name_lower, &normalized_args, &typed_args)
        {
            return Err(DbError::Bind(Box::new(
                ErrorReport::new(SqlState::SyntaxError, "VARIADIC argument must be an array")
                    .with_position(position),
            )));
        }
        // For generate_series, infer the return type from the arguments:
        // timestamp args → Timestamp, bigint args → BigInt, numeric → Numeric,
        // otherwise Int.
        let return_type = if matches!(resolved_func, aiondb_plan::ScalarFunction::GenerateSeries) {
            infer_generate_series_return_type(&typed_args)
        } else if matches!(resolved_func, aiondb_plan::ScalarFunction::Unnest) {
            infer_unnest_return_type(&typed_args)
        } else if function_name_lower == "trunc" || function_name_lower == "truncate" {
            typed_args
                .first()
                .map_or(info.return_type.clone(), |arg| match arg.data_type {
                    DataType::MacAddr => DataType::MacAddr,
                    DataType::MacAddr8 => DataType::MacAddr8,
                    _ => info.return_type.clone(),
                })
        } else if matches!(resolved_func, aiondb_plan::ScalarFunction::ArrayGet) {
            ensure_array_subscript_base_supported(
                &normalized_args,
                &typed_args,
                relation,
                span,
                true,
            )?;
            infer_array_get_return_type(&typed_args)
        } else if matches!(resolved_func, aiondb_plan::ScalarFunction::ArraySlice) {
            if should_use_fixed_array_slice(&normalized_args, &typed_args) {
                resolved_func = aiondb_plan::ScalarFunction::FixedArraySlice;
            } else {
                ensure_array_subscript_base_supported(
                    &normalized_args,
                    &typed_args,
                    relation,
                    span,
                    false,
                )?;
            }
            infer_array_slice_return_type(&typed_args)
        } else if matches!(resolved_func, aiondb_plan::ScalarFunction::ArrayAssign) {
            if should_use_fixed_array_assign(&normalized_args, &typed_args) {
                resolved_func = aiondb_plan::ScalarFunction::FixedArrayAssign;
            }
            infer_array_assign_return_type(&typed_args)
        } else if matches!(resolved_func, aiondb_plan::ScalarFunction::L2Normalize) {
            infer_l2_normalize_return_type(&typed_args, info.return_type)
        } else if matches!(resolved_func, aiondb_plan::ScalarFunction::Subvector) {
            infer_subvector_return_type(&typed_args, info.return_type)
        } else if let Some(lo_return_type) = lo_function_return_type(&function_name_lower) {
            lo_return_type
        } else {
            info.return_type
        };
        if matches!(resolved_func, aiondb_plan::ScalarFunction::PgTypeof) && typed_args.len() == 1 {
            return Ok(TypedExpr::literal(
                Value::Text(pg_typeof_name_for_expr_arg(
                    &normalized_args[0],
                    &typed_args[0],
                )),
                DataType::Text,
                false,
            ));
        }
        let try_interval_fold = function_name_lower == "__aiondb_interval_fields"
            && typed_args
                .iter()
                .all(|arg| matches!(arg.kind, TypedExprKind::Literal(_)));
        let typed_expr =
            TypedExpr::scalar_function(resolved_func, typed_args, return_type, nullable);
        if try_interval_fold {
            match aiondb_eval::ExpressionEvaluator.evaluate(&typed_expr) {
                Ok(value) => {
                    let is_null = value.is_null();
                    return Ok(TypedExpr::literal(value, typed_expr.data_type, is_null));
                }
                Err(err) => {
                    let mut report = err.report().clone();
                    report.position = Some(normalized_args[0].span().start + 1);
                    return Err(DbError::Bind(Box::new(report)));
                }
            }
        }
        return Ok(typed_expr);
    }
    if aiondb_eval::FunctionRegistry::lookup_reserved(function_name).is_some() {
        return Err(DbError::Bind(Box::new(
            ErrorReport::new(
                SqlState::FeatureNotSupported,
                format!(
                    "function \"{}\" is recognized but not implemented",
                    function_name
                ),
            )
            .with_position(span.start + 1),
        )));
    }
    if function_name.eq_ignore_ascii_case("nextval") {
        if args.len() != 1 {
            return Err(DbError::Bind(Box::new(
                ErrorReport::new(
                    SqlState::SyntaxError,
                    "NEXTVAL expects exactly one string literal argument",
                )
                .with_position(span.start + 1),
            )));
        }
        // Extract sequence name from: 'name', 'name'::text, 'name'::regclass,
        // CAST('name' AS regclass), or any other unary type-hint wrapping a
        // string literal. PG accepts nextval(text) and nextval(regclass)
        // interchangeably; both ultimately discriminate by the literal text.
        // The parser wraps `'X'::regclass` as `__aiondb_type_hint(Expr::Cast,
        // "regclass")`, so we recurse through both Cast and that wrapper.
        fn unwrap_seq_name_literal(expr: &Expr) -> Option<String> {
            match expr {
                Expr::Literal(Literal::String(s), _) => Some(s.clone()),
                Expr::Cast { expr, .. } => unwrap_seq_name_literal(expr),
                Expr::FunctionCall { name, args, .. } => {
                    let last = name.parts.last().map(String::as_str).unwrap_or("");
                    if matches!(
                        last,
                        "__aiondb_type_hint" | "__aiondb_regclass_cast" | "__aiondb_compat_cast"
                    ) {
                        args.first().and_then(unwrap_seq_name_literal)
                    } else {
                        None
                    }
                }
                _ => None,
            }
        }
        let seq_name = unwrap_seq_name_literal(&args[0]);
        return match seq_name {
            Some(name) => Ok(TypedExpr::next_value(name)),
            None => Err(DbError::Bind(Box::new(
                ErrorReport::new(
                    SqlState::SyntaxError,
                    "NEXTVAL expects a string literal sequence name",
                )
                .with_position(args[0].span().start + 1),
            ))),
        };
    }
    // User-defined compat types can be called like constructors, including
    // custom range/multirange type names created via CREATE TYPE ... AS RANGE.
    // If no built-in/user function matched above, keep it bindable as a
    // generic scalar call and let evaluation perform constructor semantics.
    let normalized_function_name = normalize_compat_type_name(function_name);
    let constructor_sources = with_current_session_context(|ctx| {
        ctx.compat_user_casts
            .iter()
            .filter(|cast| {
                cast.target_type == normalized_function_name && cast.source_type.ends_with("range")
            })
            .map(|cast| cast.source_type.clone())
            .collect::<Vec<_>>()
    });
    let is_compat_constructor_like = is_compat_user_type_name(&normalized_function_name)
        || !constructor_sources.is_empty()
        || with_current_session_context(|ctx| {
            ctx.compat_user_casts
                .iter()
                .any(|cast| cast.source_type == normalized_function_name)
        });
    if is_compat_constructor_like {
        let typed_args: Vec<TypedExpr> = normalized_args
            .iter()
            .map(|arg| infer_expr(arg, relation, params, sq, uf))
            .collect::<DbResult<Vec<_>>>()?;
        if !constructor_sources.is_empty() {
            let mut incompatible = false;
            for (arg_expr, typed_arg) in normalized_args.iter().zip(typed_args.iter()) {
                if matches!(typed_arg.kind, TypedExprKind::Literal(Value::Null)) {
                    continue;
                }
                let arg_type_name =
                    normalize_compat_type_name(&expr_type_name(arg_expr, typed_arg));
                let acceptable = arg_type_name == normalized_function_name
                    || arg_type_name == "text"
                    || constructor_sources
                        .iter()
                        .any(|source| source == &arg_type_name)
                    || find_compat_cast(&arg_type_name, &normalized_function_name, false).is_some();
                if !acceptable {
                    incompatible = true;
                    break;
                }
            }
            if incompatible {
                let signature = normalized_args
                    .iter()
                    .zip(typed_args.iter())
                    .map(|(arg, typed)| {
                        let type_name = expr_type_name(arg, typed);
                        compat_display_type_name(&normalize_compat_type_name(&type_name))
                    })
                    .collect::<Vec<_>>()
                    .join(", ");
                return Err(DbError::Bind(Box::new(
                    ErrorReport::new(
                        SqlState::UndefinedObject,
                        format!("function {user_function_name}({signature}) does not exist"),
                    )
                    .with_client_hint(
                        "No function matches the given name and argument types. You might need to add explicit type casts.",
                    )
                    .with_position(span.start + 1),
                )));
            }
        }
        let nullable = typed_args.iter().any(|arg| arg.nullable);
        return Ok(TypedExpr::scalar_function(
            ScalarFunction::Generic(normalized_function_name),
            typed_args,
            DataType::Text,
            nullable,
        ));
    }
    let resolved_name = if name.parts.is_empty() {
        function_name.to_owned()
    } else {
        name.parts.join(".")
    };
    Err(DbError::Bind(Box::new(
        ErrorReport::new(
            SqlState::UndefinedObject,
            format!("function \"{resolved_name}\" does not exist"),
        )
        .with_position(span.start + 1),
    )))
}

/// Unwrap `'name'::regclass` (and CAST('name' AS regclass)) to the underlying
/// string literal expression. The parser surrounds postfix-cast targets in
/// `__aiondb_type_hint(...)` and the binder wraps the cast itself in
/// `__aiondb_regclass_cast(...)`. For builtins like nextval/currval/setval
/// we only need the literal name, not its OID, so peel back to the inner
/// string expression when one is present.
fn unwrap_regclass_cast_literal(arg: &Expr) -> Option<&Expr> {
    fn inner(expr: &Expr) -> Option<&Expr> {
        match expr {
            Expr::Literal(Literal::String(_), _) => Some(expr),
            Expr::Cast { expr, .. } => inner(expr),
            Expr::FunctionCall { name, args, .. } => {
                let last = name.parts.last().map(String::as_str).unwrap_or("");
                if matches!(
                    last,
                    "__aiondb_type_hint" | "__aiondb_regclass_cast" | "__aiondb_compat_cast"
                ) {
                    args.first().and_then(inner)
                } else {
                    None
                }
            }
            _ => None,
        }
    }
    inner(arg)
}

fn argument_uses_bpchar_padding(arg: &Expr, relation: Option<&TableDescriptor>) -> bool {
    let Expr::Identifier(name) = arg else {
        return false;
    };
    let Some(relation) = relation else {
        return false;
    };
    let column_name = name.parts.last().map_or("", String::as_str);
    relation.columns.iter().any(|column| {
        column.name.eq_ignore_ascii_case(column_name)
            && matches!(
                column.text_type_modifier,
                Some(TextTypeModifier::Char { .. })
            )
    })
}

/// Extract the source expression from a `CAST(source AS type)` node,
/// unwrapping any `__aiondb_char_pad_length` wrapper first.
fn extract_cast_source(expr: &Expr) -> Option<&Expr> {
    match expr {
        Expr::Cast { expr: source, .. } => {
            // Unwrap __aiondb_char_pad_length if present
            if let Expr::FunctionCall { name, args, .. } = source.as_ref() {
                if name
                    .parts
                    .last()
                    .is_some_and(|p| p.eq_ignore_ascii_case("__aiondb_char_pad_length"))
                {
                    return args.first();
                }
            }
            Some(source.as_ref())
        }
        _ => None,
    }
}

/// Infer the element return type of `generate_series` from its arguments.
///
/// PostgreSQL matches the return type to the argument types:
///   - timestamp/timestamptz args → Timestamp/TimestampTz
///   - numeric args → Numeric
///   - bigint args → BigInt
///   - otherwise → Int
/// Infer the element return type of `unnest` from its array argument.
///
/// If the argument is `Array(T)`, the return type is `T`.
/// Otherwise, fall back to `Text`.
fn infer_unnest_return_type(args: &[TypedExpr]) -> DataType {
    if let Some(arg) = args.first() {
        if let DataType::Array(inner) = &arg.data_type {
            return *inner.clone();
        }
    }
    DataType::Text
}

/// Infer the return type of `array_get(arr, idx)`.
///
/// If the first argument is `Array(T)`, the result is `T`.
/// For nested subscripts like `arr[1][2]`, the first `array_get` returns
/// the sub-array type, and the second returns the element type.
fn infer_array_get_return_type(args: &[TypedExpr]) -> DataType {
    if let Some(arg) = args.first() {
        if matches!(arg.data_type, DataType::Jsonb) {
            return DataType::Jsonb;
        }
        if let DataType::Array(inner) = &arg.data_type {
            return *inner.clone();
        }
    }
    DataType::Text
}

fn infer_array_slice_return_type(args: &[TypedExpr]) -> DataType {
    args.first()
        .map_or(DataType::Text, |arg| arg.data_type.clone())
}

fn infer_array_assign_return_type(args: &[TypedExpr]) -> DataType {
    args.first()
        .map_or(DataType::Text, |arg| arg.data_type.clone())
}

fn infer_l2_normalize_return_type(args: &[TypedExpr], fallback: DataType) -> DataType {
    if let Some(arg) = args.first() {
        if let DataType::Vector { dims, element_type } = &arg.data_type {
            return DataType::Vector {
                dims: *dims,
                element_type: *element_type,
            };
        }
    }
    fallback
}

fn infer_subvector_return_type(args: &[TypedExpr], fallback: DataType) -> DataType {
    let Some(arg) = args.first() else {
        return fallback;
    };
    let DataType::Vector { element_type, .. } = &arg.data_type else {
        return fallback;
    };
    let dims = args
        .get(2)
        .and_then(|count| match &count.kind {
            TypedExprKind::Literal(Value::Int(value)) if *value > 0 => u32::try_from(*value).ok(),
            TypedExprKind::Literal(Value::BigInt(value)) if *value > 0 => {
                u32::try_from(*value).ok()
            }
            _ => None,
        })
        .unwrap_or(0);
    DataType::Vector {
        dims,
        element_type: *element_type,
    }
}

fn width_bucket_array_types_compatible(
    operand_type: &DataType,
    thresholds_type: &DataType,
) -> bool {
    let DataType::Array(threshold_element_type) = thresholds_type else {
        return true;
    };
    if matches!(threshold_element_type.as_ref(), DataType::Array(_)) {
        return true;
    }
    operand_type == threshold_element_type.as_ref()
        || (is_numeric_without_money(operand_type)
            && is_numeric_without_money(threshold_element_type.as_ref()))
}

fn width_bucket_signature_type_name(data_type: &DataType) -> String {
    match data_type {
        DataType::Int => "integer".to_owned(),
        DataType::BigInt => "bigint".to_owned(),
        DataType::Real => "real".to_owned(),
        DataType::Double => "double precision".to_owned(),
        DataType::Numeric => "numeric".to_owned(),
        DataType::Text => "text".to_owned(),
        DataType::Boolean => "boolean".to_owned(),
        DataType::Timestamp => "timestamp without time zone".to_owned(),
        DataType::TimeTz => "time with time zone".to_owned(),
        DataType::TimestampTz => "timestamp with time zone".to_owned(),
        DataType::Array(inner) => format!("{}[]", width_bucket_signature_type_name(inner)),
        other => other.to_string().to_ascii_lowercase(),
    }
}

fn pg_typeof_name_for_data_type(data_type: &DataType) -> String {
    match data_type {
        DataType::Array(inner) => format!("{}[]", pg_typeof_name_for_data_type(inner)),
        _ => data_type.pg_type_name().to_owned(),
    }
}

fn strict_text_function_signature(function_name: &str, typed_args: &[TypedExpr]) -> Option<String> {
    let allowed = match function_name {
        "length" => {
            typed_args.len() == 1
                && matches!(typed_args[0].data_type, DataType::Text | DataType::Blob)
        }
        "char_length" | "character_length" => {
            typed_args.len() == 1 && matches!(typed_args[0].data_type, DataType::Text)
        }
        "octet_length" => {
            typed_args.len() == 1
                && matches!(typed_args[0].data_type, DataType::Text | DataType::Blob)
        }
        _ => return None,
    };
    if allowed {
        return None;
    }
    Some(
        typed_args
            .iter()
            .map(|arg| arg.data_type.pg_type_name().to_owned())
            .collect::<Vec<_>>()
            .join(", "),
    )
}

fn invalid_variadic_argument_position(
    function_name: &str,
    args: &[Expr],
    typed_args: &[TypedExpr],
) -> Option<usize> {
    let variadic_index = match function_name {
        "__aiondb_variadic_concat" => 0,
        "__aiondb_variadic_concat_ws" | "__aiondb_variadic_format" => 1,
        _ => return None,
    };
    let arg = args.get(variadic_index)?;
    let typed_arg = typed_args.get(variadic_index)?;
    if matches!(typed_arg.data_type, DataType::Array(_))
        || matches!(typed_arg.kind, TypedExprKind::Literal(Value::Null))
    {
        return None;
    }
    Some(arg.span().start + 1)
}

fn pg_typeof_name_for_expr_arg(expr: &Expr, typed_expr: &TypedExpr) -> String {
    if let Expr::FunctionCall { name, args, .. } = expr {
        if name
            .parts
            .last()
            .is_some_and(|part| part.eq_ignore_ascii_case("__aiondb_type_hint"))
        {
            if let Some(Expr::Literal(Literal::String(type_name), _)) = args.get(1) {
                return type_name.clone();
            }
            if let Some(inner) = args.first() {
                return pg_typeof_name_for_expr_arg(inner, typed_expr);
            }
        }
    }
    pg_typeof_name_for_data_type(&typed_expr.data_type)
}

fn should_use_fixed_array_slice(args: &[Expr], typed_args: &[TypedExpr]) -> bool {
    let Some(base_expr) = args.first() else {
        return false;
    };
    let Some(base_typed) = typed_args.first() else {
        return false;
    };
    matches!(base_expr, Expr::Identifier(_)) && matches!(base_typed.data_type, DataType::Text)
}

fn should_use_fixed_array_assign(args: &[Expr], typed_args: &[TypedExpr]) -> bool {
    let Some(base_expr) = args.first() else {
        return false;
    };
    let Some(base_typed) = typed_args.first() else {
        return false;
    };
    // Do not use the fixed-length path when the base expression is a NULL
    // literal.  In INSERT context the base starts as NULL and the general
    // array assignment must create a proper variable-length array.
    if base_is_null_literal(base_expr) {
        return false;
    }
    matches!(base_typed.data_type, DataType::Text)
        && (matches!(base_expr, Expr::Identifier(_)) || is_nested_array_assign_expr(base_expr))
}

/// Returns true when `expr` is a NULL literal or a nested internal
/// function call (`__aiondb_array_assign`, `__aiondb_composite_field`,
/// `array_get`) whose ultimate base is a NULL literal.  This prevents
/// the type checker from routing INSERT-time array assignments (where
/// the column starts as NULL) through the fixed-length path.
fn base_is_null_literal(expr: &Expr) -> bool {
    match expr {
        Expr::Literal(Literal::Null, _) => true,
        Expr::FunctionCall { name, args, .. } => {
            let fname = name.parts.last().map_or("", |s| s.as_str());
            let is_passthrough = fname.eq_ignore_ascii_case("__aiondb_array_assign")
                || fname.eq_ignore_ascii_case("__aiondb_composite_field")
                || fname.eq_ignore_ascii_case("__aiondb_composite_assign")
                || fname.eq_ignore_ascii_case("array_get");
            is_passthrough && args.first().is_some_and(base_is_null_literal)
        }
        _ => false,
    }
}

fn is_nested_array_assign_expr(expr: &Expr) -> bool {
    matches!(
        expr,
        Expr::FunctionCall { name, .. }
            if name
                .parts
                .last()
                .is_some_and(|part| part.eq_ignore_ascii_case("__aiondb_array_assign"))
    )
}

fn try_inline_short_circuit_sql_case_when(
    func_desc: &aiondb_catalog::FunctionDescriptor,
    resolved_args: &[TypedExpr],
) -> Option<TypedExpr> {
    if !func_desc.language.eq_ignore_ascii_case("sql") || resolved_args.len() != 3 {
        return None;
    }
    // Keep this intentionally narrow: only inline the canonical SQL IF shape.
    let body = func_desc
        .body
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .trim_end_matches(';')
        .to_ascii_lowercase();
    if body != "select case when $1 then $2 else $3 end" {
        return None;
    }
    if resolved_args[0].data_type != DataType::Boolean {
        return None;
    }

    let cond = resolved_args[0].clone();
    let mut then_expr = resolved_args[1].clone();
    let mut else_expr = resolved_args[2].clone();

    then_expr = contextualize_null(then_expr, &else_expr.data_type);
    else_expr = contextualize_null(else_expr, &then_expr.data_type);
    if then_expr.data_type != else_expr.data_type {
        return None;
    }

    let result_type = then_expr.data_type.clone();
    let nullable = then_expr.nullable || else_expr.nullable;
    Some(TypedExpr::case_when(
        vec![cond],
        vec![then_expr],
        Some(else_expr),
        result_type,
        nullable,
    ))
}

fn ensure_array_subscript_base_supported(
    args: &[Expr],
    typed_args: &[TypedExpr],
    relation: Option<&TableDescriptor>,
    span: aiondb_parser::Span,
    allow_jsonb: bool,
) -> DbResult<()> {
    let Some(base_expr) = args.first() else {
        return Ok(());
    };
    let Some(base_typed) = typed_args.first() else {
        return Ok(());
    };

    if matches!(base_typed.data_type, DataType::Array(_))
        || (allow_jsonb && matches!(base_typed.data_type, DataType::Jsonb))
        || is_nested_array_subscript_expr(base_expr)
        || matches!(base_expr, Expr::Parameter { .. })
        || matches!(base_typed.kind, TypedExprKind::Literal(Value::Null))
    {
        return Ok(());
    }
    if matches!(base_typed.data_type, DataType::Text)
        && infer_geometric_source_type_from_relation(base_expr, relation).is_some()
    {
        return Ok(());
    }

    Err(DbError::Bind(Box::new(
        ErrorReport::new(
            SqlState::SyntaxError,
            format!(
                "cannot subscript type {} because it does not support subscripting",
                base_typed.data_type.pg_type_name()
            ),
        )
        .with_position(span.start + 1),
    )))
}

fn is_nested_array_subscript_expr(expr: &Expr) -> bool {
    matches!(
        expr,
        Expr::FunctionCall { name, .. }
            if name
                .parts
                .last()
                .is_some_and(|part| {
                    part.eq_ignore_ascii_case("array_get")
                        || part.eq_ignore_ascii_case("__aiondb_array_slice")
                })
    )
}

fn infer_generate_series_return_type(args: &[TypedExpr]) -> DataType {
    for arg in args {
        match &arg.data_type {
            DataType::Timestamp => return DataType::Timestamp,
            DataType::TimestampTz => return DataType::TimestampTz,
            DataType::Interval => {} // interval is only for the step, don't use it as return type
            _ => {}
        }
    }
    for arg in args {
        if matches!(&arg.data_type, DataType::Numeric) {
            return DataType::Numeric;
        }
    }
    for arg in args {
        if matches!(&arg.data_type, DataType::BigInt) {
            return DataType::BigInt;
        }
    }
    DataType::Int
}

/// Return the expected `DataType` for the `arg_index`-th argument of a known
/// pg_catalog function.  Returns `None` when the type is unknown or when the
/// argument position is out of range.
fn pg_func_arg_type(name: &str, arg_index: usize) -> Option<DataType> {
    if let Some(lo_type) = lo_function_arg_type(name, arg_index) {
        return Some(lo_type);
    }

    // (function_name, [arg0_type, arg1_type, ...])
    match (name, arg_index) {
        // pg_get_constraintdef(oid, bool?) → text
        ("pg_get_constraintdef", 0) => Some(DataType::Int),
        ("pg_get_constraintdef", 1) => Some(DataType::Boolean),
        // pg_get_expr(adbin, adrelid, bool?) → text
        ("pg_get_expr", 0) => Some(DataType::Text),
        ("pg_get_expr", 1) => Some(DataType::Int),
        ("pg_get_expr", 2) => Some(DataType::Boolean),
        // pg_get_indexdef(oid, int?, bool?) → text
        ("pg_get_indexdef" | "pg_catalog.pg_get_indexdef", 0) => Some(DataType::Int),
        ("pg_get_indexdef" | "pg_catalog.pg_get_indexdef", 1) => Some(DataType::Int),
        ("pg_get_indexdef" | "pg_catalog.pg_get_indexdef", 2) => Some(DataType::Boolean),
        // obj_description(oid, text) / shobj_description(oid, text)
        ("obj_description" | "shobj_description" | "pg_catalog.shobj_description", 0) => {
            Some(DataType::Int)
        }
        ("obj_description" | "shobj_description" | "pg_catalog.shobj_description", 1) => {
            Some(DataType::Text)
        }
        ("col_description", 0) => Some(DataType::Int),
        ("col_description", 1) => Some(DataType::Int),
        // format_type(oid, int) → text
        ("format_type" | "pg_catalog.format_type", 0) => Some(DataType::Int),
        ("format_type" | "pg_catalog.format_type", 1) => Some(DataType::Int),
        // pg_*_is_visible(oid) → bool
        (
            "pg_table_is_visible"
            | "pg_catalog.pg_table_is_visible"
            | "pg_type_is_visible"
            | "pg_catalog.pg_type_is_visible"
            | "pg_function_is_visible"
            | "pg_catalog.pg_function_is_visible"
            | "pg_proc_is_visible"
            | "pg_catalog.pg_proc_is_visible"
            | "pg_operator_is_visible"
            | "pg_catalog.pg_operator_is_visible"
            | "pg_collation_is_visible"
            | "pg_catalog.pg_collation_is_visible"
            | "pg_opclass_is_visible"
            | "pg_catalog.pg_opclass_is_visible"
            | "pg_opfamily_is_visible"
            | "pg_catalog.pg_opfamily_is_visible"
            | "pg_ts_dict_is_visible"
            | "pg_catalog.pg_ts_dict_is_visible"
            | "pg_ts_config_is_visible"
            | "pg_catalog.pg_ts_config_is_visible"
            | "pg_ts_parser_is_visible"
            | "pg_catalog.pg_ts_parser_is_visible"
            | "pg_ts_template_is_visible"
            | "pg_catalog.pg_ts_template_is_visible"
            | "pg_conversion_is_visible"
            | "pg_catalog.pg_conversion_is_visible"
            | "pg_statistics_obj_is_visible"
            | "pg_catalog.pg_statistics_obj_is_visible",
            0,
        ) => Some(DataType::Int),
        // has_*_privilege functions
        (name, 0) if name.starts_with("has_") && name.ends_with("_privilege") => {
            Some(DataType::Text)
        }
        (name, 1) if name.starts_with("has_") && name.ends_with("_privilege") => {
            Some(DataType::Text)
        }
        // json_build_object takes alternating text keys and any values
        ("json_build_object", i) if i % 2 == 0 => Some(DataType::Text),
        // pg_get_serial_sequence(text, text) → text
        ("pg_get_serial_sequence", 0 | 1) => Some(DataType::Text),
        ("__aiondb_pg_char_cast", 0) => Some(DataType::Text),
        ("__aiondb_regclass_cast", 0) => Some(DataType::Text),
        ("__aiondb_regproc_cast", 0) => Some(DataType::Text),
        ("__aiondb_regprocedure_cast", 0) => Some(DataType::Text),
        ("__aiondb_regrole_cast", 0) => Some(DataType::Text),
        ("__aiondb_regclass_out", 0) => Some(DataType::Int),
        ("__aiondb_regproc_out", 0) => Some(DataType::Int),
        ("__aiondb_regprocedure_out", 0) => Some(DataType::Int),
        ("__aiondb_regrole_out", 0) => Some(DataType::Int),
        ("__aiondb_xid_cast", 0) => Some(DataType::Text),
        ("__aiondb_xid8_cast", 0) => Some(DataType::Text),
        ("__aiondb_pg_snapshot_cast", 0) => Some(DataType::Text),
        // Cast-like functions the parser generates for SMALLINT/INT2 and OID casts
        ("int2" | "oid", 0) => Some(DataType::Int),
        _ => None,
    }
}

fn lo_function_return_type(name: &str) -> Option<DataType> {
    match name {
        "lo_create" | "lo_creat" | "lo_import" | "lo_from_bytea" => Some(DataType::Int),
        "lo_open" | "lo_close" | "lo_unlink" | "lo_export" | "lo_lseek" | "lo_tell"
        | "lo_truncate" | "lowrite" | "lo_put" | "lo_truncate64" => Some(DataType::Int),
        "lo_lseek64" | "lo_tell64" => Some(DataType::BigInt),
        "loread" | "lo_get" => Some(DataType::Blob),
        _ => None,
    }
}

fn lo_function_arg_type(name: &str, arg_index: usize) -> Option<DataType> {
    match (name, arg_index) {
        ("lo_create" | "lo_creat", 0) => Some(DataType::Int),
        ("lo_open", 0 | 1) => Some(DataType::Int),
        ("lo_close", 0) => Some(DataType::Int),
        ("lo_unlink", 0) => Some(DataType::Int),
        ("loread", 0 | 1) => Some(DataType::Int),
        ("lowrite", 0) => Some(DataType::Int),
        ("lowrite", 1) => Some(DataType::Blob),
        ("lo_lseek", 0 | 1 | 2) => Some(DataType::Int),
        ("lo_tell", 0) => Some(DataType::Int),
        ("lo_truncate", 0 | 1) => Some(DataType::Int),
        ("lo_lseek64", 0 | 2) => Some(DataType::Int),
        ("lo_lseek64", 1) => Some(DataType::BigInt),
        ("lo_tell64", 0) => Some(DataType::Int),
        ("lo_truncate64", 0) => Some(DataType::Int),
        ("lo_truncate64", 1) => Some(DataType::BigInt),
        ("lo_import", 0) => Some(DataType::Text),
        ("lo_import", 1) => Some(DataType::Int),
        ("lo_export", 0) => Some(DataType::Int),
        ("lo_export", 1) => Some(DataType::Text),
        ("lo_get", 0) => Some(DataType::Int),
        ("lo_get", 1 | 2) => Some(DataType::BigInt),
        ("lo_put", 0) => Some(DataType::Int),
        ("lo_put", 1) => Some(DataType::BigInt),
        ("lo_put", 2) => Some(DataType::Blob),
        ("lo_from_bytea", 0) => Some(DataType::Int),
        ("lo_from_bytea", 1) => Some(DataType::Blob),
        _ => None,
    }
}
