use aiondb_core::{DataType, DbError, DbResult, ErrorReport, SqlState, Value};
use aiondb_eval::{
    compat_type_name_for_data_type, normalize_compat_type_name, with_current_session_context,
};
use aiondb_parser::{Expr, Literal};
use aiondb_plan::{ScalarFunction, TypedExpr, TypedExprKind};

use super::UserFunctionResolver;

pub(crate) fn unwrap_type_hint_expr(expr: &Expr) -> &Expr {
    match expr {
        Expr::FunctionCall { name, args, .. }
            if name
                .parts
                .last()
                .is_some_and(|part| part.eq_ignore_ascii_case("__aiondb_type_hint")) =>
        {
            args.first().unwrap_or(expr)
        }
        _ => expr,
    }
}

pub(crate) fn type_hint_name(expr: &Expr) -> Option<&str> {
    let Expr::FunctionCall { name, args, .. } = expr else {
        return None;
    };
    if !name
        .parts
        .last()
        .is_some_and(|part| part.eq_ignore_ascii_case("__aiondb_type_hint"))
    {
        return None;
    }
    match args.get(1) {
        Some(Expr::Literal(Literal::String(type_name), _)) => Some(type_name.as_str()),
        _ => None,
    }
}

pub(crate) fn regclass_lookup_source(expr: &Expr) -> Option<&Expr> {
    let Expr::Cast {
        expr: source,
        data_type,
        ..
    } = expr
    else {
        return None;
    };
    if !matches!(data_type, DataType::Int) {
        return None;
    }
    regclass_name_input_expr(source)
}

pub(crate) fn regclass_name_input_expr(expr: &Expr) -> Option<&Expr> {
    match unwrap_type_hint_expr(expr) {
        Expr::Literal(Literal::String(_), _) | Expr::Parameter { .. } => Some(expr),
        Expr::Cast { expr, .. } => regclass_name_input_expr(expr),
        _ => None,
    }
}

pub(crate) fn is_compat_user_type_name(type_name: &str) -> bool {
    with_current_session_context(|ctx| ctx.compat_user_type(type_name).is_some())
}

/// Check whether a cast from `source_compat` to a domain type `domain_name`
/// is allowed without an explicit cast registration.  This is the case when
/// the source type is the domain's base type, or when the source is another
/// domain over the same base type, or when the source is broadly compatible
/// (e.g. any numeric to a numeric domain).
pub(crate) fn is_domain_cast_compatible(source_compat: &str, domain_name: &str) -> bool {
    with_current_session_context(|ctx| {
        // Traverse the target domain chain to find the ultimate base type.
        let mut target_base = domain_name.to_owned();
        let mut found = false;
        while let Some(def) = ctx.domain_def(&target_base) {
            found = true;
            target_base = normalize_compat_type_name(&def.base_type);
        }
        if !found {
            return false;
        }
        // Resolve the source's ultimate base type if it is also a domain.
        let mut source_base = source_compat.to_owned();
        while let Some(def) = ctx.domain_def(&source_base) {
            source_base = normalize_compat_type_name(&def.base_type);
        }
        if source_base == target_base {
            return true;
        }
        // Allow broadly compatible numeric/text families.
        let numeric_types = ["int4", "int8", "float4", "float8", "numeric"];
        let text_types = ["text", "varchar", "char", "character varying", "name"];
        if numeric_types.contains(&target_base.as_str())
            && numeric_types.contains(&source_base.as_str())
        {
            return true;
        }
        if text_types.contains(&target_base.as_str()) && text_types.contains(&source_base.as_str())
        {
            return true;
        }
        false
    })
}

pub(crate) fn expr_type_name(expr: &Expr, typed_expr: &TypedExpr) -> String {
    if let Some(type_name) = type_hint_name(expr) {
        return normalize_compat_type_name(type_name);
    }
    if let Expr::Identifier(name) = unwrap_type_hint_expr(expr) {
        if let Some(pseudotype) =
            crate::pg_catalog::compat_pseudotype_for_column_identifier(&name.parts)
        {
            return pseudotype.to_owned();
        }
    }
    if let Some(inferred) = infer_expr_compat_type_name(expr, typed_expr) {
        return inferred;
    }
    compat_type_name_for_data_type(&typed_expr.data_type)
}

fn infer_expr_compat_type_name(expr: &Expr, typed_expr: &TypedExpr) -> Option<String> {
    if !matches!(typed_expr.data_type, DataType::Text) {
        return None;
    }
    if let TypedExprKind::ScalarFunction {
        func: ScalarFunction::Row,
        args,
    } = &typed_expr.kind
    {
        let mut matches = with_current_session_context(|ctx| {
            ctx.compat_user_types
                .iter()
                .filter(|entry| {
                    if entry.composite_fields.len() != args.len() {
                        return false;
                    }
                    entry
                        .composite_fields
                        .iter()
                        .zip(args.iter())
                        .all(|(field, arg)| field.data_type == arg.data_type)
                })
                .map(|entry| entry.name.clone())
                .collect::<Vec<_>>()
        });
        matches.sort();
        matches.dedup();
        if matches.len() == 1 {
            return matches.into_iter().next();
        }
    }
    if let Some(pseudo_type_name) = pseudo_anyarray_compat_type_name(expr) {
        return Some(pseudo_type_name);
    }
    let Expr::FunctionCall { name, args, .. } = expr else {
        return None;
    };
    let function_name = name.parts.last()?.to_ascii_lowercase();
    if is_known_range_type_name(&function_name) || is_known_multirange_type_name(&function_name) {
        return Some(function_name);
    }
    if function_name == "multirange" {
        let source_type = args.first().and_then(|arg| {
            infer_expr_compat_type_name(
                arg,
                &TypedExpr::literal(Value::Text(String::new()), DataType::Text, true),
            )
        })?;
        let subtype = range_subtype_type_name(&source_type)?;
        return Some(match subtype {
            "int4" => "int4multirange".to_owned(),
            "int8" => "int8multirange".to_owned(),
            "numeric" => "nummultirange".to_owned(),
            "float8" => "float8multirange".to_owned(),
            "date" => "datemultirange".to_owned(),
            "timestamp" => "tsmultirange".to_owned(),
            "timestamptz" => "tstzmultirange".to_owned(),
            _ => return None,
        });
    }
    None
}

pub(crate) fn find_compat_cast(
    source_type: &str,
    target_type: &str,
    implicit_only: bool,
) -> Option<aiondb_eval::CompatUserCast> {
    let source = normalize_compat_type_name(source_type);
    let target = normalize_compat_type_name(target_type);
    with_current_session_context(|ctx| {
        ctx.compat_user_casts
            .iter()
            .find(|entry| {
                entry.source_type == source
                    && entry.target_type == target
                    && (!implicit_only || entry.context.allows_implicit())
            })
            .cloned()
    })
}

pub(crate) fn compat_cast_expr(
    source: TypedExpr,
    source_type: &str,
    target_type: &str,
) -> TypedExpr {
    let nullable = source.nullable;
    // When the target is a domain type, resolve the return DataType from the
    // domain's base type so that downstream expressions (e.g. coalesce,
    // arithmetic) see the correct type instead of always DataType::Text.
    let return_type = with_current_session_context(|ctx| {
        // Traverse domain chain to find the ultimate base type.
        let mut base_name = target_type.to_owned();
        while let Some(def) = ctx.domain_def(&base_name) {
            base_name = normalize_compat_type_name(&def.base_type);
        }
        if base_name == target_type {
            DataType::Text
        } else {
            super::expr::domain_base_type_to_data_type(&base_name)
        }
    });
    TypedExpr::scalar_function(
        ScalarFunction::Generic("__aiondb_compat_cast".to_owned()),
        vec![
            source,
            TypedExpr::literal(Value::Text(source_type.to_owned()), DataType::Text, false),
            TypedExpr::literal(Value::Text(target_type.to_owned()), DataType::Text, false),
        ],
        return_type,
        nullable,
    )
}

pub(crate) fn undefined_user_function_error(
    function_name: &str,
    typed_args: &[TypedExpr],
    span: aiondb_parser::Span,
) -> DbError {
    let signature = user_function_signature(typed_args);
    DbError::Bind(Box::new(
        ErrorReport::new(
            SqlState::UndefinedObject,
            format!("function {function_name}({signature}) does not exist"),
        )
        .with_client_hint(
            "No function matches the given name and argument types. You might need to add explicit type casts.",
        )
        .with_position(span.start + 1),
    ))
}

pub(crate) fn ambiguous_user_function_error(
    function_name: &str,
    typed_args: &[TypedExpr],
    span: aiondb_parser::Span,
) -> DbError {
    let signature = user_function_signature(typed_args);
    DbError::Bind(Box::new(
        ErrorReport::new(
            SqlState::AmbiguousFunction,
            format!("function {function_name}({signature}) is not unique"),
        )
        .with_client_hint(
            "Could not choose a best candidate function. You might need to add explicit type casts.",
        )
        .with_position(span.start + 1),
    ))
}

pub(crate) fn user_function_signature(typed_args: &[TypedExpr]) -> String {
    typed_args
        .iter()
        .map(|arg| arg.data_type.pg_type_name().to_owned())
        .collect::<Vec<_>>()
        .join(", ")
}

pub(crate) fn find_unary_user_function_overload(
    uf_resolver: UserFunctionResolver<'_>,
    function_name: &str,
) -> DbResult<Option<aiondb_catalog::FunctionDescriptor>> {
    let mut overloads = uf_resolver(function_name)?;
    if let Some(position) = overloads.iter().position(|desc| desc.params.len() == 1) {
        return Ok(Some(overloads.remove(position)));
    }
    Ok(overloads.into_iter().next())
}

pub(crate) fn strip_variadic_marker_expr(expr: &Expr) -> (&Expr, bool) {
    let Expr::FunctionCall {
        name,
        args,
        distinct,
        filter,
        ..
    } = expr
    else {
        return (expr, false);
    };
    if *distinct || filter.is_some() || args.len() != 1 {
        return (expr, false);
    }
    if !name
        .parts
        .last()
        .is_some_and(|part| part.eq_ignore_ascii_case("__aiondb_variadic_arg"))
    {
        return (expr, false);
    }
    (args.first().unwrap_or(expr), true)
}

pub(crate) fn overload_accepts_arity(
    func_desc: &aiondb_catalog::FunctionDescriptor,
    arg_len: usize,
) -> bool {
    if let Some(index) = func_desc.params.iter().position(|param| param.variadic) {
        if index + 1 != func_desc.params.len() {
            return false;
        }
        let variadic_has_default = func_desc.params[index].has_default;
        let min_args = if variadic_has_default {
            index
        } else {
            index + 1
        };
        return arg_len >= min_args;
    }
    arg_len == func_desc.params.len()
}

pub(crate) fn overload_has_default_omission_candidate(
    func_desc: &aiondb_catalog::FunctionDescriptor,
    arg_len: usize,
) -> bool {
    if arg_len >= func_desc.params.len() {
        return false;
    }
    if func_desc.params.iter().any(|param| param.variadic) {
        return false;
    }
    func_desc.params[arg_len..]
        .iter()
        .all(|param| param.has_default)
}

pub(crate) fn overload_default_omission_prefix_matches(
    func_desc: &aiondb_catalog::FunctionDescriptor,
    args: &[Expr],
    variadic_markers: &[bool],
    typed_args: &[TypedExpr],
    uf_resolver: UserFunctionResolver<'_>,
) -> DbResult<bool> {
    if !overload_has_default_omission_candidate(func_desc, args.len()) {
        return Ok(false);
    }
    let mut prefix = func_desc.clone();
    prefix.params.truncate(args.len());
    Ok(resolve_user_function_args_for_overload(
        &prefix,
        args,
        variadic_markers,
        typed_args,
        uf_resolver,
    )?
    .is_some())
}

pub(crate) fn resolve_user_function_args_for_overload(
    func_desc: &aiondb_catalog::FunctionDescriptor,
    args: &[Expr],
    variadic_markers: &[bool],
    typed_args: &[TypedExpr],
    uf_resolver: UserFunctionResolver<'_>,
) -> DbResult<Option<Vec<TypedExpr>>> {
    if variadic_markers.len() != args.len() || typed_args.len() != args.len() {
        return Ok(None);
    }

    let variadic_index = func_desc.params.iter().position(|param| param.variadic);
    if let Some(index) = variadic_index {
        if index + 1 != func_desc.params.len() {
            return Ok(None);
        }
        let min_args = if func_desc.params[index].has_default {
            index
        } else {
            func_desc.params.len()
        };
        if args.len() < min_args {
            return Ok(None);
        }
        if variadic_markers
            .iter()
            .enumerate()
            .any(|(i, marked)| *marked && i != index)
        {
            return Ok(None);
        }
    } else if args.len() != func_desc.params.len() || variadic_markers.iter().any(|marked| *marked)
    {
        return Ok(None);
    }

    let mut resolved_args = if let Some(index) = variadic_index {
        let explicit_variadic = variadic_markers.get(index).copied().unwrap_or(false);
        if explicit_variadic && args.len() != func_desc.params.len() {
            return Ok(None);
        }
        let mut packed = typed_args[..index].to_vec();
        let variadic_arg = if explicit_variadic {
            typed_args[index].clone()
        } else {
            let elements = if args.len() > index {
                typed_args[index..].to_vec()
            } else {
                Vec::new()
            };
            let element_type = elements.first().map_or_else(
                || match &func_desc.params[index].data_type {
                    DataType::Array(inner) => (**inner).clone(),
                    _ => DataType::Text,
                },
                |expr| expr.data_type.clone(),
            );
            let nullable = elements.iter().any(|expr| expr.nullable);
            TypedExpr::array_construct(elements, element_type, nullable)
        };
        packed.push(variadic_arg);
        packed
    } else {
        typed_args.to_vec()
    };

    let actual_type_names: Vec<String> = func_desc
        .params
        .iter()
        .enumerate()
        .map(|(index, _)| {
            if variadic_index == Some(index) {
                compat_type_name_for_data_type(&resolved_args[index].data_type)
            } else if index < args.len() {
                expr_type_name(&args[index], &typed_args[index])
            } else {
                compat_type_name_for_data_type(&func_desc.params[index].data_type)
            }
        })
        .collect();

    for (index, param) in func_desc.params.iter().enumerate() {
        let expected_type_name = param.raw_type_name.as_deref().map_or_else(
            || compat_type_name_for_data_type(&param.data_type),
            normalize_compat_type_name,
        );
        let actual_type_name = &actual_type_names[index];

        if is_polymorphic_type_name(&expected_type_name) {
            continue;
        }
        if actual_type_name == &expected_type_name {
            continue;
        }
        if is_compat_user_type_name(&expected_type_name) {
            let Some(cast) = find_compat_cast(actual_type_name, &expected_type_name, true) else {
                return Ok(None);
            };
            resolved_args[index] = match cast.method {
                aiondb_eval::CompatCastMethod::Function {
                    function_name: ref cast_function,
                    ..
                } => {
                    let Some(cast_func_desc) =
                        find_unary_user_function_overload(uf_resolver, cast_function)?
                    else {
                        return Ok(None);
                    };
                    let param_pairs: Vec<(String, DataType)> = cast_func_desc
                        .params
                        .iter()
                        .map(|param| (param.name.clone(), param.data_type.clone()))
                        .collect();
                    TypedExpr::user_function(
                        cast_func_desc.name.clone(),
                        vec![resolved_args[index].clone()],
                        cast_func_desc.body.clone(),
                        param_pairs,
                        cast_func_desc.return_type.clone(),
                        cast_func_desc.language.clone(),
                    )
                }
                aiondb_eval::CompatCastMethod::Binary | aiondb_eval::CompatCastMethod::InOut => {
                    compat_cast_expr(
                        resolved_args[index].clone(),
                        actual_type_name,
                        &expected_type_name,
                    )
                }
            };
            continue;
        }

        if !function_arg_type_compatible(actual_type_name, &expected_type_name) {
            return Ok(None);
        }
    }

    let mut anyelement_type: Option<String> = None;
    let mut anyarray_element_type: Option<String> = None;
    let mut anycompatible_type: Option<String> = None;
    let mut anycompatible_range_subtype: Option<String> = None;

    for (index, param) in func_desc.params.iter().enumerate() {
        let expected_type_name = param.raw_type_name.as_deref().map_or_else(
            || compat_type_name_for_data_type(&param.data_type),
            normalize_compat_type_name,
        );
        let actual_type_name = &actual_type_names[index];

        match expected_type_name.as_str() {
            "anyelement" => {
                if index < args.len()
                    && is_untyped_null_expr(&args[index])
                    && anyelement_type.is_none()
                    && anyarray_element_type.is_none()
                {
                    return Ok(None);
                }
                if let Some(bound) = anyelement_type.as_deref() {
                    if bound != actual_type_name {
                        return Ok(None);
                    }
                } else {
                    anyelement_type = Some(actual_type_name.clone());
                }
                if let Some(bound) = anyarray_element_type.as_deref() {
                    if bound != actual_type_name {
                        return Ok(None);
                    }
                }
            }
            "anyarray" => {
                if actual_type_name == "anyarray" {
                    let raw_return_type = func_desc
                        .raw_return_type_name
                        .as_deref()
                        .map(normalize_compat_type_name);
                    if func_desc.language.eq_ignore_ascii_case("sql")
                        && raw_return_type.as_deref() == Some("anyarray")
                    {
                        return Err(DbError::Bind(Box::new(
                            ErrorReport::new(
                                SqlState::FeatureNotSupported,
                                "return type anyarray is not supported for SQL functions",
                            )
                            .with_client_detail(format!(
                                "SQL function \"{}\" during inlining",
                                func_desc.name
                            ))
                            .with_position(args[index].span().start + 1),
                        )));
                    }
                    return Err(DbError::Bind(Box::new(
                        ErrorReport::new(
                            SqlState::DatatypeMismatch,
                            "cannot determine element type of \"anyarray\" argument",
                        )
                        .with_position(args[index].span().start + 1),
                    )));
                }
                if index < args.len()
                    && is_untyped_null_expr(&args[index])
                    && anyarray_element_type.is_none()
                    && anyelement_type.is_none()
                {
                    return Ok(None);
                }
                let Some(element_type) = array_element_type_name(actual_type_name) else {
                    return Ok(None);
                };
                if let Some(bound) = anyarray_element_type.as_deref() {
                    if bound != element_type {
                        return Ok(None);
                    }
                } else {
                    anyarray_element_type = Some(element_type.to_owned());
                }
                if let Some(bound) = anyelement_type.as_deref() {
                    if bound != element_type {
                        return Ok(None);
                    }
                }
            }
            "anynonarray" => {
                if actual_type_name.ends_with("[]") {
                    return Ok(None);
                }
            }
            "anycompatible" | "anycompatiblenonarray" => {
                if index < args.len()
                    && is_untyped_null_expr(&args[index])
                    && anycompatible_type.is_none()
                    && anycompatible_range_subtype.is_none()
                {
                    return Ok(None);
                }
                if expected_type_name == "anycompatiblenonarray" && actual_type_name.ends_with("[]")
                {
                    return Ok(None);
                }
                if let Some(subtype) = anycompatible_range_subtype.as_deref() {
                    if !type_fits_compat_subtype(actual_type_name, subtype) {
                        return Ok(None);
                    }
                }
                let Some(merged) = merge_anycompatible_type(anycompatible_type, actual_type_name)
                else {
                    return Ok(None);
                };
                anycompatible_type = Some(merged);
            }
            "anycompatiblearray" => {
                if index < args.len()
                    && is_untyped_null_expr(&args[index])
                    && anycompatible_type.is_none()
                    && anycompatible_range_subtype.is_none()
                {
                    return Ok(None);
                }
                let Some(element_type) = array_element_type_name(actual_type_name) else {
                    return Ok(None);
                };
                if let Some(subtype) = anycompatible_range_subtype.as_deref() {
                    if !type_fits_compat_subtype(element_type, subtype) {
                        return Ok(None);
                    }
                }
                let Some(merged) = merge_anycompatible_type(anycompatible_type, element_type)
                else {
                    return Ok(None);
                };
                anycompatible_type = Some(merged);
            }
            "anycompatiblerange" => {
                if index < args.len()
                    && is_untyped_null_expr(&args[index])
                    && anycompatible_range_subtype.is_none()
                {
                    return Ok(None);
                }
                let Some(subtype) = range_subtype_type_name(actual_type_name) else {
                    return Ok(None);
                };
                if let Some(bound) = anycompatible_range_subtype.as_deref() {
                    if bound != subtype {
                        return Ok(None);
                    }
                } else {
                    anycompatible_range_subtype = Some(subtype.to_owned());
                }
                let Some(merged) = merge_anycompatible_type(anycompatible_type, subtype) else {
                    return Ok(None);
                };
                anycompatible_type = Some(merged);
            }
            "anycompatiblemultirange" => {
                if index < args.len()
                    && is_untyped_null_expr(&args[index])
                    && anycompatible_range_subtype.is_none()
                {
                    return Ok(None);
                }
                let Some(subtype) = multirange_subtype_type_name(actual_type_name) else {
                    return Ok(None);
                };
                if let Some(bound) = anycompatible_range_subtype.as_deref() {
                    if bound != subtype {
                        return Ok(None);
                    }
                } else {
                    anycompatible_range_subtype = Some(subtype.to_owned());
                }
                let Some(merged) = merge_anycompatible_type(anycompatible_type, subtype) else {
                    return Ok(None);
                };
                anycompatible_type = Some(merged);
            }
            "anyrange" => {
                if index < args.len() && is_untyped_null_expr(&args[index]) {
                    return Ok(None);
                }
                if range_subtype_type_name(actual_type_name).is_none() {
                    return Ok(None);
                }
            }
            "anymultirange" => {
                if index < args.len() && is_untyped_null_expr(&args[index]) {
                    return Ok(None);
                }
                if multirange_subtype_type_name(actual_type_name).is_none() {
                    return Ok(None);
                }
            }
            _ => {}
        }
    }

    Ok(Some(resolved_args))
}

fn is_polymorphic_type_name(type_name: &str) -> bool {
    matches!(
        type_name,
        "any"
            | "anyarray"
            | "anyelement"
            | "anynonarray"
            | "anyenum"
            | "anyrange"
            | "anymultirange"
            | "anycompatible"
            | "anycompatiblearray"
            | "anycompatiblenonarray"
            | "anycompatiblerange"
            | "anycompatiblemultirange"
    )
}

fn function_arg_type_compatible(actual: &str, expected: &str) -> bool {
    if actual == expected {
        return true;
    }
    if matches!(expected, "refcursor") && matches!(actual, "refcursor" | "text") {
        return true;
    }
    if matches!(expected, "oid") && matches!(actual, "oid" | "int4" | "int8") {
        return true;
    }
    if matches!(
        expected,
        "regclass"
            | "regtype"
            | "regproc"
            | "regprocedure"
            | "regoper"
            | "regoperator"
            | "regnamespace"
            | "regrole"
            | "regcollation"
    ) && matches!(actual, "text" | "oid" | "int4" | "int8")
    {
        return true;
    }
    match (
        array_element_type_name(actual),
        array_element_type_name(expected),
    ) {
        (Some(actual_elem), Some(expected_elem)) => {
            return function_arg_type_compatible(actual_elem, expected_elem);
        }
        (Some(_), None) | (None, Some(_)) => return false,
        (None, None) => {}
    }
    numeric_type_rank(actual)
        .zip(numeric_type_rank(expected))
        .is_some_and(|(actual_rank, expected_rank)| actual_rank <= expected_rank)
}

fn numeric_type_rank(type_name: &str) -> Option<u8> {
    match type_name {
        "int2" => Some(0),
        "int4" => Some(1),
        "int8" => Some(2),
        "numeric" => Some(3),
        "float4" => Some(4),
        "float8" => Some(5),
        _ => None,
    }
}

fn merge_anycompatible_type(current: Option<String>, candidate: &str) -> Option<String> {
    let Some(bound) = current else {
        return Some(candidate.to_owned());
    };
    if bound == candidate {
        return Some(bound);
    }
    numeric_type_rank(&bound)
        .zip(numeric_type_rank(candidate))
        .map(|(left, right)| {
            if left >= right {
                bound.clone()
            } else {
                candidate.to_owned()
            }
        })
}

fn array_element_type_name(type_name: &str) -> Option<&str> {
    type_name.strip_suffix("[]")
}

fn range_subtype_type_name(type_name: &str) -> Option<&'static str> {
    match type_name {
        "int4range" => Some("int4"),
        "int8range" => Some("int8"),
        "numrange" => Some("numeric"),
        "float8range" => Some("float8"),
        "daterange" => Some("date"),
        "tsrange" => Some("timestamp"),
        "tstzrange" => Some("timestamptz"),
        _ => None,
    }
}

fn multirange_subtype_type_name(type_name: &str) -> Option<&'static str> {
    match type_name {
        "int4multirange" => Some("int4"),
        "int8multirange" => Some("int8"),
        "nummultirange" => Some("numeric"),
        "float8multirange" => Some("float8"),
        "datemultirange" => Some("date"),
        "tsmultirange" => Some("timestamp"),
        "tstzmultirange" => Some("timestamptz"),
        _ => None,
    }
}

fn type_fits_compat_subtype(actual: &str, subtype: &str) -> bool {
    function_arg_type_compatible(actual, subtype)
}

fn is_untyped_null_expr(expr: &Expr) -> bool {
    matches!(unwrap_type_hint_expr(expr), Expr::Literal(Literal::Null, _))
}

fn is_known_range_type_name(type_name: &str) -> bool {
    matches!(
        type_name,
        "int4range"
            | "int8range"
            | "numrange"
            | "float8range"
            | "daterange"
            | "tsrange"
            | "tstzrange"
    )
}

fn is_known_multirange_type_name(type_name: &str) -> bool {
    matches!(
        type_name,
        "int4multirange"
            | "int8multirange"
            | "nummultirange"
            | "float8multirange"
            | "datemultirange"
            | "tsmultirange"
            | "tstzmultirange"
    )
}

fn pseudo_anyarray_compat_type_name(expr: &Expr) -> Option<String> {
    let Expr::Identifier(identifier) = unwrap_type_hint_expr(expr) else {
        return None;
    };
    let lower = identifier.parts.last()?.to_ascii_lowercase();
    match lower.as_str() {
        "stavalues1" | "stavalues2" | "stavalues3" | "stavalues4" | "stavalues5"
        | "histogram_bounds" => Some("anyarray".to_owned()),
        _ => None,
    }
}
