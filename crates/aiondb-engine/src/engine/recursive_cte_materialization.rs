/// Wrap the base query of a recursive CTE as a standalone Statement that
/// can be planned and executed.
fn build_base_statement(cte: &CteDefinition, outer_select: &SelectStatement) -> Statement {
    let mut base = (*cte.query).clone();
    inject_non_recursive_ctes(&mut base, outer_select, &cte.name);
    base
}

/// Build a statement for one iteration of the recursive term.
///
/// The recursive term is a SELECT that references the CTE name in its FROM.
/// We replace that FROM reference with a subquery over the current working
/// table rows (inlined as VALUES).
fn build_recursive_iteration_statement(
    recursive_term: &SelectStatement,
    cte: &CteDefinition,
    cte_col_names: &[String],
    cte_col_types: &[DataType],
    working_rows: &[Row],
    outer_select: &SelectStatement,
    max_synthetic_values_rows: usize,
) -> DbResult<Statement> {
    let sp = Span::default();

    let worktable_cte = build_worktable_cte(
        &cte.name,
        cte_col_names,
        cte_col_types,
        working_rows,
        sp,
        max_synthetic_values_rows,
    )?;

    let mut scope = SelectStatement {
        row_lock: None,
        ctes: vec![worktable_cte],
        distinct: DistinctKind::All,
        items: Vec::new(),
        from: None,
        from_alias: None,
        from_span: None,
        joins: Vec::new(),
        selection: None,
        where_span: None,
        group_by: Vec::new(),
        group_by_items: Vec::new(),
        group_by_span: None,
        having: None,
        having_span: None,
        window_definitions: Vec::new(),
        order_by: Vec::new(),
        order_by_span: None,
        limit: None,
        limit_span: None,
        offset: None,
        offset_span: None,
        span: sp,
    };
    for outer_cte in &outer_select.ctes {
        if !outer_cte.name.eq_ignore_ascii_case(&cte.name) && outer_cte.recursive_term.is_none() {
            scope.ctes.push(outer_cte.clone());
        }
    }

    let mut iter_stmt = Statement::Select(recursive_term.clone());
    inject_non_recursive_ctes(&mut iter_stmt, &scope, "");
    Ok(iter_stmt)
}

/// Build a non-recursive CTE definition that exposes `working_rows` as a
/// relation named `cte_name` with the given column names.
fn build_worktable_cte(
    cte_name: &str,
    col_names: &[String],
    col_types: &[DataType],
    rows: &[Row],
    sp: Span,
    max_synthetic_values_rows: usize,
) -> DbResult<CteDefinition> {
    let items: Vec<SelectItem> = col_names
        .iter()
        .map(|name| SelectItem {
            expr: Expr::Identifier(ObjectName {
                parts: vec![name.clone()],
                span: sp,
            }),
            alias: None,
            span: sp,
        })
        .collect();

    let inner_select = SelectStatement {
        row_lock: None,
        ctes: Vec::new(),
        distinct: DistinctKind::All,
        items,
        from: Some(ObjectName {
            parts: vec!["_v".to_owned()],
            span: sp,
        }),
        from_alias: None,
        from_span: Some(sp),
        joins: Vec::new(),
        selection: None,
        where_span: None,
        group_by: Vec::new(),
        group_by_items: Vec::new(),
        group_by_span: None,
        having: None,
        having_span: None,
        window_definitions: Vec::new(),
        order_by: Vec::new(),
        order_by_span: None,
        limit: None,
        limit_span: None,
        offset: None,
        offset_span: None,
        span: sp,
    };

    let v_cte = build_values_cte(
        "_v",
        col_names,
        rows,
        sp,
        max_synthetic_values_rows,
        Some(col_types),
    )?;

    let mut inner_select = inner_select;
    inner_select.ctes.push(v_cte);

    Ok(CteDefinition {
        name: cte_name.to_owned(),
        column_aliases: Some(col_names.to_vec()),
        recursive: false,
        query: Box::new(Statement::Select(inner_select)),
        recursive_term: None,
        union_all: false,
        span: sp,
    })
}

/// Build a CTE that wraps literal rows as SELECT ... UNION ALL SELECT ...
fn build_values_cte(
    name: &str,
    col_names: &[String],
    rows: &[Row],
    sp: Span,
    max_synthetic_values_rows: usize,
    empty_row_types: Option<&[DataType]>,
) -> DbResult<CteDefinition> {
    if rows.is_empty() {
        let items: Vec<SelectItem> = if col_names.is_empty() {
            vec![SelectItem {
                expr: Expr::Literal(Literal::Null, sp),
                alias: Some("_empty".to_owned()),
                span: sp,
            }]
        } else {
            col_names
                .iter()
                .enumerate()
                .map(|(i, name)| SelectItem {
                    expr: empty_row_types.and_then(|types| types.get(i)).map_or_else(
                        || Expr::Literal(Literal::Null, sp),
                        |data_type| Expr::Cast {
                            expr: Box::new(Expr::Literal(Literal::Null, sp)),
                            data_type: data_type.clone(),
                            span: sp,
                        },
                    ),
                    alias: Some(name.clone()),
                    span: sp,
                })
                .collect()
        };
        let body = Statement::Select(SelectStatement {
            row_lock: None,
            ctes: Vec::new(),
            distinct: DistinctKind::All,
            items,
            from: None,
            from_alias: None,
            from_span: None,
            joins: Vec::new(),
            selection: Some(Expr::Literal(Literal::Boolean(false), sp)),
            where_span: Some(sp),
            group_by: Vec::new(),
            group_by_items: Vec::new(),
            group_by_span: None,
            having: None,
            having_span: None,
            window_definitions: Vec::new(),
            order_by: Vec::new(),
            order_by_span: None,
            limit: None,
            limit_span: None,
            offset: None,
            offset_span: None,
            span: sp,
        });
        return Ok(CteDefinition {
            name: name.to_owned(),
            column_aliases: Some(col_names.to_vec()),
            recursive: false,
            query: Box::new(body),
            recursive_term: None,
            union_all: false,
            span: sp,
        });
    }

    if rows.len() > max_synthetic_values_rows {
        return Err(DbError::program_limit(format!(
            "recursive query materialization exceeded synthetic VALUES row cap ({max_synthetic_values_rows})",
        )));
    }

    // Defence-in-depth: the row-count cap derives from a 256-byte/row estimate
    // which is unsafe for adversarial wide rows (e.g. repeat('A', 100_000)).
    // Fall back to an absolute byte ceiling so the AST clone below cannot
    // grow the heap unbounded between memory checks.
    const MAX_SYNTHETIC_VALUES_TOTAL_BYTES: usize = 64 * 1024 * 1024;
    let mut total_bytes: usize = 0;
    for row in rows {
        for value in &row.values {
            // Approximate each Value's heap cost via its serialized length.
            // Variants that own large allocations need explicit accounting;
            // a flat 16-byte estimate would let an attacker land a huge
            // `Vector` parameter and bypass the 64 MiB ceiling
            // (audit second-opinion).
            let est = match value {
                aiondb_core::Value::Text(s) => s.len(),
                aiondb_core::Value::Blob(b) => b.len(),
                aiondb_core::Value::Vector(v) => v.values.len() * 4,
                aiondb_core::Value::Jsonb(j) => j.to_string().len(),
                aiondb_core::Value::Numeric(_) => 320,
                aiondb_core::Value::Array(a) => 32 + a.len() * 32,
                _ => 16,
            };
            total_bytes = total_bytes.saturating_add(est).saturating_add(16);
            if total_bytes > MAX_SYNTHETIC_VALUES_TOTAL_BYTES {
                return Err(DbError::program_limit(format!(
                    "recursive query materialization exceeded synthetic VALUES byte cap ({MAX_SYNTHETIC_VALUES_TOTAL_BYTES} bytes)",
                )));
            }
        }
    }

    ensure_synthetic_union_depth(rows.len())?;

    let make_select = |row: &Row| -> DbResult<SelectStatement> {
        let items: Vec<SelectItem> = row
            .values
            .iter()
            .enumerate()
            .map(|(i, value)| {
                let expr = match empty_row_types.and_then(|types| types.get(i)) {
                    Some(data_type) => Expr::Cast {
                        expr: Box::new(value_to_expr(value, sp)?),
                        data_type: data_type.clone(),
                        span: sp,
                    },
                    None => value_to_expr(value, sp)?,
                };
                Ok(SelectItem {
                    expr,
                    alias: col_names.get(i).cloned(),
                    span: sp,
                })
            })
            .collect::<DbResult<_>>()?;
        Ok(SelectStatement {
            row_lock: None,
            ctes: Vec::new(),
            distinct: DistinctKind::All,
            items,
            from: None,
            from_alias: None,
            from_span: None,
            joins: Vec::new(),
            selection: None,
            where_span: None,
            group_by: Vec::new(),
            group_by_items: Vec::new(),
            group_by_span: None,
            having: None,
            having_span: None,
            window_definitions: Vec::new(),
            order_by: Vec::new(),
            order_by_span: None,
            limit: None,
            limit_span: None,
            offset: None,
            offset_span: None,
            span: sp,
        })
    };

    let body: Statement = if rows.len() == 1 {
        Statement::Select(make_select(&rows[0])?)
    } else {
        let mut level: Vec<Statement> = rows
            .iter()
            .map(|row| Ok(Statement::Select(make_select(row)?)))
            .collect::<DbResult<_>>()?;
        while level.len() > 1 {
            let mut next = Vec::with_capacity(level.len().div_ceil(2));
            let mut iter = level.into_iter();
            while let Some(left) = iter.next() {
                if let Some(right) = iter.next() {
                    next.push(Statement::SetOperation(SetOperationStatement {
                        op: SetOperationType::Union,
                        all: true,
                        left: Box::new(left),
                        right: Box::new(right),
                        order_by: Vec::new(),
                        order_by_span: None,
                        limit: None,
                        limit_span: None,
                        offset: None,
                        offset_span: None,
                        span: sp,
                    }));
                } else {
                    next.push(left);
                }
            }
            level = next;
        }
        level.pop().ok_or_else(|| {
            DbError::internal("balanced UNION ALL builder requires at least one row")
        })?
    };

    Ok(CteDefinition {
        name: name.to_owned(),
        column_aliases: Some(col_names.to_vec()),
        recursive: false,
        query: Box::new(body),
        recursive_term: None,
        union_all: false,
        span: sp,
    })
}

/// Replace a recursive CTE definition with a non-recursive one whose body is
/// the precomputed rows.
fn materialise_cte(
    original: &CteDefinition,
    rows: Vec<Row>,
    col_names: Vec<String>,
    col_types: Vec<DataType>,
    max_synthetic_values_rows: usize,
) -> DbResult<CteDefinition> {
    let sp = original.span;

    if rows.is_empty() {
        let items: Vec<SelectItem> = if col_names.is_empty() {
            vec![SelectItem {
                expr: Expr::Literal(Literal::Null, sp),
                alias: Some("_empty".to_owned()),
                span: sp,
            }]
        } else {
            col_names
                .iter()
                .enumerate()
                .map(|(i, name)| SelectItem {
                    expr: col_types.get(i).map_or_else(
                        || Expr::Literal(Literal::Null, sp),
                        |data_type| Expr::Cast {
                            expr: Box::new(Expr::Literal(Literal::Null, sp)),
                            data_type: data_type.clone(),
                            span: sp,
                        },
                    ),
                    alias: Some(name.clone()),
                    span: sp,
                })
                .collect()
        };

        let body = Statement::Select(SelectStatement {
            row_lock: None,
            ctes: Vec::new(),
            distinct: DistinctKind::All,
            items,
            from: None,
            from_alias: None,
            from_span: None,
            joins: Vec::new(),
            selection: Some(Expr::Literal(Literal::Boolean(false), sp)),
            where_span: Some(sp),
            group_by: Vec::new(),
            group_by_items: Vec::new(),
            group_by_span: None,
            having: None,
            having_span: None,
            window_definitions: Vec::new(),
            order_by: Vec::new(),
            order_by_span: None,
            limit: None,
            limit_span: None,
            offset: None,
            offset_span: None,
            span: sp,
        });

        return Ok(CteDefinition {
            name: original.name.clone(),
            column_aliases: original.column_aliases.clone().or(Some(col_names)),
            recursive: false,
            query: Box::new(body),
            recursive_term: None,
            union_all: false,
            span: sp,
        });
    }

    let body_cte = build_values_cte(
        "_rcte_v",
        &col_names,
        &rows,
        sp,
        max_synthetic_values_rows,
        Some(&col_types),
    )?;

    let items: Vec<SelectItem> = col_names
        .iter()
        .map(|name| SelectItem {
            expr: Expr::Identifier(ObjectName {
                parts: vec![name.clone()],
                span: sp,
            }),
            alias: None,
            span: sp,
        })
        .collect();

    let body = Statement::Select(SelectStatement {
        row_lock: None,
        ctes: vec![body_cte],
        distinct: DistinctKind::All,
        items,
        from: Some(ObjectName {
            parts: vec!["_rcte_v".to_owned()],
            span: sp,
        }),
        from_alias: None,
        from_span: Some(sp),
        joins: Vec::new(),
        selection: None,
        where_span: None,
        group_by: Vec::new(),
        group_by_items: Vec::new(),
        group_by_span: None,
        having: None,
        having_span: None,
        window_definitions: Vec::new(),
        order_by: Vec::new(),
        order_by_span: None,
        limit: None,
        limit_span: None,
        offset: None,
        offset_span: None,
        span: sp,
    });

    Ok(CteDefinition {
        name: original.name.clone(),
        column_aliases: original.column_aliases.clone().or(Some(col_names)),
        recursive: false,
        query: Box::new(body),
        recursive_term: None,
        union_all: false,
        span: sp,
    })
}

/// Convert a runtime `Value` into an AST `Expr` literal suitable for inclusion
/// in a synthesised SELECT.
const SYNTHETIC_VALUE_EXPR_MAX_DEPTH: usize = 256;

fn value_to_expr(value: &Value, sp: Span) -> DbResult<Expr> {
    value_to_expr_at_depth(value, sp, 0)
}

fn value_to_expr_at_depth(value: &Value, sp: Span, depth: usize) -> DbResult<Expr> {
    if depth >= SYNTHETIC_VALUE_EXPR_MAX_DEPTH {
        return Err(DbError::program_limit(format!(
            "recursive query value nesting depth exceeds limit {SYNTHETIC_VALUE_EXPR_MAX_DEPTH}"
        )));
    }
    match value {
        Value::Null => Ok(Expr::Literal(Literal::Null, sp)),
        Value::Int(i) => Ok(Expr::Literal(Literal::Integer(i64::from(*i)), sp)),
        Value::BigInt(i) => Ok(Expr::Cast {
            expr: Box::new(Expr::Literal(Literal::Integer(*i), sp)),
            data_type: aiondb_core::DataType::BigInt,
            span: sp,
        }),
        Value::Real(f) => Ok(Expr::Literal(Literal::NumericLit(f.to_string()), sp)),
        Value::Double(f) => Ok(Expr::Literal(Literal::NumericLit(f.to_string()), sp)),
        Value::Numeric(n) => Ok(Expr::Literal(Literal::NumericLit(n.to_string()), sp)),
        Value::Money(value) => Ok(Expr::Cast {
            expr: Box::new(Expr::Literal(
                Literal::String(Value::Money(*value).to_string()),
                sp,
            )),
            data_type: aiondb_core::DataType::Money,
            span: sp,
        }),
        Value::Text(s) => Ok(Expr::Literal(Literal::String(s.clone()), sp)),
        Value::Boolean(b) => Ok(Expr::Literal(Literal::Boolean(*b), sp)),
        Value::Date(d) => Ok(Expr::Cast {
            expr: Box::new(Expr::Literal(Literal::String(d.to_string()), sp)),
            data_type: aiondb_core::DataType::Date,
            span: sp,
        }),
        Value::LargeDate(d) => Ok(Expr::Cast {
            expr: Box::new(Expr::Literal(Literal::String(d.to_string()), sp)),
            data_type: aiondb_core::DataType::Date,
            span: sp,
        }),
        Value::Time(t) => Ok(Expr::Cast {
            expr: Box::new(Expr::Literal(Literal::String(t.to_string()), sp)),
            data_type: aiondb_core::DataType::Time,
            span: sp,
        }),
        Value::TimeTz(t, offset) => Ok(Expr::Cast {
            expr: Box::new(Expr::Literal(Literal::String(format!("{t}{offset}")), sp)),
            data_type: aiondb_core::DataType::TimeTz,
            span: sp,
        }),
        Value::Timestamp(ts) => Ok(Expr::Cast {
            expr: Box::new(Expr::Literal(Literal::String(ts.to_string()), sp)),
            data_type: aiondb_core::DataType::Timestamp,
            span: sp,
        }),
        Value::TimestampTz(ts) => Ok(Expr::Cast {
            expr: Box::new(Expr::Literal(Literal::String(ts.to_string()), sp)),
            data_type: aiondb_core::DataType::TimestampTz,
            span: sp,
        }),
        Value::MacAddr(value) => Ok(Expr::Cast {
            expr: Box::new(Expr::Literal(Literal::String(value.to_string()), sp)),
            data_type: aiondb_core::DataType::MacAddr,
            span: sp,
        }),
        Value::MacAddr8(value) => Ok(Expr::Cast {
            expr: Box::new(Expr::Literal(Literal::String(value.to_string()), sp)),
            data_type: aiondb_core::DataType::MacAddr8,
            span: sp,
        }),
        Value::PgLsn(value) => Ok(Expr::Cast {
            expr: Box::new(Expr::Literal(Literal::String(value.to_string()), sp)),
            data_type: aiondb_core::DataType::PgLsn,
            span: sp,
        }),
        Value::Uuid(u) => {
            let formatted = Value::Uuid(*u).to_string();
            Ok(Expr::Cast {
                expr: Box::new(Expr::Literal(Literal::String(formatted), sp)),
                data_type: aiondb_core::DataType::Uuid,
                span: sp,
            })
        }
        Value::Blob(b) => {
            let hex: String = b.iter().fold(
                String::with_capacity(b.len().saturating_mul(2)),
                |mut acc, byte| {
                    use std::fmt::Write;
                    let _ = write!(acc, "{byte:02x}");
                    acc
                },
            );
            Ok(Expr::Literal(Literal::String(format!("\\x{hex}")), sp))
        }
        Value::Interval(iv) => Ok(Expr::Cast {
            expr: Box::new(Expr::Literal(Literal::String(iv.to_string()), sp)),
            data_type: aiondb_core::DataType::Interval,
            span: sp,
        }),
        Value::Tid(value) => Ok(Expr::Cast {
            expr: Box::new(Expr::Literal(Literal::String(value.to_string()), sp)),
            data_type: aiondb_core::DataType::Tid,
            span: sp,
        }),
        Value::Jsonb(j) => Ok(Expr::Cast {
            expr: Box::new(Expr::Literal(Literal::String(j.to_string()), sp)),
            data_type: aiondb_core::DataType::Jsonb,
            span: sp,
        }),
        Value::Vector(v) => {
            let text = format!(
                "[{}]",
                v.values
                    .iter()
                    .map(|f| f.to_string())
                    .collect::<Vec<_>>()
                    .join(",")
            );
            Ok(Expr::Cast {
                expr: Box::new(Expr::Literal(Literal::String(text), sp)),
                data_type: aiondb_core::DataType::Vector {
                    dims: v.dims,
                    element_type: aiondb_core::VectorElementType::Float32,
                },
                span: sp,
            })
        }
        Value::Array(elems) => Ok(Expr::Array {
            elements: elems
                .iter()
                .map(|value| value_to_expr_at_depth(value, sp, depth + 1))
                .collect::<DbResult<_>>()?,
            span: sp,
        }),
    }
}

/// Inject non-recursive CTEs from the outer SELECT into `stmt` so that the
/// base or recursive term can reference sibling CTEs.
fn inject_non_recursive_ctes(stmt: &mut Statement, outer: &SelectStatement, exclude_name: &str) {
    fn with_outer_non_recursive_ctes(
        select: &mut SelectStatement,
        outer: &SelectStatement,
        exclude_name: &str,
    ) {
        for cte in &mut select.ctes {
            inject_non_recursive_ctes(cte.query.as_mut(), outer, exclude_name);
            if let Some(recursive_term) = &mut cte.recursive_term {
                let mut recursive_stmt = Statement::Select(recursive_term.as_ref().clone());
                inject_non_recursive_ctes(&mut recursive_stmt, outer, exclude_name);
                if let Statement::Select(updated_recursive_term) = recursive_stmt {
                    **recursive_term = updated_recursive_term;
                }
            }
        }
        for cte in &outer.ctes {
            if cte.recursive_term.is_none()
                && !cte.name.eq_ignore_ascii_case(exclude_name)
                && !select
                    .ctes
                    .iter()
                    .any(|c| c.name.eq_ignore_ascii_case(&cte.name))
            {
                select.ctes.push(cte.clone());
            }
        }
    }

    match stmt {
        Statement::Select(sel) => {
            with_outer_non_recursive_ctes(sel, outer, exclude_name);
        }
        Statement::Insert(insert) => {
            if let Some(query) = insert.query.as_mut() {
                with_outer_non_recursive_ctes(query, outer, exclude_name);
            }
        }
        Statement::Merge(merge) => {
            if let aiondb_parser::MergeSource::Subquery(query) = &mut merge.source {
                with_outer_non_recursive_ctes(query, outer, exclude_name);
            }
        }
        Statement::Copy(copy) => {
            if let Some(query) = copy.query.as_mut() {
                inject_non_recursive_ctes(query, outer, exclude_name);
            }
        }
        Statement::SetOperation(set_op) => {
            inject_non_recursive_ctes(set_op.left.as_mut(), outer, exclude_name);
            inject_non_recursive_ctes(set_op.right.as_mut(), outer, exclude_name);
        }
        Statement::Explain { statement, .. } => {
            inject_non_recursive_ctes(statement.as_mut(), outer, exclude_name);
        }
        _ => {}
    }
}
