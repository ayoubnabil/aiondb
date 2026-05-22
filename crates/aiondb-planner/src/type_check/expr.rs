#![allow(clippy::used_underscore_binding)]

use std::borrow::Cow;
use std::cell::Cell;

use aiondb_plan::ScalarFunction;

use super::expr_fn_helpers::type_hint_name;
use super::expr_functions::infer_function_call;
use super::expr_helpers::{infer_comparison_operands, infer_expr_with_expected};
use super::support::{ambiguous_column, is_numeric, resolve_session_variable};
use super::*;

const MAX_TYPE_CHECK_EXPR_DEPTH: u32 = 32;

thread_local! {
    static TYPE_CHECK_EXPR_DEPTH: Cell<u32> = const { Cell::new(0) };
}

struct TypeCheckExprDepthGuard;

impl TypeCheckExprDepthGuard {
    fn enter() -> DbResult<Self> {
        TYPE_CHECK_EXPR_DEPTH.with(|depth| {
            let next = depth.get().saturating_add(1);
            if next > MAX_TYPE_CHECK_EXPR_DEPTH {
                return Err(DbError::program_limit(format!(
                    "expression type-checking depth exceeds maximum ({MAX_TYPE_CHECK_EXPR_DEPTH})"
                )));
            }
            depth.set(next);
            Ok(Self)
        })
    }
}

impl Drop for TypeCheckExprDepthGuard {
    fn drop(&mut self) {
        TYPE_CHECK_EXPR_DEPTH.with(|depth| depth.set(depth.get().saturating_sub(1)));
    }
}

fn collect_logical_expr_chain<'a>(
    expr: &'a Expr,
    target_op: BinaryOperator,
    out: &mut Vec<&'a Expr>,
) {
    let mut stack = vec![expr];
    while let Some(expr) = stack.pop() {
        if let Expr::BinaryOp {
            left, op, right, ..
        } = expr
        {
            if *op == target_op {
                stack.push(right);
                stack.push(left);
                continue;
            }
        }
        out.push(expr);
    }
}

fn build_balanced_logical_expr(mut typed_parts: Vec<TypedExpr>, op: BinaryOperator) -> TypedExpr {
    debug_assert!(!typed_parts.is_empty());
    while typed_parts.len() > 1 {
        let mut next_level = Vec::with_capacity(typed_parts.len().div_ceil(2));
        let mut iter = typed_parts.into_iter();
        while let Some(left) = iter.next() {
            if let Some(right) = iter.next() {
                next_level.push(match op {
                    BinaryOperator::And => TypedExpr::logical_and(left, right),
                    BinaryOperator::Or => TypedExpr::logical_or(left, right),
                    _ => unreachable!("only logical operators are supported"),
                });
            } else {
                next_level.push(left);
            }
        }
        typed_parts = next_level;
    }
    typed_parts
        .pop()
        .unwrap_or_else(|| TypedExpr::literal(Value::Boolean(true), DataType::Boolean, false))
}

pub(super) fn quantified_array_arg(expr: &Expr) -> Option<(&'static str, &Expr, &Expr)> {
    let Expr::FunctionCall { name, args, .. } = expr else {
        return None;
    };
    if args.len() != 1 {
        return None;
    }
    let function_name = name.parts.last()?;
    if function_name.eq_ignore_ascii_case("any") || function_name.eq_ignore_ascii_case("some") {
        Some(("ANY", expr, &args[0]))
    } else if function_name.eq_ignore_ascii_case("all") {
        Some(("ALL", expr, &args[0]))
    } else {
        None
    }
}

pub(super) fn coerce_quantified_array_literal(
    array: TypedExpr,
    scalar_type: &DataType,
) -> TypedExpr {
    if matches!(array.data_type, DataType::Array(_)) {
        return array;
    }
    if matches!(
        array.kind,
        TypedExprKind::Literal(Value::Text(_) | Value::Null)
    ) {
        return TypedExpr::cast(array, DataType::Array(Box::new(scalar_type.clone())));
    }
    array
}

fn quantified_array_comparison_function_name(
    op: &BinaryOperator,
    quantifier: &str,
) -> Option<String> {
    let known_quantifier = if quantifier.eq_ignore_ascii_case("ANY") {
        Some("any")
    } else if quantifier.eq_ignore_ascii_case("ALL") {
        Some("all")
    } else {
        None
    };

    if let Some(quantifier) = known_quantifier {
        let function_name = match (quantifier, op) {
            ("any", BinaryOperator::Eq) => "__aiondb_quantified_any_eq",
            ("any", BinaryOperator::Ne) => "__aiondb_quantified_any_ne",
            ("any", BinaryOperator::Ge) => "__aiondb_quantified_any_ge",
            ("any", BinaryOperator::Gt) => "__aiondb_quantified_any_gt",
            ("any", BinaryOperator::Le) => "__aiondb_quantified_any_le",
            ("any", BinaryOperator::Lt) => "__aiondb_quantified_any_lt",
            ("any", BinaryOperator::RegexMatch) => "__aiondb_quantified_any_regex_match",
            ("any", BinaryOperator::RegexMatchInsensitive) => {
                "__aiondb_quantified_any_regex_match_ci"
            }
            ("any", BinaryOperator::NotRegexMatch) => "__aiondb_quantified_any_not_regex_match",
            ("any", BinaryOperator::NotRegexMatchInsensitive) => {
                "__aiondb_quantified_any_not_regex_match_ci"
            }
            ("all", BinaryOperator::Eq) => "__aiondb_quantified_all_eq",
            ("all", BinaryOperator::Ne) => "__aiondb_quantified_all_ne",
            ("all", BinaryOperator::Ge) => "__aiondb_quantified_all_ge",
            ("all", BinaryOperator::Gt) => "__aiondb_quantified_all_gt",
            ("all", BinaryOperator::Le) => "__aiondb_quantified_all_le",
            ("all", BinaryOperator::Lt) => "__aiondb_quantified_all_lt",
            ("all", BinaryOperator::RegexMatch) => "__aiondb_quantified_all_regex_match",
            ("all", BinaryOperator::RegexMatchInsensitive) => {
                "__aiondb_quantified_all_regex_match_ci"
            }
            ("all", BinaryOperator::NotRegexMatch) => "__aiondb_quantified_all_not_regex_match",
            ("all", BinaryOperator::NotRegexMatchInsensitive) => {
                "__aiondb_quantified_all_not_regex_match_ci"
            }
            _ => return None,
        };
        return Some(function_name.to_owned());
    }

    let op_name = match op {
        BinaryOperator::Eq => "eq",
        BinaryOperator::Ne => "ne",
        BinaryOperator::Ge => "ge",
        BinaryOperator::Gt => "gt",
        BinaryOperator::Le => "le",
        BinaryOperator::Lt => "lt",
        BinaryOperator::RegexMatch => "regex_match",
        BinaryOperator::RegexMatchInsensitive => "regex_match_ci",
        BinaryOperator::NotRegexMatch => "not_regex_match",
        BinaryOperator::NotRegexMatchInsensitive => "not_regex_match_ci",
        _ => return None,
    };
    Some(format!(
        "__aiondb_quantified_{}_{}",
        quantifier.to_ascii_lowercase(),
        op_name
    ))
}

pub(super) fn quantified_array_bind_error(message: impl Into<String>, position: usize) -> DbError {
    DbError::Bind(Box::new(
        ErrorReport::new(SqlState::SyntaxError, message).with_position(position),
    ))
}

pub(super) fn binary_operator_position(left: &Expr, right: &Expr) -> usize {
    let gap_start = left.span().end;
    let gap_end = right.span().start;
    if gap_end <= gap_start {
        gap_start + 1
    } else {
        usize::midpoint(gap_start, gap_end) + 1
    }
}

pub(super) fn quantified_like_function_name(
    negated: bool,
    case_insensitive: bool,
    quantifier: &str,
) -> String {
    let quantifier = if quantifier.eq_ignore_ascii_case("ANY") {
        Cow::Borrowed("any")
    } else if quantifier.eq_ignore_ascii_case("ALL") {
        Cow::Borrowed("all")
    } else {
        Cow::Owned(quantifier.to_ascii_lowercase())
    };
    let like_name = match (negated, case_insensitive) {
        (false, false) => "like",
        (true, false) => "not_like",
        (false, true) => "ilike",
        (true, true) => "not_ilike",
    };
    if quantifier == "any" {
        return match (negated, case_insensitive) {
            (false, false) => "__aiondb_quantified_any_like".to_owned(),
            (true, false) => "__aiondb_quantified_any_not_like".to_owned(),
            (false, true) => "__aiondb_quantified_any_ilike".to_owned(),
            (true, true) => "__aiondb_quantified_any_not_ilike".to_owned(),
        };
    }
    if quantifier == "all" {
        return match (negated, case_insensitive) {
            (false, false) => "__aiondb_quantified_all_like".to_owned(),
            (true, false) => "__aiondb_quantified_all_not_like".to_owned(),
            (false, true) => "__aiondb_quantified_all_ilike".to_owned(),
            (true, true) => "__aiondb_quantified_all_not_ilike".to_owned(),
        };
    }
    format!("__aiondb_quantified_{quantifier}_{like_name}")
}

fn is_xid_type_hint(expr: &Expr) -> bool {
    type_hint_name(expr).is_some_and(|name| name.eq_ignore_ascii_case("xid"))
}

pub(super) fn infer_quantified_array_comparison(
    op: &BinaryOperator,
    left: &Expr,
    right: &Expr,
    relation: Option<&TableDescriptor>,
    params: &mut ParameterTypes,
    sq: Option<SubqueryResolver<'_>>,
    uf: Option<UserFunctionResolver<'_>>,
) -> DbResult<Option<TypedExpr>> {
    let quantified = quantified_array_arg(right)
        .map(|(quantifier, quantified_expr, array_expr)| {
            (
                left,
                quantifier,
                quantified_expr,
                array_expr,
                binary_operator_position(left, quantified_expr),
            )
        })
        .or_else(|| {
            quantified_array_arg(left).map(|(quantifier, quantified_expr, array_expr)| {
                (
                    right,
                    quantifier,
                    quantified_expr,
                    array_expr,
                    binary_operator_position(quantified_expr, right),
                )
            })
        });

    let Some((scalar_expr, quantifier, _quantified_expr, array_expr, operator_position)) =
        quantified
    else {
        return Ok(None);
    };

    // PG accepts both `expr = ANY(array)` and `expr = ANY(subquery)`. We
    // detect a subquery on the array side and rewrite the eq/ne forms as
    // `expr IN (subquery)` / `expr NOT IN (subquery)` so the existing
    // InSubquery binder handles them. Other comparison ops on subqueries
    // remain unsupported (PG only allows the comparison operator family
    // there too — but the rewrite is more involved than a parser tweak).
    if let Expr::Subquery { query, span } = array_expr {
        let negated = match (op, quantifier) {
            (BinaryOperator::Eq, "ANY") => Some(false),
            (BinaryOperator::Ne, "ALL") => Some(true),
            _ => None,
        };
        if let Some(negated) = negated {
            let rewritten = Expr::InSubquery {
                expr: Box::new(scalar_expr.clone()),
                query: query.clone(),
                negated,
                span: *span,
            };
            return Ok(Some(infer_expr(&rewritten, relation, params, sq, uf)?));
        }
        return Err(quantified_array_bind_error(
            "ANY/ALL on a subquery is only supported with the = (ANY) and <> (ALL) operators",
            operator_position,
        ));
    }

    let scalar = infer_expr(scalar_expr, relation, params, sq, uf)?;
    let scalar_nullable = scalar.nullable;
    let scalar_type = scalar.data_type.clone();
    let expected_array_type = DataType::Array(Box::new(scalar_type.clone()));
    let array = coerce_quantified_array_literal(
        infer_expr_with_expected(
            array_expr,
            relation,
            &expected_array_type,
            true,
            params,
            sq,
            uf,
        )?,
        &scalar_type,
    );
    if !matches!(array.data_type, DataType::Array(_)) {
        return Err(quantified_array_bind_error(
            "op ANY/ALL (array) requires array on right side",
            operator_position,
        ));
    }
    let Some(function_name) = quantified_array_comparison_function_name(op, quantifier) else {
        return Err(quantified_array_bind_error(
            "op ANY/ALL (array) requires operator to yield boolean",
            operator_position,
        ));
    };
    let nullable = scalar_nullable || array.nullable;
    Ok(Some(TypedExpr::scalar_function(
        ScalarFunction::Generic(function_name),
        vec![scalar, array],
        DataType::Boolean,
        nullable,
    )))
}

fn reject_invalid_quantified_array_binary_op(
    op: &BinaryOperator,
    left: &Expr,
    right: &Expr,
    relation: Option<&TableDescriptor>,
    params: &mut ParameterTypes,
    sq: Option<SubqueryResolver<'_>>,
    uf: Option<UserFunctionResolver<'_>>,
) -> DbResult<()> {
    if matches!(
        op,
        BinaryOperator::Eq
            | BinaryOperator::Ne
            | BinaryOperator::Ge
            | BinaryOperator::Gt
            | BinaryOperator::Le
            | BinaryOperator::Lt
            | BinaryOperator::RegexMatch
            | BinaryOperator::RegexMatchInsensitive
            | BinaryOperator::NotRegexMatch
            | BinaryOperator::NotRegexMatchInsensitive
    ) {
        return Ok(());
    }

    let quantified = quantified_array_arg(right)
        .map(|(_, quantified_expr, array_expr)| {
            (
                left,
                array_expr,
                binary_operator_position(left, quantified_expr),
            )
        })
        .or_else(|| {
            quantified_array_arg(left).map(|(_, quantified_expr, array_expr)| {
                (
                    right,
                    array_expr,
                    binary_operator_position(quantified_expr, right),
                )
            })
        });
    let Some((scalar_expr, array_expr, operator_position)) = quantified else {
        return Ok(());
    };

    let scalar = infer_expr(scalar_expr, relation, params, sq, uf)?;
    let expected_array_type = DataType::Array(Box::new(scalar.data_type.clone()));
    let array = coerce_quantified_array_literal(
        infer_expr_with_expected(
            array_expr,
            relation,
            &expected_array_type,
            true,
            params,
            sq,
            uf,
        )?,
        &scalar.data_type,
    );
    if !matches!(array.data_type, DataType::Array(_)) {
        return Err(quantified_array_bind_error(
            "op ANY/ALL (array) requires array on right side",
            operator_position,
        ));
    }
    Err(quantified_array_bind_error(
        "op ANY/ALL (array) requires operator to yield boolean",
        operator_position,
    ))
}

pub(super) fn infer_expr(
    expr: &Expr,
    relation: Option<&TableDescriptor>,
    params: &mut ParameterTypes,
    sq: Option<SubqueryResolver<'_>>,
    uf: Option<UserFunctionResolver<'_>>,
) -> DbResult<TypedExpr> {
    let _depth_guard = TypeCheckExprDepthGuard::enter()?;

    if let Expr::Parameter { index, span } = expr {
        params.mark_seen(*index);
        if let Some(data_type) = params.known(*index) {
            return Ok(TypedExpr::literal(Value::Null, data_type.clone(), true));
        }
        return Err(DbError::Bind(Box::new(
            ErrorReport::new(
                SqlState::SyntaxError,
                format!("could not infer data type of parameter ${index}"),
            )
            .with_position(span.start + 1),
        )));
    }
    if let Expr::BinaryOp {
        left, op, right, ..
    } = expr
    {
        reject_invalid_quantified_array_binary_op(op, left, right, relation, params, sq, uf)?;
    }
    match expr {
        Expr::Literal(Literal::Integer(value), _) => match i32::try_from(*value) {
            Ok(value) => Ok(TypedExpr::literal(Value::Int(value), DataType::Int, false)),
            Err(_) => Ok(TypedExpr::literal(
                Value::BigInt(*value),
                DataType::BigInt,
                false,
            )),
        },
        Expr::Literal(Literal::NumericLit(raw), _span) => {
            if let Ok(nv) = raw.parse::<aiondb_core::NumericValue>() {
                Ok(TypedExpr::literal(
                    Value::Numeric(nv),
                    DataType::Numeric,
                    false,
                ))
            } else if let Ok(f) = raw.parse::<f64>() {
                Ok(TypedExpr::literal(
                    Value::Double(f),
                    DataType::Double,
                    false,
                ))
            } else {
                Ok(TypedExpr::literal(Value::Null, DataType::Numeric, true))
            }
        }
        Expr::Literal(Literal::String(value), _) => Ok(TypedExpr::literal(
            Value::Text(value.clone()),
            DataType::Text,
            false,
        )),
        Expr::Literal(Literal::Boolean(value), _) => Ok(TypedExpr::literal(
            Value::Boolean(*value),
            DataType::Boolean,
            false,
        )),
        Expr::Literal(Literal::Null, _) => {
            Ok(TypedExpr::literal(Value::Null, DataType::Text, true))
        }
        Expr::Parameter { index, span } => {
            // This should have been handled by the early-return above.
            Err(DbError::internal(format!(
                "parameter ${index} reached infer_expr match unexpectedly at position {}",
                span.start + 1
            )))
        }
        Expr::Default { span } => Err(DbError::Bind(Box::new(
            ErrorReport::new(
                SqlState::SyntaxError,
                "DEFAULT is only allowed in INSERT VALUES",
            )
            .with_position(span.start + 1),
        ))),
        Expr::Identifier(name) => {
            let column_name = name.parts.last().map_or("", String::as_str);
            // For qualified references (table.column), build the lookup key
            // used for outer-scope resolution (NUL-separated or plain name).
            let outer_lookup_name: Cow<'_, str> = if name.parts.len() == 2 {
                Cow::Owned(format!("{}\x00{}", name.parts[0], name.parts[1]))
            } else if name.parts.len() == 3 {
                // 3-part: schema.table.column → table\0column
                Cow::Owned(format!("{}\x00{}", name.parts[1], name.parts[2]))
            } else {
                Cow::Borrowed(column_name)
            };
            // PG session variables (current_user, session_user, etc.)
            if name.parts.len() == 1 {
                if let Some(val) = resolve_session_variable(
                    column_name,
                    params.session_context().current_user.as_deref(),
                    params.session_context().session_user.as_deref(),
                    params.session_context().current_schema.as_deref(),
                    params.session_context().current_database.as_deref(),
                ) {
                    return Ok(TypedExpr::literal(val, DataType::Text, false));
                }
            }
            let Some(relation) = relation else {
                // Correlated subquery fallback: check outer scope columns.
                // Try both the qualified name (table\0col) and unqualified name.
                if let Some(outer_col) = params
                    .find_outer_column(outer_lookup_name.as_ref())
                    .or_else(|| params.find_outer_column(column_name))
                {
                    let col_ordinal = usize::try_from(outer_col.ordinal_position.saturating_sub(1))
                        .unwrap_or(usize::MAX);
                    return Ok(TypedExpr::outer_column_ref(
                        outer_col.name.clone(),
                        col_ordinal,
                        outer_col.data_type.clone(),
                        outer_col.nullable,
                    ));
                }
                // SQL-standard bare identifiers (CURRENT_TIMESTAMP, CURRENT_TIME,
                // LOCALTIME, CURRENT_DATE, etc.) are zero-arg functions written
                // without parentheses.  Try resolving as a function call before
                // giving up.
                if name.parts.len() == 1 {
                    if let Ok(typed) =
                        infer_function_call(name, &[], false, None, None, params, name.span, sq, uf)
                    {
                        return Ok(typed);
                    }
                }
                return Err(undefined_column(name.span.start + 1, column_name));
            };
            if name.parts.len() >= 2 {
                let mut qualified_column = None;
                let mut qualified_ordinal = 0_u32;
                let mut qualified_ambiguous = false;
                for candidate in &relation.columns {
                    if candidate
                        .name
                        .eq_ignore_ascii_case(outer_lookup_name.as_ref())
                    {
                        if qualified_column.is_some() {
                            if candidate.ordinal_position != qualified_ordinal {
                                qualified_ambiguous = true;
                                break;
                            }
                        } else {
                            qualified_ordinal = candidate.ordinal_position;
                            qualified_column = Some(candidate);
                        }
                    }
                }
                if qualified_ambiguous {
                    let bare_col = column_name.rsplit('\0').next().unwrap_or(column_name);
                    return Err(ambiguous_column(name.span.start + 1, bare_col));
                }
                if let Some(column) = qualified_column {
                    let col_ordinal = usize::try_from(column.ordinal_position.saturating_sub(1))
                        .unwrap_or(usize::MAX);
                    return Ok(TypedExpr::column_ref(
                        column.name.clone(),
                        col_ordinal,
                        column.data_type.clone(),
                        column.nullable,
                    ));
                }
                // For qualified references like `outer_tbl.col`, do not fall
                // back to a local unqualified `col` match when the qualifier
                // does not resolve in the current relation. That would
                // mis-bind correlated references such as `x.b < t1.b` to
                // `x.b < x.b` when the inner query aliases the same table.
                if let Some(outer_col) = params.find_outer_column(outer_lookup_name.as_ref()) {
                    let col_ordinal = usize::try_from(outer_col.ordinal_position.saturating_sub(1))
                        .unwrap_or(usize::MAX);
                    return Ok(TypedExpr::outer_column_ref(
                        outer_col.name.clone(),
                        col_ordinal,
                        outer_col.data_type.clone(),
                        outer_col.nullable,
                    ));
                }
            }
            let mut column = None;
            let mut first_ordinal = 0_u32;
            let mut ambiguous = false;
            for candidate in &relation.columns {
                if candidate.name.eq_ignore_ascii_case(column_name) {
                    if column.is_some() {
                        if candidate.ordinal_position != first_ordinal {
                            ambiguous = true;
                            break;
                        }
                    } else {
                        first_ordinal = candidate.ordinal_position;
                        column = Some(candidate);
                    }
                }
            }
            let Some(column) = column else {
                // Correlated subquery fallback: check outer scope columns.
                // Try both the qualified name (table\0col) and unqualified name.
                if let Some(outer_col) = params
                    .find_outer_column(outer_lookup_name.as_ref())
                    .or_else(|| params.find_outer_column(column_name))
                {
                    let col_ordinal = usize::try_from(outer_col.ordinal_position.saturating_sub(1))
                        .unwrap_or(usize::MAX);
                    return Ok(TypedExpr::outer_column_ref(
                        outer_col.name.clone(),
                        col_ordinal,
                        outer_col.data_type.clone(),
                        outer_col.nullable,
                    ));
                }
                let bare_col = column_name.rsplit('\0').next().unwrap_or(column_name);
                if is_system_column(bare_col) {
                    if let Some(system_column) = super::expr_cases::compat_system_column(bare_col) {
                        return Ok(system_column);
                    }
                }
                // SQL-standard bare identifiers (CURRENT_TIMESTAMP, CURRENT_TIME,
                // LOCALTIME, CURRENT_DATE, etc.) are zero-arg functions written
                // without parentheses.  Try resolving as a function call before
                // giving up.
                if name.parts.len() == 1 {
                    if let Ok(typed) = infer_function_call(
                        name,
                        &[],
                        false,
                        None,
                        Some(relation),
                        params,
                        name.span,
                        sq,
                        uf,
                    ) {
                        return Ok(typed);
                    }
                }
                // PostgreSQL whole-row references can appear as either a bare
                // alias (`t1`) or a qualified star (`t1.*`) in expression
                // contexts like `excluded.*` inside ON CONFLICT DO UPDATE.
                // Resolve them to a ROW(...) composite built from all columns
                // under that alias, preserving relation order.
                let whole_row_prefix = if name.parts.len() == 1 && !column_name.contains('\0') {
                    Some(column_name)
                } else if name.parts.len() == 2 && name.parts[1] == "*" {
                    Some(name.parts[0].as_str())
                } else {
                    None
                };
                if let Some(prefix) = whole_row_prefix {
                    let mut whole_row_columns: Vec<_> = relation
                        .columns
                        .iter()
                        .filter(|c| {
                            let bare_name = c.name.rsplit('\0').next().unwrap_or(&c.name);
                            c.name
                                .split('\0')
                                .next()
                                .is_some_and(|candidate| candidate.eq_ignore_ascii_case(prefix))
                                && !is_system_column(bare_name)
                        })
                        .collect();
                    if !whole_row_columns.is_empty() {
                        whole_row_columns.sort_by_key(|column| column.ordinal_position);
                        let mut row_args = Vec::with_capacity(whole_row_columns.len());
                        let mut nullable = false;
                        for column in whole_row_columns {
                            let col_ordinal =
                                usize::try_from(column.ordinal_position.saturating_sub(1))
                                    .unwrap_or(usize::MAX);
                            nullable |= column.nullable;
                            row_args.push(TypedExpr::column_ref(
                                column.name.clone(),
                                col_ordinal,
                                column.data_type.clone(),
                                column.nullable,
                            ));
                        }
                        return Ok(TypedExpr::scalar_function(
                            ScalarFunction::Row,
                            row_args,
                            DataType::Text,
                            nullable,
                        ));
                    }
                }
                return Err(undefined_column(name.span.start + 1, column_name));
            };
            if ambiguous {
                let bare_col = column_name.rsplit('\0').next().unwrap_or(column_name);
                return Err(ambiguous_column(name.span.start + 1, bare_col));
            }
            let col_ordinal =
                usize::try_from(column.ordinal_position.saturating_sub(1)).unwrap_or(usize::MAX);
            Ok(TypedExpr::column_ref(
                column.name.clone(),
                col_ordinal,
                column.data_type.clone(),
                column.nullable,
            ))
        }
        Expr::FunctionCall {
            name,
            args,
            distinct: agg_distinct,
            filter: agg_filter,
            span,
        } => infer_function_call(
            name,
            args,
            *agg_distinct,
            agg_filter.as_deref(),
            relation,
            params,
            *span,
            sq,
            uf,
        ),
        Expr::UnaryOp {
            op: UnaryOperator::Not,
            expr,
            ..
        } => Ok(TypedExpr::logical_not(infer_predicate(
            expr, relation, params, sq, uf,
        )?)),
        Expr::BinaryOp {
            left,
            op:
                op @ (BinaryOperator::Eq
                | BinaryOperator::Ne
                | BinaryOperator::Ge
                | BinaryOperator::Gt
                | BinaryOperator::Le
                | BinaryOperator::Lt),
            right,
            ..
        } => {
            if matches!(
                op,
                BinaryOperator::Ge | BinaryOperator::Gt | BinaryOperator::Le | BinaryOperator::Lt
            ) && is_xid_type_hint(left)
                && is_xid_type_hint(right)
            {
                let op_name = match op {
                    BinaryOperator::Ge => ">=",
                    BinaryOperator::Gt => ">",
                    BinaryOperator::Le => "<=",
                    BinaryOperator::Lt => "<",
                    _ => unreachable!(),
                };
                return Err(DbError::Bind(Box::new(
                    ErrorReport::new(
                        SqlState::UndefinedObject,
                        format!("operator does not exist: xid {op_name} xid"),
                    )
                    .with_position(binary_operator_position(left, right))
                    .with_client_hint(
                        "No operator matches the given name and argument types. You might need to add explicit type casts.",
                    ),
                )));
            }
            if let Some(typed) =
                infer_quantified_array_comparison(op, left, right, relation, params, sq, uf)?
            {
                return Ok(typed);
            }
            let (left, right) = infer_comparison_operands(left, right, relation, params, sq, uf)?;
            let left = contextualize_null(left, &right.data_type);
            let right = contextualize_null(right, &left.data_type);
            match op {
                BinaryOperator::Eq => {
                    ensure_comparable_for_eq(&left, &right)?;
                    Ok(TypedExpr::binary_eq(left, right))
                }
                BinaryOperator::Ne => {
                    ensure_comparable_for_eq(&left, &right)?;
                    Ok(TypedExpr::binary_ne(left, right))
                }
                BinaryOperator::Ge => {
                    ensure_orderable_comparison(&left, &right)?;
                    Ok(TypedExpr::binary_ge(left, right))
                }
                BinaryOperator::Gt => {
                    ensure_orderable_comparison(&left, &right)?;
                    Ok(TypedExpr::binary_gt(left, right))
                }
                BinaryOperator::Le => {
                    ensure_orderable_comparison(&left, &right)?;
                    Ok(TypedExpr::binary_le(left, right))
                }
                BinaryOperator::Lt => {
                    ensure_orderable_comparison(&left, &right)?;
                    Ok(TypedExpr::binary_lt(left, right))
                }
                _ => unreachable!("comparison match arm only receives comparison operators"),
            }
        }
        Expr::BinaryOp {
            left,
            op: op @ (BinaryOperator::And | BinaryOperator::Or),
            right,
            ..
        } => {
            let mut parts = Vec::new();
            collect_logical_expr_chain(expr, *op, &mut parts);
            if parts.len() <= 2 {
                let left = infer_predicate(left, relation, params, sq, uf)?;
                let right = infer_predicate(right, relation, params, sq, uf)?;
                return match op {
                    BinaryOperator::And => Ok(TypedExpr::logical_and(left, right)),
                    BinaryOperator::Or => Ok(TypedExpr::logical_or(left, right)),
                    _ => unreachable!("logical match arm only receives AND/OR"),
                };
            }
            let typed_parts: DbResult<Vec<_>> = parts
                .into_iter()
                .map(|part| infer_predicate(part, relation, params, sq, uf))
                .collect();
            Ok(build_balanced_logical_expr(typed_parts?, *op))
        }
        Expr::BinaryOp {
            left,
            op:
                op @ (BinaryOperator::Add
                | BinaryOperator::Sub
                | BinaryOperator::Mul
                | BinaryOperator::Div
                | BinaryOperator::Mod),
            right,
            ..
        } => {
            let (mut left_typed, mut right_typed) = match (left.as_ref(), right.as_ref()) {
                (
                    Expr::Parameter {
                        index: left_index,
                        span: left_span,
                    },
                    Expr::Parameter {
                        index: right_index,
                        span: right_span,
                    },
                ) => {
                    // PREPARE NAME(types) seeds parameter types via hints
                    // - if both placeholders already have known types
                    // (because the user declared them), use those instead
                    // of erroring out. PG accepts `PREPARE p(int,int) AS
                    // SELECT $1+$2`; we'd otherwise reject it.
                    if params.known(*left_index).is_some() && params.known(*right_index).is_some() {
                        let left_typed = infer_expr(left, relation, params, sq, uf)?;
                        let right_typed = infer_expr(right, relation, params, sq, uf)?;
                        (left_typed, right_typed)
                    } else if params.known(*left_index).is_some() {
                        let left_typed = infer_expr(left, relation, params, sq, uf)?;
                        let right_typed = infer_expr_with_expected(
                            right,
                            relation,
                            &left_typed.data_type,
                            true,
                            params,
                            sq,
                            uf,
                        )?;
                        (left_typed, right_typed)
                    } else if params.known(*right_index).is_some() {
                        let right_typed = infer_expr(right, relation, params, sq, uf)?;
                        let left_typed = infer_expr_with_expected(
                            left,
                            relation,
                            &right_typed.data_type,
                            true,
                            params,
                            sq,
                            uf,
                        )?;
                        (left_typed, right_typed)
                    } else {
                        let _ = right_index;
                        let _ = right_span;
                        return Err(DbError::Bind(Box::new(
                            ErrorReport::new(
                                SqlState::SyntaxError,
                                format!("could not infer data type of parameter ${left_index}"),
                            )
                            .with_position(left_span.start + 1),
                        )));
                    }
                }
                (Expr::Parameter { .. }, _) => {
                    let right_typed = infer_expr(right, relation, params, sq, uf)?;
                    let left_typed = infer_expr_with_expected(
                        left,
                        relation,
                        &right_typed.data_type,
                        true,
                        params,
                        sq,
                        uf,
                    )?;
                    (left_typed, right_typed)
                }
                (_, Expr::Parameter { .. }) => {
                    let left_typed = infer_expr(left, relation, params, sq, uf)?;
                    let right_typed = infer_expr_with_expected(
                        right,
                        relation,
                        &left_typed.data_type,
                        true,
                        params,
                        sq,
                        uf,
                    )?;
                    (left_typed, right_typed)
                }
                _ => (
                    infer_expr(left, relation, params, sq, uf)?,
                    infer_expr(right, relation, params, sq, uf)?,
                ),
            };
            let left_is_null_literal = matches!(left.as_ref(), Expr::Literal(Literal::Null, _));
            let right_is_null_literal = matches!(right.as_ref(), Expr::Literal(Literal::Null, _));
            if left_is_null_literal && right_is_null_literal {
                // Preserve SQL NULL propagation for arithmetic on two untyped NULLs.
                left_typed.data_type = DataType::Int;
                right_typed.data_type = DataType::Int;
            } else if left_is_null_literal {
                left_typed.data_type = right_typed.data_type.clone();
            } else if right_is_null_literal {
                right_typed.data_type = left_typed.data_type.clone();
            }
            // JSONB delete operator: jsonb - text | int | text[] -> jsonb
            if matches!(
                expr,
                Expr::BinaryOp {
                    op: BinaryOperator::Sub,
                    ..
                }
            ) && matches!(left_typed.data_type, DataType::Jsonb)
            {
                let right_ok = match &right_typed.data_type {
                    DataType::Text | DataType::Int | DataType::BigInt => true,
                    DataType::Array(elem) => matches!(**elem, DataType::Text),
                    _ => false,
                };
                if right_ok {
                    let nullable = left_typed.nullable || right_typed.nullable;
                    let right_arg = if matches!(right_typed.data_type, DataType::BigInt) {
                        TypedExpr::cast(right_typed, DataType::Int)
                    } else {
                        right_typed
                    };
                    return Ok(TypedExpr::scalar_function(
                        ScalarFunction::Generic("jsonb_delete".into()),
                        vec![left_typed, right_arg],
                        DataType::Jsonb,
                        nullable,
                    ));
                }
            }
            // JSONB path delete: jsonb #- text[] -> jsonb (handled here as Sub
            // when parser maps `#-` to a generic operator; left untouched).
            if matches!(
                expr,
                Expr::BinaryOp {
                    op: BinaryOperator::Sub,
                    ..
                }
            ) && matches!(
                (&left_typed.data_type, &right_typed.data_type),
                (DataType::Date, DataType::TimeTz) | (DataType::TimeTz, DataType::Date)
            ) {
                return Err(DbError::Bind(Box::new(
                    ErrorReport::new(
                        SqlState::SyntaxError,
                        "operator does not exist: date - time with time zone",
                    )
                    .with_position(binary_operator_position(left, right))
                    .with_client_hint(
                        "No operator matches the given name and argument types. You might need to add explicit type casts.",
                    ),
                )));
            }
            if matches!(
                expr,
                Expr::BinaryOp {
                    op: BinaryOperator::Add,
                    ..
                }
            ) && matches!(
                (&left_typed.data_type, &right_typed.data_type),
                (DataType::TimeTz, DataType::TimeTz)
            ) {
                return Err(DbError::Bind(Box::new(
                    ErrorReport::new(
                        SqlState::SyntaxError,
                        "operator does not exist: time with time zone + time with time zone",
                    )
                    .with_position(binary_operator_position(left, right))
                    .with_client_hint(
                        "No operator matches the given name and argument types. You might need to add explicit type casts.",
                    ),
                )));
            }
            let is_money_text_combo = matches!(
                (&left_typed.data_type, &right_typed.data_type),
                (DataType::Money, DataType::Text) | (DataType::Text, DataType::Money)
            );
            // Untyped text **literals** (string constants) are treated as the
            // peer's type via implicit coercion, matching PG behaviour where
            // `'7' + 1` works. Text **columns** (e.g. derived from a previous
            // CTE term) are still rejected because the coercion can't be
            // proven safe at plan time.
            let is_text_literal = |e: &Expr| matches!(e, Expr::Literal(Literal::String(_), _));
            let is_network_text_hint = |e: &Expr| {
                type_hint_name(e).is_some_and(|hint| {
                    hint.eq_ignore_ascii_case("inet") || hint.eq_ignore_ascii_case("cidr")
                })
            };
            let text_is_untyped_literal = (matches!(left_typed.data_type, DataType::Text)
                && is_text_literal(left))
                || (matches!(right_typed.data_type, DataType::Text) && is_text_literal(right));
            let text_is_network_literal = (matches!(left_typed.data_type, DataType::Text)
                && is_network_text_hint(left))
                || (matches!(right_typed.data_type, DataType::Text) && is_network_text_hint(right));
            if !is_money_text_combo
                && !text_is_untyped_literal
                && !text_is_network_literal
                && ((matches!(left_typed.data_type, DataType::Text)
                    && is_numeric(&right_typed.data_type))
                    || (matches!(right_typed.data_type, DataType::Text)
                        && is_numeric(&left_typed.data_type)))
            {
                let op_symbol = match op {
                    BinaryOperator::Add => "+",
                    BinaryOperator::Sub => "-",
                    BinaryOperator::Mul => "*",
                    BinaryOperator::Div => "/",
                    BinaryOperator::Mod => "%",
                    _ => unreachable!("arithmetic match arm only receives arithmetic operators"),
                };
                return Err(DbError::Bind(Box::new(
                    ErrorReport::new(
                        SqlState::UndefinedFunction,
                        format!(
                            "operator does not exist: {} {} {}",
                            left_typed.data_type.pg_type_name(),
                            op_symbol,
                            right_typed.data_type.pg_type_name()
                        ),
                    )
                    .with_position(binary_operator_position(left, right))
                    .with_client_hint(
                        "No operator matches the given name and argument types. You might need to add explicit type casts.",
                    ),
                )));
            }
            if matches!(
                (&left_typed.data_type, &right_typed.data_type),
                (DataType::Vector { .. }, DataType::Vector { .. })
            ) && !matches!(
                op,
                BinaryOperator::Add | BinaryOperator::Sub | BinaryOperator::Mul
            ) {
                let op_symbol = match op {
                    BinaryOperator::Div => "/",
                    BinaryOperator::Mod => "%",
                    _ => unreachable!("arithmetic match arm only receives arithmetic operators"),
                };
                return Err(DbError::Bind(Box::new(
                    ErrorReport::new(
                        SqlState::UndefinedFunction,
                        format!(
                            "operator does not exist: {} {} {}",
                            left_typed.data_type.pg_type_name(),
                            op_symbol,
                            right_typed.data_type.pg_type_name()
                        ),
                    )
                    .with_position(binary_operator_position(left, right))
                    .with_client_hint(
                        "No operator matches the given name and argument types. You might need to add explicit type casts.",
                    ),
                )));
            }
            let result_type = if text_is_network_literal
                && ((matches!(left_typed.data_type, DataType::Text)
                    && is_numeric(&right_typed.data_type))
                    || (matches!(right_typed.data_type, DataType::Text)
                        && is_numeric(&left_typed.data_type)))
            {
                DataType::Text
            } else if matches!(
                (&left_typed.data_type, &right_typed.data_type),
                (DataType::Money, DataType::Money)
            ) && matches!(
                expr,
                Expr::BinaryOp {
                    op: BinaryOperator::Div,
                    ..
                }
            ) {
                DataType::Double
            } else {
                resolve_arithmetic_type(&left_typed.data_type, &right_typed.data_type)?
            };
            let nullable = left_typed.nullable || right_typed.nullable;
            match expr {
                Expr::BinaryOp {
                    op: BinaryOperator::Add,
                    ..
                } => Ok(TypedExpr::arith_add(
                    left_typed,
                    right_typed,
                    result_type,
                    nullable,
                )),
                Expr::BinaryOp {
                    op: BinaryOperator::Sub,
                    ..
                } => Ok(TypedExpr::arith_sub(
                    left_typed,
                    right_typed,
                    result_type,
                    nullable,
                )),
                Expr::BinaryOp {
                    op: BinaryOperator::Mul,
                    ..
                } => Ok(TypedExpr::arith_mul(
                    left_typed,
                    right_typed,
                    result_type,
                    nullable,
                )),
                Expr::BinaryOp {
                    op: BinaryOperator::Div,
                    ..
                } => Ok(TypedExpr::arith_div(
                    left_typed,
                    right_typed,
                    result_type,
                    nullable,
                )),
                Expr::BinaryOp {
                    op: BinaryOperator::Mod,
                    ..
                } => Ok(TypedExpr::arith_mod(
                    left_typed,
                    right_typed,
                    result_type,
                    nullable,
                )),
                _ => Err(DbError::internal(format!(
                    "unsupported arithmetic operator during type inference: {expr:?}"
                ))),
            }
        }
        Expr::BinaryOp {
            left,
            op: BinaryOperator::Concat,
            right,
            span: _span,
        } => {
            let left_typed = infer_expr(left, relation, params, sq, uf)?;
            let right_typed = infer_expr(right, relation, params, sq, uf)?;
            if matches!(left_typed.data_type, DataType::Vector { .. })
                && matches!(right_typed.data_type, DataType::Vector { .. })
            {
                let result_type =
                    resolve_vector_result_type(&left_typed.data_type, &right_typed.data_type, true);
                return Ok(TypedExpr::concat_typed(
                    left_typed,
                    right_typed,
                    result_type,
                ));
            }
            // Array concatenation: arr || arr, arr || elem, elem || arr
            let left_is_array = matches!(left_typed.data_type, DataType::Array(_));
            let right_is_array = matches!(right_typed.data_type, DataType::Array(_));
            if left_is_array || right_is_array {
                let result_type = if left_is_array {
                    left_typed.data_type.clone()
                } else {
                    right_typed.data_type.clone()
                };
                return Ok(TypedExpr::array_concat(
                    left_typed,
                    right_typed,
                    result_type,
                ));
            }
            // JSONB concatenation: jsonb || jsonb, jsonb || text, text || jsonb
            let left_is_jsonb = matches!(left_typed.data_type, DataType::Jsonb);
            let right_is_jsonb = matches!(right_typed.data_type, DataType::Jsonb);
            if left_is_jsonb || right_is_jsonb {
                return Ok(TypedExpr::concat_typed(
                    left_typed,
                    right_typed,
                    DataType::Jsonb,
                ));
            }
            // BYTEA concatenation: bytea || bytea
            if matches!(left_typed.data_type, DataType::Blob)
                && matches!(right_typed.data_type, DataType::Blob)
            {
                return Ok(TypedExpr::concat_typed(
                    left_typed,
                    right_typed,
                    DataType::Blob,
                ));
            }
            // PostgreSQL implicit cast: when one operand is TEXT and the
            // other is a non-TEXT scalar, insert an implicit cast to TEXT.
            // When neither operand is TEXT (e.g. `1 || 2`), it's an error.
            let left_is_text = matches!(left_typed.data_type, DataType::Text);
            let right_is_text = matches!(right_typed.data_type, DataType::Text);
            if !left_is_text && !right_is_text {
                return Err(DbError::Bind(Box::new(
                    ErrorReport::new(
                        SqlState::UndefinedObject,
                        format!(
                            "operator does not exist: {} || {}",
                            left_typed.data_type.pg_type_name(),
                            right_typed.data_type.pg_type_name()
                        ),
                    )
                    .with_client_hint(
                        "No operator matches the given name and argument types. You might need to add explicit type casts.",
                    )
                    .with_position(binary_operator_position(left, right).saturating_sub(1)),
                )));
            }
            let left_typed = if left_is_text {
                left_typed
            } else {
                TypedExpr::cast(left_typed, DataType::Text)
            };
            let right_typed = if right_is_text {
                right_typed
            } else {
                TypedExpr::cast(right_typed, DataType::Text)
            };
            Ok(TypedExpr::concat(left_typed, right_typed))
        }
        Expr::BinaryOp {
            left,
            op: BinaryOperator::JsonGet,
            right,
            ..
        } => {
            let left_typed = infer_expr(left, relation, params, sq, uf)?;
            let right_typed = infer_expr(right, relation, params, sq, uf)?;
            Ok(TypedExpr::json_get(left_typed, right_typed))
        }
        Expr::BinaryOp {
            left,
            op: BinaryOperator::JsonGetText,
            right,
            ..
        } => {
            let left_typed = infer_expr(left, relation, params, sq, uf)?;
            let right_typed = infer_expr(right, relation, params, sq, uf)?;
            Ok(TypedExpr::json_get_text(left_typed, right_typed))
        }
        Expr::BinaryOp {
            left,
            op: BinaryOperator::JsonPathGet,
            right,
            ..
        } => {
            let left_typed = infer_expr(left, relation, params, sq, uf)?;
            let right_typed = infer_expr(right, relation, params, sq, uf)?;
            Ok(TypedExpr::json_path_get(left_typed, right_typed))
        }
        Expr::BinaryOp {
            left,
            op: BinaryOperator::JsonPathGetText,
            right,
            ..
        } => {
            let left_typed = infer_expr(left, relation, params, sq, uf)?;
            let right_typed = infer_expr(right, relation, params, sq, uf)?;
            Ok(TypedExpr::json_path_get_text(left_typed, right_typed))
        }
        Expr::BinaryOp {
            left,
            op: BinaryOperator::JsonContains,
            right,
            ..
        } => {
            let left_typed = infer_expr(left, relation, params, sq, uf)?;
            let right_typed = infer_expr(right, relation, params, sq, uf)?;
            if matches!(left_typed.data_type, DataType::Array(_))
                || matches!(right_typed.data_type, DataType::Array(_))
            {
                Ok(TypedExpr::array_contains(left_typed, right_typed))
            } else {
                Ok(TypedExpr::json_contains(left_typed, right_typed))
            }
        }
        Expr::BinaryOp {
            left,
            op: BinaryOperator::JsonContainedBy,
            right,
            ..
        } => {
            let left_typed = infer_expr(left, relation, params, sq, uf)?;
            let right_typed = infer_expr(right, relation, params, sq, uf)?;
            if matches!(left_typed.data_type, DataType::Array(_))
                || matches!(right_typed.data_type, DataType::Array(_))
            {
                Ok(TypedExpr::array_contained_by(left_typed, right_typed))
            } else {
                Ok(TypedExpr::json_contained_by(left_typed, right_typed))
            }
        }
        Expr::BinaryOp {
            left,
            op: BinaryOperator::JsonKeyExists,
            right,
            ..
        } => {
            let left_typed = infer_expr(left, relation, params, sq, uf)?;
            let right_typed = infer_expr(right, relation, params, sq, uf)?;
            Ok(TypedExpr::json_key_exists(left_typed, right_typed))
        }
        Expr::BinaryOp {
            left,
            op: BinaryOperator::JsonAnyKeyExists,
            right,
            ..
        } => {
            let left_typed = infer_expr(left, relation, params, sq, uf)?;
            let right_typed = infer_expr(right, relation, params, sq, uf)?;
            Ok(TypedExpr::json_any_key_exists(left_typed, right_typed))
        }
        Expr::BinaryOp {
            left,
            op: BinaryOperator::JsonAllKeysExist,
            right,
            ..
        } => {
            let left_typed = infer_expr(left, relation, params, sq, uf)?;
            let right_typed = infer_expr(right, relation, params, sq, uf)?;
            Ok(TypedExpr::json_all_keys_exist(left_typed, right_typed))
        }
        Expr::BinaryOp {
            left,
            op: BinaryOperator::ArrayOverlap,
            right,
            ..
        } => {
            let left_typed = infer_expr(left, relation, params, sq, uf)?;
            let right_typed = infer_expr(right, relation, params, sq, uf)?;
            Ok(TypedExpr::array_overlap(left_typed, right_typed))
        }
        Expr::BinaryOp {
            left,
            op: BinaryOperator::Exp,
            right,
            span,
        } => super::expr_cases::infer_special_binary_operator(
            &BinaryOperator::Exp,
            left,
            right,
            relation,
            params,
            sq,
            uf,
            span.start + 1,
        ),
        Expr::BinaryOp {
            left,
            op: BinaryOperator::BitwiseAnd,
            right,
            span,
        } => super::expr_cases::infer_special_binary_operator(
            &BinaryOperator::BitwiseAnd,
            left,
            right,
            relation,
            params,
            sq,
            uf,
            span.start + 1,
        ),
        Expr::BinaryOp {
            left,
            op: BinaryOperator::BitwiseOr,
            right,
            span,
        } => super::expr_cases::infer_special_binary_operator(
            &BinaryOperator::BitwiseOr,
            left,
            right,
            relation,
            params,
            sq,
            uf,
            span.start + 1,
        ),
        Expr::BinaryOp {
            left,
            op: BinaryOperator::BitwiseXor,
            right,
            span,
        } => super::expr_cases::infer_special_binary_operator(
            &BinaryOperator::BitwiseXor,
            left,
            right,
            relation,
            params,
            sq,
            uf,
            span.start + 1,
        ),
        Expr::BinaryOp {
            left,
            op: BinaryOperator::ShiftLeft,
            right,
            span,
        } => super::expr_cases::infer_special_binary_operator(
            &BinaryOperator::ShiftLeft,
            left,
            right,
            relation,
            params,
            sq,
            uf,
            span.start + 1,
        ),
        Expr::BinaryOp {
            left,
            op: BinaryOperator::ShiftRight,
            right,
            span,
        } => super::expr_cases::infer_special_binary_operator(
            &BinaryOperator::ShiftRight,
            left,
            right,
            relation,
            params,
            sq,
            uf,
            span.start + 1,
        ),
        Expr::BinaryOp {
            left,
            op: BinaryOperator::RegexMatch,
            right,
            span,
        } => super::expr_cases::infer_regex_binary_operator(
            &BinaryOperator::RegexMatch,
            left,
            right,
            relation,
            params,
            sq,
            uf,
            span.start + 1,
        ),
        Expr::BinaryOp {
            left,
            op: BinaryOperator::RegexMatchInsensitive,
            right,
            span,
        } => super::expr_cases::infer_regex_binary_operator(
            &BinaryOperator::RegexMatchInsensitive,
            left,
            right,
            relation,
            params,
            sq,
            uf,
            span.start + 1,
        ),
        Expr::BinaryOp {
            left,
            op: BinaryOperator::NotRegexMatch,
            right,
            span,
        } => super::expr_cases::infer_regex_binary_operator(
            &BinaryOperator::NotRegexMatch,
            left,
            right,
            relation,
            params,
            sq,
            uf,
            span.start + 1,
        ),
        Expr::BinaryOp {
            left,
            op: BinaryOperator::NotRegexMatchInsensitive,
            right,
            span,
        } => super::expr_cases::infer_regex_binary_operator(
            &BinaryOperator::NotRegexMatchInsensitive,
            left,
            right,
            relation,
            params,
            sq,
            uf,
            span.start + 1,
        ),
        Expr::BinaryOp {
            left,
            op: BinaryOperator::FullTextSearch,
            right,
            ..
        } => super::expr_cases::infer_full_text_search_operator(
            left, right, relation, params, sq, uf,
        ),
        Expr::BinaryOp {
            left,
            op: BinaryOperator::JsonPathExists,
            right,
            ..
        } => {
            super::expr_cases::infer_jsonpath_exists_operator(left, right, relation, params, sq, uf)
        }
        Expr::BinaryOp {
            left,
            op: BinaryOperator::GeometricEq,
            right,
            ..
        } => super::expr_cases::infer_geometric_eq_operator(left, right, relation, params, sq, uf),
        Expr::BinaryOp {
            left,
            op: BinaryOperator::VectorL2Distance,
            right,
            ..
        } => super::expr_cases::infer_pgvector_distance_operator(
            left,
            right,
            aiondb_plan::ScalarFunction::L2Distance,
            relation,
            params,
            sq,
            uf,
        ),
        Expr::BinaryOp {
            left,
            op: BinaryOperator::VectorCosineDistance,
            right,
            ..
        } => super::expr_cases::infer_pgvector_distance_operator(
            left,
            right,
            aiondb_plan::ScalarFunction::CosineDistance,
            relation,
            params,
            sq,
            uf,
        ),
        Expr::BinaryOp {
            left,
            op: BinaryOperator::VectorNegativeInnerProduct,
            right,
            ..
        } => super::expr_cases::infer_pgvector_distance_operator(
            left,
            right,
            // `<#>` is pgvector's negative inner product: smaller = closer,
            // so `ORDER BY v <#> q ASC LIMIT k` returns the max-dot-product
            // neighbours. The HNSW storage `InnerProduct` metric uses the
            // same negated sign internally.
            aiondb_plan::ScalarFunction::NegativeInnerProduct,
            relation,
            params,
            sq,
            uf,
        ),
        Expr::BinaryOp {
            left,
            op: BinaryOperator::VectorL1Distance,
            right,
            ..
        } => super::expr_cases::infer_pgvector_distance_operator(
            left,
            right,
            aiondb_plan::ScalarFunction::ManhattanDistance,
            relation,
            params,
            sq,
            uf,
        ),
        Expr::BinaryOp {
            left,
            op: BinaryOperator::VectorHammingDistance,
            right,
            ..
        } => super::expr_cases::infer_pgvector_bit_distance_operator(
            left,
            right,
            aiondb_plan::ScalarFunction::HammingDistance,
            relation,
            params,
            sq,
            uf,
        ),
        Expr::BinaryOp {
            left,
            op: BinaryOperator::VectorJaccardDistance,
            right,
            ..
        } => super::expr_cases::infer_pgvector_bit_distance_operator(
            left,
            right,
            aiondb_plan::ScalarFunction::JaccardDistance,
            relation,
            params,
            sq,
            uf,
        ),
        Expr::UnaryOp {
            op: UnaryOperator::BitwiseNot,
            expr: inner,
            span,
        } => super::expr_cases::infer_bitwise_not(inner, relation, params, sq, uf, span.start + 1),
        Expr::UnaryOp {
            op: UnaryOperator::Abs,
            expr: inner,
            span,
        } => super::expr_cases::infer_abs(inner, relation, params, sq, uf, span.start + 1),
        Expr::UnaryOp {
            op: UnaryOperator::SquareRoot,
            expr: inner,
            ..
        } => super::expr_cases::infer_square_root(inner, relation, params, sq, uf),
        Expr::UnaryOp {
            op: UnaryOperator::CubeRoot,
            expr: inner,
            ..
        } => super::expr_cases::infer_cube_root(inner, relation, params, sq, uf),
        Expr::UnaryOp {
            op: UnaryOperator::Minus,
            expr: inner,
            span,
        } => super::expr_cases::infer_negate(inner, relation, params, sq, uf, span.start + 1),
        Expr::IsNull {
            expr: inner,
            negated,
            ..
        } => super::expr_cases::infer_is_null(inner, *negated, relation, params, sq, uf),
        Expr::IsDistinctFrom {
            left,
            right,
            negated,
            ..
        } => super::expr_cases::infer_is_distinct_from(
            left, right, *negated, relation, params, sq, uf,
        ),
        Expr::Like {
            expr: inner,
            pattern,
            negated,
            case_insensitive,
            span,
        } => super::expr_cases::infer_like(
            inner,
            pattern,
            *negated,
            *case_insensitive,
            relation,
            params,
            sq,
            uf,
            span.start + 1,
        ),
        Expr::InList {
            expr: inner,
            list,
            negated,
            ..
        } => super::expr_cases::infer_in_list(inner, list, *negated, relation, params, sq, uf),
        Expr::Between {
            expr: inner,
            low,
            high,
            negated,
            ..
        } => super::expr_cases::infer_between(inner, low, high, *negated, relation, params, sq, uf),
        Expr::Cast {
            expr: inner,
            data_type: target_type,
            span,
        } => super::expr_cases::infer_cast(
            inner,
            target_type,
            relation,
            params,
            sq,
            uf,
            span.start + 1,
        ),
        Expr::CaseWhen {
            operand,
            conditions,
            results,
            else_result,
            ..
        } => super::expr_cases::infer_case_when(
            operand.as_deref(),
            conditions,
            results,
            else_result.as_deref(),
            relation,
            params,
            sq,
            uf,
        ),
        Expr::Array { elements, span } => super::expr_cases::infer_array_construct(
            elements,
            relation,
            params,
            sq,
            uf,
            span.start + 1,
        ),
        Expr::ArraySubquery { query, span } => {
            super::expr_cases::infer_array_subquery(query, relation, params, sq, span.start + 1)
        }
        Expr::Subquery { query, span } => {
            super::expr_cases::infer_scalar_subquery(query, relation, params, sq, span.start + 1)
        }
        Expr::InSubquery {
            expr: inner,
            query,
            negated,
            span,
        } => super::expr_cases::infer_in_subquery(
            inner,
            query,
            *negated,
            relation,
            params,
            sq,
            uf,
            span.start + 1,
        ),
        Expr::Exists {
            query,
            span: _,
            negated,
        } => super::expr_cases::infer_exists(query, *negated, params, sq),
        Expr::CypherExists { .. } => Err(DbError::feature_not_supported(
            "Cypher EXISTS subqueries are only supported inside Cypher expressions",
        )),
        Expr::CypherPatternComprehension { .. } => Err(DbError::feature_not_supported(
            "Cypher pattern comprehensions are only supported inside Cypher expressions",
        )),
        Expr::WindowFunction {
            function,
            partition_by,
            order_by,
            span,
            ..
        } => super::window::infer_window_function(
            function,
            partition_by,
            order_by,
            relation,
            params,
            sq,
            uf,
            *span,
        )
        .or_else(|_| Ok(TypedExpr::literal(Value::Null, DataType::Text, true))),
    }
}

// Re-export from expr_cases so that sibling modules can still use this path.
pub(super) use super::expr_cases::domain_base_type_to_data_type;
