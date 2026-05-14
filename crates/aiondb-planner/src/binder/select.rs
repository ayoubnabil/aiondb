#![allow(clippy::map_unwrap_or)]

use super::*;

impl Binder {
    pub(crate) fn bind_select(
        &self,
        select: &SelectStatement,
        txn_id: TxnId,
        default_schema: Option<&str>,
    ) -> DbResult<BoundSelect> {
        cte::validate_recursive_ctes(self, select, txn_id, default_schema)?;

        // Check if the FROM target references a CTE; if so, inline it
        if let Some(name) = &select.from {
            if let Some(cte_def) = cte::find_cte(&select.ctes, name) {
                return self.bind_cte_select(select, cte_def, txn_id, default_schema);
            }
        }

        // Check if the FROM target is a view; if so, inline the view query
        if let Some(name) = &select.from {
            for relation_name in relation_lookup_candidates(name, default_schema)? {
                if self.catalog.get_table(txn_id, &relation_name)?.is_some() {
                    break;
                }
                if let Some(view) = self.catalog.get_view(txn_id, &relation_name)? {
                    return self.bind_view_select(select, &view, txn_id, default_schema);
                }
            }
        }

        let relation = match &select.from {
            Some(name) => {
                let mut relation_name = relation_error_name(name, default_schema)?;
                let mut relation = None;
                for candidate in relation_lookup_candidates(name, default_schema)? {
                    relation_name = candidate;
                    if let Some(table) = self.catalog.get_table(txn_id, &relation_name)? {
                        relation = Some(table);
                        break;
                    }
                    if let Some(sequence) = self.catalog.get_sequence(txn_id, &relation_name)? {
                        relation = Some(build_sequence_relation_descriptor(
                            &relation_name,
                            &sequence,
                        ));
                        break;
                    }
                    if let Some(desc) = resolve_virtual_relation(&relation_name) {
                        relation = Some(desc);
                        break;
                    }
                }
                Some(relation.ok_or_else(|| undefined_table(name, &relation_name))?)
            }
            None => None,
        };

        // Resolve join tables (check tables first, then views, then CTEs)
        let mut joins: Vec<BoundJoin> = Vec::new();
        for join in &select.joins {
            let (join_relation, join_source) = if let Some(cte_def) =
                cte::find_cte(&select.ctes, &join.table)
            {
                let join_parent_ctes = cte::parent_ctes_without_self(&select.ctes, &cte_def.name);
                let outer_columns = merge_bound_outer_columns(
                    self.outer_columns.clone(),
                    cte::build_outer_scope_columns(
                        relation.as_ref(),
                        select.from_alias.as_deref(),
                        &joins,
                    ),
                );
                let effective_join_query =
                    cte::inject_parent_ctes(&cte_def.query, &join_parent_ctes);
                let cte_bound = self.bind(&effective_join_query, txn_id, default_schema)?;
                let join_relation = cte::build_cte_table_descriptor_from_bound(
                    cte_def,
                    self,
                    &cte_bound,
                    outer_columns,
                )?;
                let join_source = match cte_bound {
                    BoundStatement::Select(bound) => Some(Box::new(BoundStatement::Select(bound))),
                    BoundStatement::SetOperation(set_op) => {
                        Some(Box::new(BoundStatement::SetOperation(set_op)))
                    }
                    BoundStatement::Insert(insert) => {
                        Some(Box::new(BoundStatement::Insert(insert)))
                    }
                    BoundStatement::Update(update) => {
                        Some(Box::new(BoundStatement::Update(update)))
                    }
                    BoundStatement::Delete(delete) => {
                        Some(Box::new(BoundStatement::Delete(delete)))
                    }
                    _ => {
                        return Err(DbError::feature_not_supported(
                            "CTE in JOIN must resolve to a SELECT, set operation, or data-modifying statement",
                        ));
                    }
                };
                (join_relation, join_source)
            } else {
                let mut join_name = relation_error_name(&join.table, default_schema)?;
                let mut relation = None;
                for candidate in relation_lookup_candidates(&join.table, default_schema)? {
                    join_name = candidate;
                    if let Some(table) = self.catalog.get_table(txn_id, &join_name)? {
                        relation = Some(table);
                        break;
                    }
                    if let Some(sequence) = self.catalog.get_sequence(txn_id, &join_name)? {
                        relation = Some(build_sequence_relation_descriptor(&join_name, &sequence));
                        break;
                    }
                    if let Some(view) = self.catalog.get_view(txn_id, &join_name)? {
                        relation = Some(
                            views::resolve_view_underlying_table(&view, &*self.catalog, txn_id, 0)?
                                .unwrap_or_else(|| views::build_view_table_descriptor(&view)),
                        );
                        break;
                    }
                    if let Some(desc) = resolve_virtual_relation(&join_name) {
                        relation = Some(desc);
                        break;
                    }
                }
                let relation = relation.ok_or_else(|| undefined_table(&join.table, &join_name))?;
                (relation, None)
            };
            // Expand USING/NATURAL into ON conditions
            let left_alias = if let Some(prev_join) = joins.last() {
                prev_join
                    .alias
                    .clone()
                    .unwrap_or_else(|| prev_join.relation.name.object_name().to_owned())
            } else {
                select
                    .from_alias
                    .clone()
                    .or_else(|| relation.as_ref().map(|r| r.name.object_name().to_owned()))
                    .unwrap_or_default()
            };
            let right_alias = join
                .alias
                .clone()
                .unwrap_or_else(|| join_relation.name.object_name().to_owned());
            let (condition, using_columns) = if !join.using_columns.is_empty() {
                (
                    Some(build_using_condition(
                        &join.using_columns,
                        &left_alias,
                        &right_alias,
                        join.span,
                    )?),
                    join.using_columns.clone(),
                )
            } else if join.natural {
                // Collect columns from the primary relation AND all previously
                // joined tables so NATURAL JOIN considers the full left side.
                let mut left_cols: std::collections::HashSet<&str> = relation
                    .as_ref()
                    .map(|r| r.columns.iter().map(|c| c.name.as_str()).collect())
                    .unwrap_or_default();
                for prev_join in &joins {
                    for c in &prev_join.relation.columns {
                        left_cols.insert(c.name.as_str());
                    }
                }
                let shared: Vec<String> = join_relation
                    .columns
                    .iter()
                    .filter(|c| left_cols.contains(c.name.as_str()))
                    .map(|c| c.name.clone())
                    .collect();
                if shared.is_empty() {
                    (None, Vec::new())
                } else {
                    (
                        Some(build_using_condition(
                            &shared,
                            &left_alias,
                            &right_alias,
                            join.span,
                        )?),
                        shared,
                    )
                }
            } else {
                (join.condition.clone(), Vec::new())
            };
            let effective_join_alias = join
                .alias
                .clone()
                .or_else(|| join.table.parts.last().cloned());
            joins.push(BoundJoin {
                join_type: join.join_type,
                relation: join_relation,
                alias: effective_join_alias,
                condition,
                source: join_source,
                using_columns,
                using_alias: join.using_alias.clone(),
            });
        }

        let mut projections = Vec::new();
        for item in &select.items {
            if is_star_expr(&item.expr) {
                let Some(ref table) = relation else {
                    return Err(DbError::bind_error(
                        SqlState::SyntaxError,
                        "SELECT * requires a FROM clause",
                    ));
                };
                // Check if this is a qualified star (e.g. t.*)
                let qualifier = if let Expr::Identifier(name) = &item.expr {
                    if name.parts.len() >= 2 {
                        // The qualifier is everything except the last part (*)
                        Some(name.parts[name.parts.len() - 2].clone())
                    } else {
                        None
                    }
                } else {
                    None
                };

                if let Some(ref qual) = qualifier {
                    // Qualified star: expand only columns from the matching
                    // table/alias.  Check primary table alias first, then joins.
                    let primary_alias = select
                        .from_alias
                        .as_deref()
                        .unwrap_or_else(|| table.name.object_name());
                    if qual.eq_ignore_ascii_case(primary_alias) {
                        for column in &table.columns {
                            projections.push(BoundProjection {
                                alias: None,
                                expr: Expr::Identifier(ObjectName {
                                    parts: vec![qual.clone(), column.name.clone()],
                                    span: item.span,
                                }),
                            });
                        }
                    } else {
                        let mut found = false;
                        for bound_join in &joins {
                            let join_alias = bound_join
                                .alias
                                .as_deref()
                                .unwrap_or_else(|| bound_join.relation.name.object_name());
                            if qual.eq_ignore_ascii_case(join_alias) {
                                for column in &bound_join.relation.columns {
                                    projections.push(BoundProjection {
                                        alias: None,
                                        expr: Expr::Identifier(ObjectName {
                                            parts: vec![qual.clone(), column.name.clone()],
                                            span: item.span,
                                        }),
                                    });
                                }
                                found = true;
                                break;
                            }
                        }
                        if !found {
                            for bound_join in &joins {
                                if bound_join
                                    .using_alias
                                    .as_deref()
                                    .is_some_and(|alias| qual.eq_ignore_ascii_case(alias))
                                {
                                    for column_name in &bound_join.using_columns {
                                        projections.push(BoundProjection {
                                            alias: None,
                                            expr: Expr::Identifier(ObjectName {
                                                parts: vec![qual.clone(), column_name.clone()],
                                                span: item.span,
                                            }),
                                        });
                                    }
                                    found = true;
                                    break;
                                }
                            }
                        }
                        if !found {
                            // Qualifier didn't match any table - treat as
                            // column reference (will error later in type check).
                            projections.push(BoundProjection {
                                alias: item.alias.clone(),
                                expr: item.expr.clone(),
                            });
                        }
                    }
                } else {
                    // Unqualified star: expand ALL columns from all tables.
                    // Use aliases when available so that self-joins produce
                    // distinct qualified column references.
                    let primary_public_name = select
                        .from_alias
                        .clone()
                        .unwrap_or_else(|| table.name.object_name().to_owned());
                    let join_public_names = joins
                        .iter()
                        .map(|join| {
                            join.alias
                                .clone()
                                .unwrap_or_else(|| join.relation.name.object_name().to_owned())
                        })
                        .collect::<Vec<_>>();
                    let primary_name = if join_public_names
                        .iter()
                        .any(|name| name.eq_ignore_ascii_case(&primary_public_name))
                    {
                        table.name.object_name().to_owned()
                    } else {
                        primary_public_name
                    };
                    for column in &table.columns {
                        projections.push(BoundProjection {
                            alias: None,
                            expr: Expr::Identifier(ObjectName {
                                parts: vec![primary_name.clone(), column.name.clone()],
                                span: item.span,
                            }),
                        });
                    }
                    for (join_index, bound_join) in joins.iter().enumerate() {
                        let join_public_name = bound_join
                            .alias
                            .clone()
                            .unwrap_or_else(|| bound_join.relation.name.object_name().to_owned());
                        let duplicates_primary = join_public_name
                            .eq_ignore_ascii_case(&primary_name)
                            || join_public_name.eq_ignore_ascii_case(
                                select
                                    .from_alias
                                    .as_deref()
                                    .unwrap_or_else(|| table.name.object_name()),
                            );
                        let duplicates_other_join =
                            join_public_names
                                .iter()
                                .enumerate()
                                .any(|(other_index, name)| {
                                    other_index != join_index
                                        && name.eq_ignore_ascii_case(&join_public_name)
                                });
                        let join_name = if duplicates_primary || duplicates_other_join {
                            bound_join.relation.name.object_name().to_owned()
                        } else {
                            join_public_name
                        };
                        for column in &bound_join.relation.columns {
                            // Skip USING columns from the right side - they are
                            // already included from the left side (PostgreSQL
                            // coalesces them into a single output column).
                            if bound_join
                                .using_columns
                                .iter()
                                .any(|uc| uc.eq_ignore_ascii_case(&column.name))
                            {
                                continue;
                            }
                            projections.push(BoundProjection {
                                alias: None,
                                expr: Expr::Identifier(ObjectName {
                                    parts: vec![join_name.clone(), column.name.clone()],
                                    span: item.span,
                                }),
                            });
                        }
                    }
                }
            } else {
                projections.push(BoundProjection {
                    alias: item.alias.clone(),
                    expr: item.expr.clone(),
                });
            }
        }

        Ok(BoundSelect {
            row_lock: select.row_lock.clone(),
            relation,
            from_alias: select.from_alias.clone(),
            source: None,
            joins,
            projections,
            selection: select.selection.clone(),
            group_by: select.group_by.clone(),
            group_by_items: select.group_by_items.clone(),
            having: select.having.clone(),
            order_by: select
                .order_by
                .iter()
                .map(|item| BoundOrderBy {
                    expr: item.expr.clone(),
                    descending: item.descending,
                    nulls_first: item.nulls_first,
                })
                .collect(),
            limit: select.limit.clone(),
            offset: select.offset.clone(),
            distinct: select.distinct.clone(),
        })
    }
}

/// Build an ON condition from USING/NATURAL column names.
/// `t1 JOIN t2 USING (a, b)` becomes `t1.a = t2.a AND t1.b = t2.b`.
pub(super) fn build_using_condition(
    columns: &[String],
    left_table: &str,
    right_table: &str,
    span: Span,
) -> DbResult<Expr> {
    let mut conditions: Vec<Expr> = columns
        .iter()
        .map(|col| {
            let left = Expr::Identifier(ObjectName {
                parts: vec![left_table.to_owned(), col.clone()],
                span,
            });
            let right = Expr::Identifier(ObjectName {
                parts: vec![right_table.to_owned(), col.clone()],
                span,
            });
            Expr::BinaryOp {
                left: Box::new(left),
                op: BinaryOperator::Eq,
                right: Box::new(right),
                span,
            }
        })
        .collect();
    let Some(mut result) = conditions.pop() else {
        return Err(DbError::bind_error(
            SqlState::SyntaxError,
            "JOIN USING requires at least one column",
        )
        .with_position(span.start + 1));
    };
    while let Some(cond) = conditions.pop() {
        result = Expr::BinaryOp {
            left: Box::new(cond),
            op: BinaryOperator::And,
            right: Box::new(result),
            span,
        };
    }
    Ok(result)
}
