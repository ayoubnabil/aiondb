use super::*;
use crate::type_check::expr_fn_helpers::unwrap_type_hint_expr;

pub(crate) fn default_column_name(expr: &Expr) -> String {
    match expr {
        Expr::Identifier(name) => name
            .parts
            .last()
            .cloned()
            .unwrap_or_else(|| "?column?".to_owned()),
        Expr::Literal(_, _) => "?column?".to_owned(),
        Expr::Parameter { .. } => "?column?".to_owned(),
        Expr::Default { .. } => "?column?".to_owned(),
        Expr::FunctionCall { name, args, .. } => {
            if name
                .parts
                .last()
                .is_some_and(|part| part.eq_ignore_ascii_case("__aiondb_type_hint"))
            {
                let hinted_type_name = args.get(1).and_then(|expr| match expr {
                    Expr::Literal(Literal::String(type_name), _) => Some(type_name.as_str()),
                    _ => None,
                });
                if let Some(type_name) = hinted_type_name {
                    if let Some(column_name) = hinted_type_column_name(type_name) {
                        return column_name;
                    }
                }
                if let Some(inner) = args.first() {
                    if let Some(src) = cast_source_column_name(inner) {
                        return src;
                    }
                    if let Expr::FunctionCall {
                        args: inner_args, ..
                    } = inner
                    {
                        if let Some(src) = inner_args.first().and_then(cast_source_column_name) {
                            return src;
                        }
                    }
                }
                let inner_name = args
                    .first()
                    .map_or_else(|| "?column?".to_owned(), default_column_name);
                if hinted_type_name.is_some_and(is_range_like_type_hint)
                    && matches!(inner_name.as_str(), "text" | "?column?")
                {
                    return aiondb_eval::normalize_compat_type_name(
                        hinted_type_name.unwrap_or_default(),
                    );
                }
                if inner_name != "?column?" {
                    return inner_name;
                }
                return "?column?".to_owned();
            }
            if name
                .parts
                .last()
                .is_some_and(|part| part.eq_ignore_ascii_case("__aiondb_interval_fields"))
            {
                return args
                    .first()
                    .map_or_else(|| "?column?".to_owned(), default_column_name);
            }
            if name
                .parts
                .last()
                .is_some_and(|part| part.eq_ignore_ascii_case("__aiondb_interval_precision"))
            {
                return args
                    .first()
                    .map_or_else(|| "?column?".to_owned(), default_column_name);
            }
            if name
                .parts
                .last()
                .is_some_and(|part| part.eq_ignore_ascii_case("__aiondb_temporal_precision"))
            {
                return args
                    .first()
                    .map_or_else(|| "?column?".to_owned(), default_column_name);
            }
            if name
                .parts
                .last()
                .is_some_and(|part| part.eq_ignore_ascii_case("__aiondb_char_pad_length"))
            {
                return args
                    .first()
                    .map_or_else(|| "?column?".to_owned(), default_column_name);
            }
            if name.parts.last().is_some_and(|part| {
                part.eq_ignore_ascii_case("__aiondb_array_agg_ordered_desc")
                    || part.eq_ignore_ascii_case("__aiondb_array_agg_ordered_asc")
            }) {
                return "array_agg".to_owned();
            }
            if name.parts.last().is_some_and(|part| {
                part.eq_ignore_ascii_case("__aiondb_jsonb_agg_ordered_desc")
                    || part.eq_ignore_ascii_case("__aiondb_jsonb_agg_ordered_asc")
            }) {
                return "jsonb_agg".to_owned();
            }
            if name.parts.last().is_some_and(|part| {
                part.eq_ignore_ascii_case("__aiondb_json_agg_ordered_desc")
                    || part.eq_ignore_ascii_case("__aiondb_json_agg_ordered_asc")
            }) {
                return "json_agg".to_owned();
            }
            if name
                .parts
                .last()
                .is_some_and(|part| part.eq_ignore_ascii_case("__aiondb_composite_field"))
            {
                if let Some(Expr::Literal(Literal::String(field), _)) = args.get(1) {
                    return field.clone();
                }
            }
            if name
                .parts
                .last()
                .is_some_and(|part| part.eq_ignore_ascii_case("__aiondb_pg_char_cast"))
            {
                return "?column?".to_owned();
            }
            // The parser rewrites the range/multirange operators `&<`, `&>`
            // and `-|-` into nested calls wrapped in `__aiondb_anon_op`.  The
            // wrapper preserves operator semantics while letting the
            // projection use the canonical PostgreSQL `?column?` label
            // (since the user wrote an operator, not a function).
            if name
                .parts
                .last()
                .is_some_and(|part| part.eq_ignore_ascii_case("__aiondb_anon_op"))
            {
                return "?column?".to_owned();
            }
            if name
                .parts
                .last()
                .is_some_and(|part| part.eq_ignore_ascii_case("__aiondb_is_json"))
            {
                return "?column?".to_owned();
            }
            let fname = display_function_name(
                &name
                    .parts
                    .last()
                    .map_or_else(|| "?column?".to_owned(), |s| s.to_ascii_lowercase()),
            );
            if fname == "array_get" || fname == "array_slice" {
                if let Some(first_arg) = args.first() {
                    let base = default_column_name(first_arg);
                    if base != "?column?" {
                        return base;
                    }
                }
            }
            fname
        }
        Expr::UnaryOp { .. } => "?column?".to_owned(),
        Expr::BinaryOp { .. } => "?column?".to_owned(),
        Expr::IsNull { .. } => "?column?".to_owned(),
        Expr::IsDistinctFrom { .. } => "?column?".to_owned(),
        Expr::Like { .. } => "?column?".to_owned(),
        Expr::InList { .. } => "?column?".to_owned(),
        Expr::Between { .. } => "?column?".to_owned(),
        Expr::Cast {
            expr, data_type, ..
        } => cast_source_column_name(unwrap_type_hint_expr(expr))
            .unwrap_or_else(|| pg_type_column_name(data_type)),
        Expr::CaseWhen {
            results,
            else_result,
            ..
        } => {
            if let Some(ref else_expr) = else_result {
                let name = default_column_name(else_expr);
                if name != "?column?" {
                    return name;
                }
            }
            if let Some(first_result) = results.first() {
                let name = default_column_name(first_result);
                if name != "?column?" {
                    return name;
                }
            }
            "case".to_owned()
        }
        Expr::Array { .. } => "array".to_owned(),
        Expr::ArraySubquery { .. } => "array".to_owned(),
        Expr::Subquery { query, .. } => {
            if query.items.len() == 1 {
                let inner = &query.items[0];
                if let Some(ref alias) = inner.alias {
                    alias.clone()
                } else {
                    default_column_name(&inner.expr)
                }
            } else {
                "?column?".to_owned()
            }
        }
        Expr::InSubquery { .. } => "?column?".to_owned(),
        Expr::Exists { .. } => "exists".to_owned(),
        Expr::CypherExists { .. } => "exists".to_owned(),
        Expr::CypherPatternComprehension { .. } => "?column?".to_owned(),
        Expr::WindowFunction { function, .. } => default_column_name(function),
    }
}

fn cast_source_column_name(expr: &Expr) -> Option<String> {
    match expr {
        Expr::Identifier(name) => name.parts.last().cloned(),
        Expr::Cast { expr, .. } => cast_source_column_name(unwrap_type_hint_expr(expr)),
        Expr::FunctionCall { name, args, .. } => {
            if name
                .parts
                .last()
                .is_some_and(|part| part.eq_ignore_ascii_case("__aiondb_type_hint"))
                || name
                    .parts
                    .last()
                    .is_some_and(|part| part.eq_ignore_ascii_case("__aiondb_interval_fields"))
                || name
                    .parts
                    .last()
                    .is_some_and(|part| part.eq_ignore_ascii_case("__aiondb_interval_precision"))
                || name
                    .parts
                    .last()
                    .is_some_and(|part| part.eq_ignore_ascii_case("__aiondb_temporal_precision"))
                || name
                    .parts
                    .last()
                    .is_some_and(|part| part.eq_ignore_ascii_case("__aiondb_char_pad_length"))
            {
                return args.first().and_then(cast_source_column_name);
            }

            if name
                .parts
                .last()
                .is_some_and(|part| part.eq_ignore_ascii_case("__aiondb_composite_field"))
            {
                if let Some(Expr::Literal(Literal::String(field), _)) = args.get(1) {
                    return Some(field.clone());
                }
                return None;
            }
            if name
                .parts
                .last()
                .is_some_and(|part| part.eq_ignore_ascii_case("__aiondb_is_json"))
            {
                return None;
            }

            let fname = display_function_name(
                &name
                    .parts
                    .last()
                    .map_or_else(|| "?column?".to_owned(), |s| s.to_ascii_lowercase()),
            );
            if fname == "array_get" || fname == "array_slice" {
                return args.first().and_then(cast_source_column_name);
            }
            // Internal synthetic wrappers (`__aiondb_*`) are implementation
            // column names - fall through to the caller so the surrounding
            // context (e.g. the outer cast's target type) can supply a name.
            if fname.starts_with("__aiondb_") {
                return None;
            }
            if fname != "?column?" {
                return Some(fname);
            }
            None
        }
        Expr::Array { .. } | Expr::ArraySubquery { .. } => Some("array".to_owned()),
        _ => None,
    }
}

fn hinted_type_column_name(type_name: &str) -> Option<String> {
    match type_name {
        "char" => Some("char".to_owned()),
        "jsonpath" => Some("jsonpath".to_owned()),
        "xid" => Some("xid".to_owned()),
        "xid8" => Some("xid8".to_owned()),
        "pg_snapshot" => Some("pg_snapshot".to_owned()),
        "txid_snapshot" => Some("txid_snapshot".to_owned()),
        type_name
            if aiondb_eval::with_current_session_context(|ctx| {
                ctx.compat_user_type(type_name).is_some()
            }) =>
        {
            Some(aiondb_eval::normalize_compat_type_name(type_name))
        }
        _ => None,
    }
}

fn is_range_like_type_hint(type_name: &str) -> bool {
    let normalized = aiondb_eval::normalize_compat_type_name(type_name);
    normalized.ends_with("range")
}

pub(crate) fn display_function_name(name: &str) -> String {
    match name {
        "__aiondb_variadic_num_nulls" => "num_nulls".to_owned(),
        "__aiondb_variadic_num_nonnulls" => "num_nonnulls".to_owned(),
        "__aiondb_variadic_concat" => "concat".to_owned(),
        "__aiondb_variadic_concat_ws" => "concat_ws".to_owned(),
        "__aiondb_variadic_format" => "format".to_owned(),
        "__aiondb_array_slice" => "array_slice".to_owned(),
        "__aiondb_json_array_subquery" => "json_array".to_owned(),
        other => other.to_owned(),
    }
}

fn pg_type_column_name(dt: &DataType) -> String {
    match dt {
        DataType::Int => "int4".to_owned(),
        DataType::BigInt => "int8".to_owned(),
        DataType::Real => "float4".to_owned(),
        DataType::Double => "float8".to_owned(),
        DataType::Numeric => "numeric".to_owned(),
        DataType::Money => "money".to_owned(),
        DataType::Text => "text".to_owned(),
        DataType::Boolean => "bool".to_owned(),
        DataType::Blob => "bytea".to_owned(),
        DataType::Timestamp => "timestamp".to_owned(),
        DataType::Date => "date".to_owned(),
        DataType::Time => "time".to_owned(),
        DataType::TimeTz => "timetz".to_owned(),
        DataType::Interval => "interval".to_owned(),
        DataType::Tid => "tid".to_owned(),
        DataType::PgLsn => "pg_lsn".to_owned(),
        DataType::Uuid => "uuid".to_owned(),
        DataType::TimestampTz => "timestamptz".to_owned(),
        DataType::Jsonb => "jsonb".to_owned(),
        DataType::MacAddr => "macaddr".to_owned(),
        DataType::MacAddr8 => "macaddr8".to_owned(),
        DataType::Vector {
            dims: 0,
            element_type: aiondb_core::VectorElementType::Float16,
        } => "halfvec".to_owned(),
        DataType::Vector {
            dims,
            element_type: aiondb_core::VectorElementType::Float16,
        } => format!("halfvec({dims})"),
        DataType::Vector {
            dims: 0,
            element_type: aiondb_core::VectorElementType::Float32,
        } => "vector".to_owned(),
        DataType::Vector {
            dims,
            element_type: aiondb_core::VectorElementType::Float32,
        } => format!("vector({dims})"),
        DataType::Vector { dims, element_type } => format!("vector({dims}, {element_type})"),
        DataType::Array(inner) => pg_type_column_name(inner),
    }
}

pub(crate) fn merge_parameter_types(
    index: usize,
    position: usize,
    left: &DataType,
    right: &DataType,
) -> DbResult<DataType> {
    if left == right {
        return Ok(left.clone());
    }

    if matches!(
        (left, right),
        (DataType::Int, DataType::BigInt) | (DataType::BigInt, DataType::Int)
    ) {
        return Ok(DataType::BigInt);
    }

    // PostgreSQL parameter inference allows widening across numeric families.
    // Use a stable common type to avoid spurious "inconsistent type" failures.
    if let (Some(left_rank), Some(right_rank)) =
        (numeric_merge_rank(left), numeric_merge_rank(right))
    {
        let merged = match left_rank.max(right_rank) {
            1 => DataType::Int,
            2 => DataType::BigInt,
            3 => DataType::Real,
            4 => DataType::Double,
            _ => DataType::Numeric,
        };
        return Ok(merged);
    }

    // Some PostgreSQL drivers/ORMs route reused bind names through protocol
    // adapters that leave one occurrence inferred as TEXT while another
    // occurrence is inferred from a concrete numeric context. Mirror PG's
    // "unknown parameter can be coerced later" behavior and keep the concrete
    // numeric type.
    if matches!(left, DataType::Text) && numeric_merge_rank(right).is_some() {
        return Ok(right.clone());
    }
    if matches!(right, DataType::Text) && numeric_merge_rank(left).is_some() {
        return Ok(left.clone());
    }
    if matches!(left, DataType::Text) && matches!(right, DataType::Boolean) {
        return Ok(DataType::Boolean);
    }
    if matches!(right, DataType::Text) && matches!(left, DataType::Boolean) {
        return Ok(DataType::Boolean);
    }

    if let (
        DataType::Vector {
            dims: 0,
            element_type: aiondb_core::VectorElementType::Float32,
        },
        DataType::Vector { .. },
    ) = (left, right)
    {
        return Ok(right.clone());
    }
    if let (
        DataType::Vector { .. },
        DataType::Vector {
            dims: 0,
            element_type: aiondb_core::VectorElementType::Float32,
        },
    ) = (left, right)
    {
        return Ok(left.clone());
    }

    Err(DbError::Bind(Box::new(
        ErrorReport::new(
            SqlState::SyntaxError,
            format!(
                "could not infer consistent type for parameter ${index}: {left:?} vs {right:?}"
            ),
        )
        .with_position(position),
    )))
}

fn numeric_merge_rank(dt: &DataType) -> Option<u8> {
    match dt {
        DataType::Int => Some(1),
        DataType::BigInt => Some(2),
        DataType::Real => Some(3),
        DataType::Double => Some(4),
        DataType::Numeric => Some(5),
        _ => None,
    }
}

enum ParameterWork<'a> {
    Expr(&'a Expr),
    Select(&'a aiondb_parser::SelectStatement),
    Statement(&'a aiondb_parser::Statement),
}

pub(crate) fn expr_contains_parameter(expr: &Expr) -> bool {
    contains_parameter_work([ParameterWork::Expr(expr)])
}

fn contains_parameter_work<'a>(initial: impl IntoIterator<Item = ParameterWork<'a>>) -> bool {
    let mut stack: Vec<ParameterWork<'a>> = initial.into_iter().collect();
    while let Some(work) = stack.pop() {
        match work {
            ParameterWork::Expr(expr) => match expr {
                Expr::Parameter { .. } => return true,
                Expr::Literal(_, _)
                | Expr::Identifier(_)
                | Expr::Default { .. }
                | Expr::CypherExists { .. } => {}
                Expr::CypherPatternComprehension {
                    where_clause,
                    map_expr,
                    ..
                } => {
                    stack.push(ParameterWork::Expr(map_expr));
                    if let Some(where_clause) = where_clause {
                        stack.push(ParameterWork::Expr(where_clause));
                    }
                }
                Expr::FunctionCall { args, filter, .. } => {
                    if let Some(expr) = filter {
                        stack.push(ParameterWork::Expr(expr));
                    }
                    stack.extend(args.iter().map(ParameterWork::Expr));
                }
                Expr::UnaryOp { expr, .. }
                | Expr::IsNull { expr, .. }
                | Expr::Cast { expr, .. } => {
                    stack.push(ParameterWork::Expr(expr));
                }
                Expr::BinaryOp { left, right, .. }
                | Expr::IsDistinctFrom { left, right, .. }
                | Expr::Like {
                    expr: left,
                    pattern: right,
                    ..
                } => {
                    stack.push(ParameterWork::Expr(right));
                    stack.push(ParameterWork::Expr(left));
                }
                Expr::InList { expr, list, .. } => {
                    stack.extend(list.iter().map(ParameterWork::Expr));
                    stack.push(ParameterWork::Expr(expr));
                }
                Expr::Between {
                    expr, low, high, ..
                } => {
                    stack.push(ParameterWork::Expr(high));
                    stack.push(ParameterWork::Expr(low));
                    stack.push(ParameterWork::Expr(expr));
                }
                Expr::CaseWhen {
                    operand,
                    conditions,
                    results,
                    else_result,
                    ..
                } => {
                    if let Some(expr) = else_result {
                        stack.push(ParameterWork::Expr(expr));
                    }
                    stack.extend(results.iter().map(ParameterWork::Expr));
                    stack.extend(conditions.iter().map(ParameterWork::Expr));
                    if let Some(expr) = operand {
                        stack.push(ParameterWork::Expr(expr));
                    }
                }
                Expr::Array { elements, .. } => {
                    stack.extend(elements.iter().map(ParameterWork::Expr));
                }
                Expr::ArraySubquery { query, .. } | Expr::Subquery { query, .. } => {
                    stack.push(ParameterWork::Select(query));
                }
                Expr::InSubquery { expr, query, .. } => {
                    stack.push(ParameterWork::Select(query));
                    stack.push(ParameterWork::Expr(expr));
                }
                Expr::Exists { query, .. } => {
                    stack.push(ParameterWork::Select(query));
                }
                Expr::WindowFunction {
                    function,
                    partition_by,
                    order_by,
                    ..
                } => {
                    for item in order_by {
                        stack.push(ParameterWork::Expr(&item.expr));
                    }
                    stack.extend(partition_by.iter().map(ParameterWork::Expr));
                    stack.push(ParameterWork::Expr(function));
                }
            },
            ParameterWork::Select(select) => push_select_parameter_work(select, &mut stack),
            ParameterWork::Statement(statement) => {
                push_statement_parameter_work(statement, &mut stack);
            }
        }
    }
    false
}

fn push_select_parameter_work<'a>(
    select: &'a aiondb_parser::SelectStatement,
    stack: &mut Vec<ParameterWork<'a>>,
) {
    for cte in &select.ctes {
        stack.push(ParameterWork::Statement(&cte.query));
        if let Some(term) = &cte.recursive_term {
            stack.push(ParameterWork::Select(term));
        }
    }
    for item in &select.items {
        stack.push(ParameterWork::Expr(&item.expr));
    }
    for join in &select.joins {
        if let Some(condition) = &join.condition {
            stack.push(ParameterWork::Expr(condition));
        }
    }
    if let Some(selection) = &select.selection {
        stack.push(ParameterWork::Expr(selection));
    }
    stack.extend(select.group_by.iter().map(ParameterWork::Expr));
    if let Some(having) = &select.having {
        stack.push(ParameterWork::Expr(having));
    }
    for window in &select.window_definitions {
        stack.extend(window.partition_by.iter().map(ParameterWork::Expr));
        for item in &window.order_by {
            stack.push(ParameterWork::Expr(&item.expr));
        }
    }
    for item in &select.order_by {
        stack.push(ParameterWork::Expr(&item.expr));
    }
    if let Some(limit) = &select.limit {
        stack.push(ParameterWork::Expr(limit));
    }
    if let Some(offset) = &select.offset {
        stack.push(ParameterWork::Expr(offset));
    }
    if let aiondb_parser::DistinctKind::DistinctOn(exprs) = &select.distinct {
        stack.extend(exprs.iter().map(ParameterWork::Expr));
    }
}

fn push_statement_parameter_work<'a>(
    statement: &'a aiondb_parser::Statement,
    stack: &mut Vec<ParameterWork<'a>>,
) {
    match statement {
        aiondb_parser::Statement::Select(select) => stack.push(ParameterWork::Select(select)),
        aiondb_parser::Statement::SetOperation(set_op) => {
            stack.push(ParameterWork::Statement(&set_op.right));
            stack.push(ParameterWork::Statement(&set_op.left));
            for item in &set_op.order_by {
                stack.push(ParameterWork::Expr(&item.expr));
            }
            if let Some(limit) = &set_op.limit {
                stack.push(ParameterWork::Expr(limit));
            }
            if let Some(offset) = &set_op.offset {
                stack.push(ParameterWork::Expr(offset));
            }
        }
        _ => {}
    }
}
