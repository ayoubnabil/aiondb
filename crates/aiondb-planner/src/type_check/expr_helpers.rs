#![allow(clippy::doc_markdown)]

use super::*;

use super::expr::infer_expr;

/// Pick a non-NULL placeholder value of the given type for an unbound
/// parameter during planning.
///
/// The optimizer's access-path selection treats `column = NULL` as
/// always-false and skips IndexEq for it. That's the right call for a
/// user-written `WHERE id = NULL`, but it also kicked in for
/// parameterized statements where the parameter happens to be unbound
/// at plan time - every prepared `WHERE id = $1` was planning to
/// SeqScan and only ever rewriting the literal at execute time, never
/// upgrading the access path. Returning a typed default value here
/// keeps the cached plan IndexEq-shaped; the rewrite then substitutes
/// the actual bound value (including NULL, which the storage layer
/// then correctly returns no rows for).
pub(crate) fn parameter_placeholder_value(data_type: &DataType) -> Value {
    match data_type {
        DataType::Int => Value::Int(0),
        DataType::BigInt => Value::BigInt(0),
        DataType::Boolean => Value::Boolean(false),
        DataType::Real => Value::Real(0.0),
        DataType::Double => Value::Double(0.0),
        DataType::Text => Value::Text(String::new()),
        DataType::Money => Value::Money(0),
        // Fall back to Null for types where there is no obvious cheap
        // default available without depending on the `time` crate from
        // this crate (Date, Time, Timestamp, Interval) or where there
        // is no canonical zero (Numeric, Vector, Array, Jsonb, Uuid,
        // Tid, PgLsn, MacAddr, Blob, …). Parameterized predicates
        // over those types still fall back to SeqScan, which mirrors
        // the pre-fix behaviour and is acceptable until those columns
        // become a measurable hot path. The OLTP B-tree-on-int(eger)
        // case - by far the dominant pgwire-prepared workload - is
        // the one that needs the placeholder.
        _ => Value::Null,
    }
}

fn normalize_projection_expr_for_join_aliases(
    expr: &Expr,
    relation: Option<&TableDescriptor>,
) -> Expr {
    let has_internal_aliases = relation.is_some_and(|relation| {
        relation
            .columns
            .iter()
            .any(|column| column.name.contains('\x00'))
    });
    if has_internal_aliases {
        rewrite_table_aliases(expr)
    } else {
        expr.clone()
    }
}

pub(super) fn infer_expr_with_expected(
    expr: &Expr,
    relation: Option<&TableDescriptor>,
    expected_type: &DataType,
    nullable: bool,
    params: &mut ParameterTypes,
    sq: Option<SubqueryResolver<'_>>,
    uf: Option<UserFunctionResolver<'_>>,
) -> DbResult<TypedExpr> {
    match expr {
        Expr::Array { elements, .. }
            if elements.is_empty() && matches!(expected_type, DataType::Array(_)) =>
        {
            Ok(TypedExpr::literal(
                Value::Array(Vec::new()),
                expected_type.clone(),
                false,
            ))
        }
        Expr::Parameter { index, span } => {
            let data_type = params.infer(*index, span.start + 1, expected_type)?;
            // Use a typed-default placeholder rather than `Value::Null`. The
            // optimizer rejects `column = NULL` predicates as unindexable
            // (since `id = NULL` is logically false), so a Null placeholder
            // for an unbound parameter forced every parameterized SELECT
            // through SeqScan even when the column had a unique B-tree
            // index. The rewrite step at execute time substitutes the real
            // bound value and re-checks the access path, so the placeholder
            // is purely a planning hint - using the type-appropriate
            // default lets the optimizer pick `IndexEq` cleanly.
            let placeholder = parameter_placeholder_value(&data_type);
            Ok(TypedExpr::literal(placeholder, data_type, nullable))
        }
        Expr::Literal(Literal::String(value), span) if !matches!(expected_type, DataType::Text) => {
            aiondb_eval::coercions::coerce_value(Value::Text(value.clone()), expected_type)
                .map_err(|err| {
                    if err.report().position.is_some() {
                        err
                    } else {
                        err.with_position(span.start + 1)
                    }
                })?;
            Ok(TypedExpr::cast(
                infer_expr(expr, relation, params, sq, uf)?,
                expected_type.clone(),
            ))
        }
        _ => Ok(contextualize_null(
            infer_expr(expr, relation, params, sq, uf)?,
            expected_type,
        )),
    }
}

pub(super) fn infer_predicate(
    expr: &Expr,
    relation: Option<&TableDescriptor>,
    params: &mut ParameterTypes,
    sq: Option<SubqueryResolver<'_>>,
    uf: Option<UserFunctionResolver<'_>>,
) -> DbResult<TypedExpr> {
    let expr = infer_expr_with_expected(expr, relation, &DataType::Boolean, true, params, sq, uf)?;
    if expr.data_type == DataType::Boolean {
        Ok(expr)
    } else {
        Err(DbError::Bind(Box::new(ErrorReport::new(
            SqlState::DatatypeMismatch,
            format!(
                "argument of WHERE must be type boolean, not type {}",
                expr.data_type.pg_type_name()
            ),
        ))))
    }
}

pub(super) fn infer_order_by_expr(
    expr: &Expr,
    projections: &[BoundProjection],
    relation: Option<&TableDescriptor>,
    params: &mut ParameterTypes,
    sq: Option<SubqueryResolver<'_>>,
    uf: Option<UserFunctionResolver<'_>>,
) -> DbResult<TypedExpr> {
    // Positional reference: ORDER BY <integer> means the Nth projection (1-based).
    if let Some(resolved) =
        resolve_positional_ref(expr, "ORDER BY", projections, relation, params, sq, uf)?
    {
        ensure_orderable_sort_expr(&resolved)?;
        return Ok(resolved);
    }

    if let Expr::Identifier(name) = expr {
        if name.parts.len() == 1 {
            let alias = &name.parts[0];
            if let Some(projection) = projections
                .iter()
                .find(|projection| projection.alias.as_deref() == Some(alias.as_str()))
            {
                let normalized =
                    normalize_projection_expr_for_join_aliases(&projection.expr, relation);
                let expr = infer_expr(&normalized, relation, params, sq, uf)?;
                ensure_orderable_sort_expr(&expr)?;
                return Ok(expr);
            }

            // PostgreSQL also resolves ORDER BY names against output column
            // names (including implicit names from SELECT list expressions).
            let mut matching_projection_exprs = projections
                .iter()
                .filter_map(|projection| {
                    let output_name = projection
                        .alias
                        .clone()
                        .unwrap_or_else(|| default_column_name(&projection.expr));
                    output_name
                        .eq_ignore_ascii_case(alias)
                        .then_some(projection.expr.clone())
                })
                .collect::<Vec<_>>();
            if matching_projection_exprs.len() > 1 {
                return Err(DbError::bind_error(
                    SqlState::SyntaxError,
                    format!("column reference \"{alias}\" is ambiguous"),
                )
                .with_position(name.span.start + 1));
            }
            if let Some(projection_expr) = matching_projection_exprs.pop() {
                let normalized =
                    normalize_projection_expr_for_join_aliases(&projection_expr, relation);
                let expr = infer_expr(&normalized, relation, params, sq, uf)?;
                ensure_orderable_sort_expr(&expr)?;
                return Ok(expr);
            }
        }
    }

    let expr = infer_expr(expr, relation, params, sq, uf)?;
    ensure_orderable_sort_expr(&expr)?;
    Ok(expr)
}

/// Resolve a positional reference (integer literal) in ORDER BY or GROUP BY
/// to the corresponding projection expression (1-based index).
///
/// PostgreSQL behavior:
///   - Position 0 or negative: ERROR
///   - Position > select list length: ERROR
///   - Otherwise: resolve to the Nth projection expression
pub(super) fn resolve_positional_ref(
    expr: &Expr,
    clause_name: &str,
    projections: &[BoundProjection],
    relation: Option<&TableDescriptor>,
    params: &mut ParameterTypes,
    sq: Option<SubqueryResolver<'_>>,
    uf: Option<UserFunctionResolver<'_>>,
) -> DbResult<Option<TypedExpr>> {
    if let Expr::Literal(Literal::Integer(n), span) = expr {
        let pos = *n;
        if pos <= 0 {
            return Err(DbError::Bind(Box::new(
                ErrorReport::new(
                    SqlState::SyntaxError,
                    format!("{clause_name} position {pos} is not in select list"),
                )
                .with_position(span.start + 1),
            )));
        }
        if projections.is_empty() {
            return Ok(None);
        }
        let pos_usize = usize::try_from(pos).unwrap_or(usize::MAX);
        if pos_usize > projections.len() {
            return Err(DbError::Bind(Box::new(
                ErrorReport::new(
                    SqlState::SyntaxError,
                    format!("{clause_name} position {pos} is not in select list"),
                )
                .with_position(span.start + 1),
            )));
        }
        let idx = pos_usize - 1;
        let projection = &projections[idx];
        let normalized = normalize_projection_expr_for_join_aliases(&projection.expr, relation);
        let typed = infer_expr(&normalized, relation, params, sq, uf)?;
        return Ok(Some(typed));
    }
    Ok(None)
}

pub(super) fn infer_comparison_operands(
    left: &Expr,
    right: &Expr,
    relation: Option<&TableDescriptor>,
    params: &mut ParameterTypes,
    sq: Option<SubqueryResolver<'_>>,
    uf: Option<UserFunctionResolver<'_>>,
) -> DbResult<(TypedExpr, TypedExpr)> {
    fn coerce_array_literal(expr: TypedExpr, target_type: &DataType) -> TypedExpr {
        if !matches!(target_type, DataType::Array(_))
            || matches!(expr.data_type, DataType::Array(_))
        {
            return expr;
        }
        if matches!(expr.kind, TypedExprKind::Literal(Value::Text(_))) {
            return TypedExpr::cast(expr, target_type.clone());
        }
        expr
    }

    fn coerce_string_literal_to_peer_type(
        original: &Expr,
        typed: TypedExpr,
        peer_type: &DataType,
        relation: Option<&TableDescriptor>,
        params: &mut ParameterTypes,
        sq: Option<SubqueryResolver<'_>>,
        uf: Option<UserFunctionResolver<'_>>,
    ) -> DbResult<TypedExpr> {
        if matches!(typed.data_type, DataType::Text)
            && matches!(
                original,
                Expr::Literal(Literal::String(_), _) | Expr::Parameter { .. }
            )
            && !matches!(peer_type, DataType::Text)
        {
            return infer_expr_with_expected(original, relation, peer_type, true, params, sq, uf);
        }
        Ok(typed)
    }

    match (left, right) {
        (
            Expr::Parameter {
                index: left_index,
                span: left_span,
            },
            Expr::Parameter { .. },
        ) => Err(DbError::Bind(Box::new(
            ErrorReport::new(
                SqlState::SyntaxError,
                format!("could not infer data type of parameter ${left_index}"),
            )
            .with_position(left_span.start + 1),
        ))),
        (Expr::Parameter { .. }, _) => {
            let right = infer_expr(right, relation, params, sq, uf)?;
            let left =
                infer_expr_with_expected(left, relation, &right.data_type, true, params, sq, uf)?;
            Ok((left, right))
        }
        (_, Expr::Parameter { .. }) => {
            let left = infer_expr(left, relation, params, sq, uf)?;
            let right =
                infer_expr_with_expected(right, relation, &left.data_type, true, params, sq, uf)?;
            Ok((left, right))
        }
        _ => {
            let left_expr = infer_expr(left, relation, params, sq, uf)?;
            let right_expr = infer_expr(right, relation, params, sq, uf)?;
            let left_expr = coerce_string_literal_to_peer_type(
                left,
                left_expr,
                &right_expr.data_type,
                relation,
                params,
                sq,
                uf,
            )?;
            let right_expr = coerce_string_literal_to_peer_type(
                right,
                right_expr,
                &left_expr.data_type,
                relation,
                params,
                sq,
                uf,
            )?;
            let left_expr = coerce_array_literal(left_expr, &right_expr.data_type);
            let right_expr = coerce_array_literal(right_expr, &left_expr.data_type);
            Ok((left_expr, right_expr))
        }
    }
}

#[allow(dead_code)]
pub(super) fn subquery_not_supported(pos: usize) -> DbError {
    DbError::Bind(Box::new(
        ErrorReport::new(
            SqlState::SyntaxError,
            "subqueries are not supported in this context",
        )
        .with_position(pos),
    ))
}

pub(super) fn subquery_column_error(pos: usize) -> DbError {
    DbError::Bind(Box::new(
        ErrorReport::new(
            SqlState::SyntaxError,
            "subquery must return exactly one column",
        )
        .with_position(pos),
    ))
}
