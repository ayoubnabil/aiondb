#![allow(
    clippy::assigning_clones,
    clippy::doc_markdown,
    clippy::ref_option,
    clippy::uninlined_format_args
)]

use super::*;
use aiondb_core::ErrorReport;
use aiondb_parser::Literal;

impl Binder {
    /// Bind a SELECT that references a CTE in its FROM clause.
    /// The CTE's inner query is bound and inlined, similar to view expansion.
    pub(super) fn bind_cte_select(
        &self,
        select: &SelectStatement,
        cte: &aiondb_parser::CteDefinition,
        txn_id: TxnId,
        default_schema: Option<&str>,
    ) -> DbResult<BoundSelect> {
        validate_recursive_cte_subquery_scope_in_bind_path(cte)?;

        let parent_ctes = parent_ctes_without_self(&select.ctes, &cte.name);
        let effective_alias = select
            .from_alias
            .clone()
            .unwrap_or_else(|| cte.name.clone());
        let effective_query = inject_parent_ctes(&cte.query, &parent_ctes);
        let effective_query = rewrite_json_populate_record_source_query(&effective_query);
        let cte_bound = self.bind(&effective_query, txn_id, default_schema)?;
        let cte_table = match build_cte_table_descriptor_from_bound(
            cte,
            self,
            &cte_bound,
            self.outer_columns.clone(),
        ) {
            Ok(table) => table,
            Err(err) => fallback_internal_from_function_cte_table_descriptor(cte).ok_or(err)?,
        };
        let cte_source = match cte_bound {
            BoundStatement::Select(bound) => BoundStatement::Select(bound),
            BoundStatement::SetOperation(set_op) => BoundStatement::SetOperation(set_op),
            BoundStatement::Insert(insert) => BoundStatement::Insert(insert),
            BoundStatement::Update(update) => BoundStatement::Update(update),
            BoundStatement::Delete(delete) => BoundStatement::Delete(delete),
            _ => {
                return Err(DbError::feature_not_supported(
                    "CTE in FROM must resolve to a SELECT, set operation, or data-modifying statement",
                ));
            }
        };

        let mut bound = BoundSelect {
            row_lock: select.row_lock.clone(),
            relation: Some(cte_table.clone()),
            from_alias: Some(effective_alias.clone()),
            source: Some(Box::new(cte_source)),
            joins: Vec::new(),
            projections: Vec::new(),
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
        };
        let json_record_projection_info = infer_json_populate_record_projection_info(cte);

        for join in &select.joins {
            let (join_relation, join_alias, join_source) = if let Some(cte_def) =
                find_cte(&select.ctes, &join.table)
            {
                let join_parent_ctes = parent_ctes_without_self(&select.ctes, &cte_def.name);
                let effective_join_query = inject_parent_ctes(&cte_def.query, &join_parent_ctes);
                let cte_bound = self.bind(&effective_join_query, txn_id, default_schema)?;
                let outer_columns = merge_bound_outer_columns(
                    self.outer_columns.clone(),
                    build_outer_scope_columns(
                        Some(&cte_table),
                        Some(&effective_alias),
                        &bound.joins,
                    ),
                );
                let table = build_cte_table_descriptor_from_bound(
                    cte_def,
                    self,
                    &cte_bound,
                    outer_columns,
                )?;
                let alias = join.alias.clone().unwrap_or_else(|| cte_def.name.clone());
                (table, Some(alias), Some(Box::new(cte_bound)))
            } else {
                let mut join_name = relation_error_name(&join.table, default_schema)?;
                let mut join_relation = None;
                for candidate in relation_lookup_candidates(&join.table, default_schema)? {
                    join_name = candidate.clone();
                    if let Some(table) = self.catalog.get_table(txn_id, &candidate)? {
                        join_relation = Some(table);
                        break;
                    }
                    if let Some(view) = self.catalog.get_view(txn_id, &candidate)? {
                        let desc = super::views::resolve_view_underlying_table(
                            &view,
                            &*self.catalog,
                            txn_id,
                            0,
                        )?
                        .unwrap_or_else(|| super::views::build_view_table_descriptor(&view));
                        join_relation = Some(desc);
                        break;
                    }
                    if let Some(desc) = resolve_virtual_relation(&candidate) {
                        join_relation = Some(desc);
                        break;
                    }
                }
                (
                    join_relation.ok_or_else(|| undefined_table(&join.table, &join_name))?,
                    join.alias.clone(),
                    None,
                )
            };
            let left_alias = if let Some(prev_join) = bound.joins.last() {
                prev_join
                    .alias
                    .clone()
                    .unwrap_or_else(|| prev_join.relation.name.object_name().to_owned())
            } else {
                effective_alias.clone()
            };
            let right_alias = join_alias
                .clone()
                .unwrap_or_else(|| join_relation.name.object_name().to_owned());
            let (condition, using_columns) = if !join.using_columns.is_empty() {
                (
                    Some(super::select::build_using_condition(
                        &join.using_columns,
                        &left_alias,
                        &right_alias,
                        join.span,
                    )?),
                    join.using_columns.clone(),
                )
            } else if join.natural {
                // Collect columns from the primary CTE and all previously
                // joined tables.
                let mut left_cols: std::collections::HashSet<&str> =
                    cte_table.columns.iter().map(|c| c.name.as_str()).collect();
                for prev_join in &bound.joins {
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
                        Some(super::select::build_using_condition(
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
            bound.joins.push(BoundJoin {
                join_type: join.join_type,
                relation: join_relation,
                alias: join_alias,
                condition,
                source: join_source,
                using_columns,
                using_alias: join.using_alias.clone(),
            });
        }

        for item in &select.items {
            if is_star_expr(&item.expr) {
                let qualifier = if let Expr::Identifier(name) = &item.expr {
                    if name.parts.len() >= 2 {
                        Some(name.parts[name.parts.len() - 2].clone())
                    } else {
                        None
                    }
                } else {
                    None
                };

                if let Some(ref qual) = qualifier {
                    if qual.eq_ignore_ascii_case(&effective_alias)
                        || qual.eq_ignore_ascii_case(&cte.name)
                    {
                        for column in &cte_table.columns {
                            bound.projections.push(BoundProjection {
                                alias: None,
                                expr: Expr::Identifier(ObjectName {
                                    parts: vec![qual.clone(), column.name.clone()],
                                    span: item.span,
                                }),
                            });
                        }
                        continue;
                    }

                    let mut found = false;
                    for bound_join in &bound.joins {
                        let join_alias = bound_join
                            .alias
                            .as_deref()
                            .unwrap_or_else(|| bound_join.relation.name.object_name());
                        if qual.eq_ignore_ascii_case(join_alias) {
                            for column in &bound_join.relation.columns {
                                bound.projections.push(BoundProjection {
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
                        for bound_join in &bound.joins {
                            if bound_join
                                .using_alias
                                .as_deref()
                                .is_some_and(|alias| qual.eq_ignore_ascii_case(alias))
                            {
                                for column_name in &bound_join.using_columns {
                                    bound.projections.push(BoundProjection {
                                        alias: None,
                                        expr: Expr::Identifier(ObjectName {
                                            parts: vec![column_name.clone()],
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
                        bound.projections.push(BoundProjection {
                            alias: item.alias.clone(),
                            expr: item.expr.clone(),
                        });
                    }
                } else {
                    let join_public_names = bound
                        .joins
                        .iter()
                        .map(|join| {
                            join.alias
                                .clone()
                                .unwrap_or_else(|| join.relation.name.object_name().to_owned())
                        })
                        .collect::<Vec<_>>();
                    let cte_name = if join_public_names
                        .iter()
                        .any(|name| name.eq_ignore_ascii_case(&effective_alias))
                    {
                        cte.name.clone()
                    } else {
                        effective_alias.clone()
                    };
                    if let Some(info) = &json_record_projection_info {
                        for (index, (field_name, field_type)) in info.fields.iter().enumerate() {
                            let mut field_expr = json_record_field_expr(info, index, item.span);
                            field_expr = Expr::Cast {
                                expr: Box::new(field_expr),
                                data_type: field_type.clone(),
                                span: item.span,
                            };
                            bound.projections.push(BoundProjection {
                                alias: Some(field_name.clone()),
                                expr: field_expr,
                            });
                        }
                    } else {
                        for column in &cte_table.columns {
                            bound.projections.push(BoundProjection {
                                alias: None,
                                expr: Expr::Identifier(ObjectName {
                                    parts: vec![cte_name.clone(), column.name.clone()],
                                    span: item.span,
                                }),
                            });
                        }
                    }
                    for (join_index, bound_join) in bound.joins.iter().enumerate() {
                        let join_public_name = bound_join
                            .alias
                            .clone()
                            .unwrap_or_else(|| bound_join.relation.name.object_name().to_owned());
                        let duplicates_primary =
                            join_public_name.eq_ignore_ascii_case(&effective_alias);
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
                            bound.projections.push(BoundProjection {
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
                let mut resolved_expr = item.expr.clone();
                let mut resolved_alias = item.alias.clone();
                if let Some(info) = &json_record_projection_info {
                    if let Expr::Identifier(identifier) = &item.expr {
                        if identifier.parts.len() == 1 {
                            let requested = &identifier.parts[0];
                            if let Some((field_index, (_, field_type))) = info
                                .fields
                                .iter()
                                .enumerate()
                                .find(|(_, (name, _))| name.eq_ignore_ascii_case(requested))
                            {
                                let mut field_expr =
                                    json_record_field_expr(info, field_index, item.span);
                                field_expr = Expr::Cast {
                                    expr: Box::new(field_expr),
                                    data_type: field_type.clone(),
                                    span: item.span,
                                };
                                resolved_expr = field_expr;
                                if resolved_alias.is_none() {
                                    resolved_alias = Some(requested.clone());
                                }
                            }
                        }
                    }
                }
                bound.projections.push(BoundProjection {
                    alias: resolved_alias,
                    expr: resolved_expr,
                });
            }
        }

        Ok(bound)
    }
}

#[derive(Clone, Debug)]
struct JsonPopulateRecordProjectionInfo {
    helper_name: String,
    helper_args: Vec<Expr>,
    fields: Vec<(String, DataType)>,
}

fn infer_json_populate_record_projection_info(
    cte: &aiondb_parser::CteDefinition,
) -> Option<JsonPopulateRecordProjectionInfo> {
    let Statement::Select(select) = cte.query.as_ref() else {
        return None;
    };
    if select.items.len() != 1 {
        return None;
    }
    let item = select.items.first()?;
    let Expr::FunctionCall { name, args, .. } = &item.expr else {
        return None;
    };
    let func_name = name.parts.last()?.to_ascii_lowercase();
    let helper_name = if func_name == "jsonb_populate_record" {
        "__aiondb_jsonb_populate_record"
    } else if func_name == "json_populate_record" {
        "__aiondb_json_populate_record"
    } else {
        return None;
    };
    let first_arg = args.first()?;
    let Expr::FunctionCall {
        name: hint_name,
        args: hint_args,
        ..
    } = first_arg
    else {
        return None;
    };
    if !hint_name
        .parts
        .last()
        .is_some_and(|part| part.eq_ignore_ascii_case("__aiondb_type_hint"))
    {
        return None;
    }
    let Expr::Literal(Literal::String(type_name), _) = hint_args.get(1)? else {
        return None;
    };
    let normalized = aiondb_eval::normalize_compat_type_name(type_name);
    let fields = aiondb_eval::with_current_session_context(|ctx| {
        ctx.compat_user_type(&normalized).map(|user_type| {
            user_type
                .composite_fields
                .iter()
                .map(|field| (field.name.clone(), field.data_type.clone()))
                .collect::<Vec<_>>()
        })
    })?;
    if fields.is_empty() {
        return None;
    }
    let keys_expr = Expr::Array {
        elements: fields
            .iter()
            .map(|(field_name, _)| Expr::Literal(Literal::String(field_name.clone()), item.span))
            .collect(),
        span: item.span,
    };
    let modes_expr = Expr::Array {
        elements: fields
            .iter()
            .map(|(_, data_type)| {
                let mode = match data_type {
                    DataType::Jsonb => "json",
                    DataType::Array(_) => "array",
                    _ => "scalar",
                };
                Expr::Literal(Literal::String(mode.to_owned()), item.span)
            })
            .collect(),
        span: item.span,
    };
    let mut helper_args = args.clone();
    helper_args.push(keys_expr);
    helper_args.push(modes_expr);
    Some(JsonPopulateRecordProjectionInfo {
        helper_name: helper_name.to_owned(),
        helper_args,
        fields,
    })
}

fn json_record_field_expr(
    info: &JsonPopulateRecordProjectionInfo,
    field_index: usize,
    span: aiondb_parser::Span,
) -> Expr {
    let helper_call = Expr::FunctionCall {
        name: ObjectName {
            parts: vec![info.helper_name.clone()],
            span,
        },
        args: info.helper_args.clone(),
        distinct: false,
        filter: None,
        span,
    };
    let as_text_array = Expr::Cast {
        expr: Box::new(helper_call),
        data_type: DataType::Array(Box::new(DataType::Text)),
        span,
    };
    Expr::FunctionCall {
        name: ObjectName {
            parts: vec!["array_get".to_owned()],
            span,
        },
        args: vec![
            as_text_array,
            Expr::Literal(
                Literal::Integer(i64::try_from(field_index + 1).unwrap_or(i64::MAX)),
                span,
            ),
        ],
        distinct: false,
        filter: None,
        span,
    }
}

fn rewrite_json_populate_record_source_query(statement: &Statement) -> Statement {
    let Statement::Select(select) = statement else {
        return statement.clone();
    };
    if select.items.len() != 1 {
        return statement.clone();
    }
    if let Some(info) = infer_json_populate_record_projection_info(&aiondb_parser::CteDefinition {
        name: "__inline__".to_owned(),
        column_aliases: None,
        recursive: false,
        query: Box::new(statement.clone()),
        recursive_term: None,
        union_all: false,
        span: select.span,
    }) {
        let mut rewritten = select.clone();
        rewritten.items[0].expr = Expr::FunctionCall {
            name: ObjectName {
                parts: vec![info.helper_name],
                span: rewritten.items[0].span,
            },
            args: info.helper_args,
            distinct: false,
            filter: None,
            span: rewritten.items[0].span,
        };
        return Statement::Select(rewritten);
    }
    if let Some(rewritten) = rewrite_set_returning_record_function_source(statement) {
        return rewritten;
    }
    statement.clone()
}

fn rewrite_set_returning_record_function_source(statement: &Statement) -> Option<Statement> {
    let Statement::Select(select) = statement else {
        return None;
    };
    if select.items.len() != 1 {
        return None;
    }
    let item = select.items.first()?;
    let Expr::FunctionCall { name, args, .. } = &item.expr else {
        return None;
    };
    let func_name = name.parts.last()?.to_ascii_lowercase();
    let helper_name = match func_name.as_str() {
        "jsonb_populate_recordset" => "__aiondb_jsonb_populate_recordset",
        "json_populate_recordset" => "__aiondb_json_populate_recordset",
        _ => return None,
    };
    let first_arg = args.first()?;
    let Expr::FunctionCall {
        name: hint_name,
        args: hint_args,
        ..
    } = first_arg
    else {
        return None;
    };
    if !hint_name
        .parts
        .last()
        .is_some_and(|part| part.eq_ignore_ascii_case("__aiondb_type_hint"))
    {
        return None;
    }
    let Expr::Literal(Literal::String(type_name), _) = hint_args.get(1)? else {
        return None;
    };
    let normalized = aiondb_eval::normalize_compat_type_name(type_name);
    let fields = aiondb_eval::with_current_session_context(|ctx| {
        ctx.compat_user_type(&normalized).map(|user_type| {
            user_type
                .composite_fields
                .iter()
                .map(|field| (field.name.clone(), field.data_type.clone()))
                .collect::<Vec<_>>()
        })
    })?;
    if fields.is_empty() {
        return None;
    }
    let span = item.span;
    let keys_expr = Expr::Array {
        elements: fields
            .iter()
            .map(|(field_name, _)| Expr::Literal(Literal::String(field_name.clone()), span))
            .collect(),
        span,
    };
    let modes_expr = Expr::Array {
        elements: fields
            .iter()
            .map(|(_, data_type)| {
                let mode = match data_type {
                    DataType::Jsonb => "json",
                    DataType::Array(_) => "array",
                    _ => "scalar",
                };
                Expr::Literal(Literal::String(mode.to_owned()), span)
            })
            .collect(),
        span,
    };
    let mut helper_args = args.clone();
    helper_args.push(keys_expr);
    helper_args.push(modes_expr);
    let helper_call = Expr::FunctionCall {
        name: ObjectName {
            parts: vec![helper_name.to_owned()],
            span,
        },
        args: helper_args,
        distinct: false,
        filter: None,
        span,
    };
    let row_col = "__row".to_owned();
    let rows_cte_name = "__aiondb_record_set_rows".to_owned();
    let rows_select = aiondb_parser::SelectStatement {
        row_lock: None,
        ctes: Vec::new(),
        distinct: aiondb_parser::DistinctKind::All,
        items: vec![aiondb_parser::SelectItem {
            expr: helper_call,
            alias: Some(row_col.clone()),
            span,
        }],
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
        span,
    };
    let rows_cte = aiondb_parser::CteDefinition {
        name: rows_cte_name.clone(),
        column_aliases: None,
        recursive: false,
        query: Box::new(Statement::Select(rows_select)),
        recursive_term: None,
        union_all: false,
        span,
    };
    let outer_items = fields
        .iter()
        .enumerate()
        .map(|(index, (field_name, field_type))| {
            let row_ref = Expr::Identifier(ObjectName {
                parts: vec![rows_cte_name.clone(), row_col.clone()],
                span,
            });
            let row_as_text_array = Expr::Cast {
                expr: Box::new(row_ref),
                data_type: DataType::Array(Box::new(DataType::Text)),
                span,
            };
            let pick = Expr::FunctionCall {
                name: ObjectName {
                    parts: vec!["array_get".to_owned()],
                    span,
                },
                args: vec![
                    row_as_text_array,
                    Expr::Literal(
                        Literal::Integer(i64::try_from(index + 1).unwrap_or(i64::MAX)),
                        span,
                    ),
                ],
                distinct: false,
                filter: None,
                span,
            };
            let casted = Expr::Cast {
                expr: Box::new(pick),
                data_type: field_type.clone(),
                span,
            };
            aiondb_parser::SelectItem {
                expr: casted,
                alias: Some(field_name.clone()),
                span,
            }
        })
        .collect::<Vec<_>>();
    let outer_select = aiondb_parser::SelectStatement {
        row_lock: None,
        ctes: vec![rows_cte],
        distinct: aiondb_parser::DistinctKind::All,
        items: outer_items,
        from: Some(ObjectName {
            parts: vec![rows_cte_name],
            span,
        }),
        from_alias: None,
        from_span: Some(span),
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
        span,
    };
    Some(Statement::Select(outer_select))
}

fn validate_recursive_cte_subquery_scope_in_bind_path(
    cte: &aiondb_parser::CteDefinition,
) -> DbResult<()> {
    if !cte.recursive {
        return Ok(());
    }

    if let Some(pos) =
        find_recursive_ref_in_immediate_local_ctes_in_statement(&cte.query, &cte.name)
    {
        return Err(recursive_ref_in_subquery_error(&cte.name, pos));
    }

    if let Some(recursive_term) = cte.recursive_term.as_deref() {
        if let Some(pos) =
            find_recursive_ref_in_immediate_local_ctes_in_select(recursive_term, &cte.name)
        {
            return Err(recursive_ref_in_subquery_error(&cte.name, pos));
        }
        if let Some(r#ref) = collect_recursive_refs_in_select(
            recursive_term,
            &cte.name,
            RecursiveRefContext::default(),
        )
        .iter()
        .find(|r| r.in_subquery)
        .copied()
        {
            return Err(recursive_ref_in_subquery_error(&cte.name, r#ref.position));
        }
    }

    Ok(())
}

#[derive(Clone, Copy, Default)]
struct RecursiveRefContext {
    in_subquery: bool,
    in_outer_join: bool,
    in_except: bool,
}

#[derive(Clone, Copy)]
struct RecursiveRefUse {
    position: usize,
    in_subquery: bool,
    in_outer_join: bool,
    in_except: bool,
}

pub(super) fn validate_recursive_ctes(
    binder: &Binder,
    select: &SelectStatement,
    txn_id: TxnId,
    default_schema: Option<&str>,
) -> DbResult<()> {
    if select.ctes.is_empty() {
        return Ok(());
    }

    validate_recursive_cte_dependency_cycles(select)?;

    for (cte_index, cte_ref) in select.ctes.iter().enumerate() {
        // Work on an owned snapshot so validation remains fully isolated from
        // the caller AST even on early-return error paths.
        let cte = cte_ref.clone();
        if !cte.recursive {
            continue;
        }

        let base_refs = collect_recursive_refs_in_statement(
            &cte.query,
            &cte.name,
            RecursiveRefContext::default(),
        );
        let mut union_right_has_recursive_ref = false;
        if cte.recursive_term.is_none() {
            if let Statement::SetOperation(set_op) = cte.query.as_ref() {
                if set_op.op == aiondb_parser::SetOperationType::Union {
                    if let Some(pos) = find_recursive_ref_in_used_local_ctes_in_non_recursive_term(
                        set_op.left.as_ref(),
                        &cte.name,
                    ) {
                        return Err(recursive_ref_in_non_recursive_term_error(&cte.name, pos));
                    }
                }
            }

            if let Some(pos) =
                find_recursive_ref_in_immediate_local_ctes_in_statement(&cte.query, &cte.name)
            {
                return Err(recursive_ref_in_subquery_error(&cte.name, pos));
            }

            if let Statement::SetOperation(set_op) = cte.query.as_ref() {
                if set_op.op == aiondb_parser::SetOperationType::Union {
                    if let Some(order_by) = set_op.order_by.iter().find(|item| {
                        let mut refs = Vec::new();
                        collect_recursive_refs_in_expr(
                            &item.expr,
                            &cte.name,
                            RecursiveRefContext::default(),
                            &mut refs,
                        );
                        !refs.is_empty()
                    }) {
                        return Err(DbError::bind_error(
                            SqlState::FeatureNotSupported,
                            "ORDER BY in a recursive query is not implemented",
                        )
                        .with_position(order_by.expr.span().start + 1));
                    }
                    if let Some(offset) = &set_op.offset {
                        let mut refs = Vec::new();
                        collect_recursive_refs_in_expr(
                            offset,
                            &cte.name,
                            RecursiveRefContext::default(),
                            &mut refs,
                        );
                        if !refs.is_empty() {
                            return Err(DbError::bind_error(
                                SqlState::FeatureNotSupported,
                                "OFFSET in a recursive query is not implemented",
                            )
                            .with_position(offset.span().start + 1));
                        }
                    }

                    if let Some(pos) = find_recursive_ref_in_immediate_local_ctes_in_statement(
                        set_op.left.as_ref(),
                        &cte.name,
                    ) {
                        return Err(recursive_ref_in_subquery_error(&cte.name, pos));
                    }

                    let left_refs = collect_recursive_refs_in_statement(
                        set_op.left.as_ref(),
                        &cte.name,
                        RecursiveRefContext::default(),
                    );
                    if let Some(r#ref) = left_refs.first().copied() {
                        return Err(recursive_ref_in_non_recursive_term_error(
                            &cte.name,
                            r#ref.position,
                        ));
                    }

                    if let Statement::Select(recursive_term) = set_op.right.as_ref() {
                        if let Some(pos) = find_recursive_ref_in_immediate_local_ctes_in_select(
                            recursive_term,
                            &cte.name,
                        ) {
                            return Err(recursive_ref_in_subquery_error(&cte.name, pos));
                        }

                        let recursive_refs = collect_recursive_refs_in_select(
                            recursive_term,
                            &cte.name,
                            RecursiveRefContext::default(),
                        );
                        union_right_has_recursive_ref = !recursive_refs.is_empty();
                        if let Some(r#ref) = recursive_refs.iter().find(|r| r.in_subquery).copied()
                        {
                            return Err(recursive_ref_in_subquery_error(&cte.name, r#ref.position));
                        }
                        if let Some(r#ref) =
                            recursive_refs.iter().find(|r| r.in_outer_join).copied()
                        {
                            return Err(recursive_ref_in_outer_join_error(
                                &cte.name,
                                r#ref.position,
                            ));
                        }
                        if recursive_refs.len() > 1 {
                            if let Some(r#ref) =
                                recursive_refs.iter().find(|r| r.in_except).copied()
                            {
                                return Err(recursive_ref_in_except_error(
                                    &cte.name,
                                    r#ref.position,
                                ));
                            }
                            return Err(recursive_ref_more_than_once_error(
                                &cte.name,
                                recursive_refs[1].position,
                            ));
                        }

                        if let Some(pos) = find_first_aggregate_in_select(recursive_term) {
                            return Err(DbError::bind_error(
                                SqlState::SyntaxError,
                                "aggregate functions are not allowed in a recursive query's recursive term",
                            )
                            .with_position(pos));
                        }
                        if let Some(order_by) = recursive_term.order_by.first() {
                            return Err(DbError::bind_error(
                                SqlState::FeatureNotSupported,
                                "ORDER BY in a recursive query is not implemented",
                            )
                            .with_position(order_by.expr.span().start + 1));
                        }
                        if let Some(offset) = &recursive_term.offset {
                            return Err(DbError::bind_error(
                                SqlState::FeatureNotSupported,
                                "OFFSET in a recursive query is not implemented",
                            )
                            .with_position(offset.span().start + 1));
                        }
                    }
                }
            }

            if base_refs.iter().any(|r| !r.in_subquery) {
                return Err(DbError::bind_error(
                    SqlState::SyntaxError,
                    format!(
                        "recursive query \"{}\" does not have the form non-recursive-term UNION [ALL] recursive-term",
                        cte.name
                    ),
                )
                .with_position(cte.query.span().start + 1));
            }

            if union_right_has_recursive_ref {
                validate_recursive_cte_type_rules(
                    binder,
                    select,
                    cte_index,
                    &cte,
                    txn_id,
                    default_schema,
                )?;
            }
            continue;
        }

        if let Some(pos) = find_recursive_ref_in_referenced_immediate_local_ctes_in_statement(
            &cte.query, &cte.name,
        ) {
            return Err(recursive_ref_in_non_recursive_term_error(&cte.name, pos));
        }
        if let Some(pos) =
            find_recursive_ref_in_used_local_ctes_in_non_recursive_term(&cte.query, &cte.name)
        {
            return Err(recursive_ref_in_non_recursive_term_error(&cte.name, pos));
        }
        if let Some(pos) =
            find_recursive_ref_in_immediate_local_ctes_in_statement(&cte.query, &cte.name)
        {
            return Err(recursive_ref_in_subquery_error(&cte.name, pos));
        }
        if let Some(r#ref) = base_refs.first().copied() {
            if r#ref.in_subquery && statement_directly_references_any_local_cte(&cte.query) {
                return Err(recursive_ref_in_non_recursive_term_error(
                    &cte.name,
                    r#ref.position,
                ));
            }
            if r#ref.in_subquery {
                return Err(recursive_ref_in_subquery_error(&cte.name, r#ref.position));
            }
            return Err(recursive_ref_in_non_recursive_term_error(
                &cte.name,
                r#ref.position,
            ));
        }

        let recursive_term = cte
            .recursive_term
            .as_ref()
            .ok_or_else(|| DbError::internal("recursive CTE missing recursive_term"))?;

        if parser_wrapped_recursive_term_statement(recursive_term).is_none() {
            if let Some(pos) =
                find_recursive_ref_in_immediate_local_ctes_in_select(recursive_term, &cte.name)
            {
                return Err(recursive_ref_in_subquery_error(&cte.name, pos));
            }
        }

        let recursive_refs = collect_recursive_refs_in_recursive_term(recursive_term, &cte.name);
        if let Some(r#ref) = recursive_refs.iter().find(|r| r.in_subquery).copied() {
            return Err(recursive_ref_in_subquery_error(&cte.name, r#ref.position));
        }
        if let Some(r#ref) = recursive_refs.iter().find(|r| r.in_outer_join).copied() {
            return Err(recursive_ref_in_outer_join_error(&cte.name, r#ref.position));
        }
        if recursive_refs.len() > 1 {
            if let Some(r#ref) = recursive_refs.iter().find(|r| r.in_except).copied() {
                return Err(recursive_ref_in_except_error(&cte.name, r#ref.position));
            }
            if !recursive_term_contains_set_operation(recursive_term) {
                return Err(recursive_ref_more_than_once_error(
                    &cte.name,
                    recursive_refs[1].position,
                ));
            }
        }

        if let Some(pos) = find_first_aggregate_in_select(recursive_term) {
            return Err(DbError::bind_error(
                SqlState::SyntaxError,
                "aggregate functions are not allowed in a recursive query's recursive term",
            )
            .with_position(pos));
        }
        if let Some(order_by) = recursive_term.order_by.first() {
            return Err(DbError::bind_error(
                SqlState::FeatureNotSupported,
                "ORDER BY in a recursive query is not implemented",
            )
            .with_position(order_by.expr.span().start + 1));
        }
        if let Some(offset) = &recursive_term.offset {
            return Err(DbError::bind_error(
                SqlState::FeatureNotSupported,
                "OFFSET in a recursive query is not implemented",
            )
            .with_position(offset.span().start + 1));
        }

        validate_recursive_cte_type_rules(binder, select, cte_index, &cte, txn_id, default_schema)?;
    }

    Ok(())
}

fn validate_recursive_cte_type_rules(
    binder: &Binder,
    select: &SelectStatement,
    cte_index: usize,
    cte: &aiondb_parser::CteDefinition,
    txn_id: TxnId,
    default_schema: Option<&str>,
) -> DbResult<()> {
    let (base_term, recursive_term, union_all) =
        if let Some(recursive_term) = cte.recursive_term.as_ref() {
            (cte.query.as_ref(), recursive_term.as_ref(), cte.union_all)
        } else if let Statement::SetOperation(set_op) = cte.query.as_ref() {
            if set_op.op != aiondb_parser::SetOperationType::Union {
                return Ok(());
            }
            let Statement::Select(recursive_term) = set_op.right.as_ref() else {
                return Ok(());
            };
            (set_op.left.as_ref(), recursive_term, set_op.all)
        } else {
            return Ok(());
        };

    let parent_ctes = select.ctes[..cte_index].to_vec();
    let base_statement = inject_parent_ctes(base_term, &parent_ctes);
    let base_fields =
        typed_output_fields_for_statement(binder, &base_statement, txn_id, default_schema)?;

    let recursive_select =
        build_recursive_term_with_context(select, cte_index, cte, recursive_term, &base_statement);
    if let Some((left, op, right, position)) = find_recursive_text_arithmetic_error(
        &recursive_select,
        &cte.name,
        &base_statement,
        &base_fields,
        cte.column_aliases.as_deref(),
    ) {
        return Err(DbError::Bind(Box::new(
            ErrorReport::new(
                SqlState::UndefinedFunction,
                format!("operator does not exist: {left} {op} {right}"),
            )
            .with_position(position)
            .with_client_hint(
                "No operator matches the given name and argument types. You might need to add explicit type casts.",
            ),
        )));
    }

    let recursive_statement = Statement::Select(recursive_select.clone());
    let recursive_fields =
        typed_output_fields_for_statement(binder, &recursive_statement, txn_id, default_schema)?;

    for (index, (base, recursive)) in base_fields.iter().zip(recursive_fields.iter()).enumerate() {
        if (!recursive_base_type_can_coerce_to_overall(&base.data_type, &recursive.data_type)
            && base.data_type != recursive.data_type)
            || base.text_type_modifier != recursive.text_type_modifier
        {
            return Err(DbError::Bind(Box::new(
                ErrorReport::new(
                    SqlState::DatatypeMismatch,
                    format!(
                        "recursive query \"{}\" column {} has type {} in non-recursive term but type {} overall",
                        cte.name,
                        index.saturating_add(1),
                        recursive_result_field_type_name(base),
                        recursive_result_field_type_name(recursive),
                    ),
                )
                .with_position(cte.query.span().start + 1)
                .with_client_hint("Cast the output of the non-recursive term to the correct type."),
            )));
        }
    }

    let overall_statement = Statement::SetOperation(aiondb_parser::SetOperationStatement {
        op: aiondb_parser::SetOperationType::Union,
        all: union_all,
        left: Box::new(base_statement),
        right: Box::new(recursive_statement),
        order_by: Vec::new(),
        order_by_span: None,
        limit: None,
        limit_span: None,
        offset: None,
        offset_span: None,
        span: cte.span,
    });
    let overall_fields =
        typed_output_fields_for_statement(binder, &overall_statement, txn_id, default_schema)?;

    for (index, (base, overall)) in base_fields.iter().zip(overall_fields.iter()).enumerate() {
        if (!recursive_base_type_can_coerce_to_overall(&base.data_type, &overall.data_type)
            && base.data_type != overall.data_type)
            || base.text_type_modifier != overall.text_type_modifier
        {
            return Err(DbError::Bind(Box::new(
                ErrorReport::new(
                    SqlState::DatatypeMismatch,
                    format!(
                        "recursive query \"{}\" column {} has type {} in non-recursive term but type {} overall",
                        cte.name,
                        index.saturating_add(1),
                        recursive_result_field_type_name(base),
                        recursive_result_field_type_name(overall),
                    ),
                )
                .with_position(cte.query.span().start + 1)
                .with_client_hint("Cast the output of the non-recursive term to the correct type."),
            )));
        }
    }

    Ok(())
}

fn recursive_base_type_can_coerce_to_overall(
    base_type: &aiondb_core::DataType,
    overall_type: &aiondb_core::DataType,
) -> bool {
    matches!(
        (base_type, overall_type),
        (aiondb_core::DataType::Int, aiondb_core::DataType::BigInt)
    )
}

fn typed_output_fields_for_statement(
    binder: &Binder,
    statement: &Statement,
    txn_id: TxnId,
    default_schema: Option<&str>,
) -> DbResult<Vec<aiondb_plan::ResultField>> {
    use crate::type_check::TypeChecker;

    let bound = binder.bind(statement, txn_id, default_schema)?;
    let search_path_schemas = aiondb_eval::current_search_path_schemas();
    let (current_user, session_user, current_schema, current_database) =
        aiondb_eval::with_current_session_context(|ctx| {
            (
                ctx.current_user.clone(),
                ctx.session_user.clone(),
                ctx.current_schema.clone(),
                ctx.current_database.clone(),
            )
        });
    let tc = TypeChecker::new(Arc::clone(&binder.catalog))
        .with_session_context(current_user, session_user, current_schema, current_database)
        .with_search_path_schemas(search_path_schemas);
    match bound {
        BoundStatement::Select(select) => Ok(tc
            .type_check_select(&select)?
            .outputs
            .into_iter()
            .map(|output| output.field)
            .collect()),
        BoundStatement::SetOperation(set_op) => {
            Ok(tc.type_check_set_operation(&set_op)?.output_fields)
        }
        _ => Err(DbError::feature_not_supported(
            "recursive CTE type validation requires SELECT/UNION terms",
        )),
    }
}

fn build_recursive_term_with_context(
    select: &SelectStatement,
    cte_index: usize,
    cte: &aiondb_parser::CteDefinition,
    recursive_term: &SelectStatement,
    base_query: &Statement,
) -> SelectStatement {
    let mut recursive_select = recursive_term.clone();
    let mut context_ctes = select.ctes[..cte_index].to_vec();
    context_ctes.push(aiondb_parser::CteDefinition {
        name: cte.name.clone(),
        column_aliases: cte.column_aliases.clone(),
        recursive: false,
        query: Box::new(base_query.clone()),
        recursive_term: None,
        union_all: false,
        span: cte.span,
    });
    context_ctes.extend(std::mem::take(&mut recursive_select.ctes));
    recursive_select.ctes = context_ctes;
    recursive_select
}

fn find_recursive_text_arithmetic_error(
    recursive_term: &SelectStatement,
    cte_name: &str,
    base_statement: &Statement,
    base_fields: &[aiondb_plan::ResultField],
    column_aliases: Option<&[String]>,
) -> Option<(String, &'static str, String, usize)> {
    let mut base_column_types = std::collections::HashMap::new();
    for (index, field) in base_fields.iter().enumerate() {
        base_column_types.insert(field.name.to_ascii_lowercase(), field.data_type.clone());
        if let Some(alias) = column_aliases.and_then(|aliases| aliases.get(index)) {
            base_column_types.insert(alias.to_ascii_lowercase(), field.data_type.clone());
        }
    }
    if let Statement::Select(base_select) = base_statement {
        for (index, item) in base_select.items.iter().enumerate() {
            if matches!(item.expr, Expr::Literal(Literal::String(_), _)) {
                if let Some(field) = base_fields.get(index) {
                    base_column_types.insert(field.name.to_ascii_lowercase(), DataType::Text);
                }
                if let Some(alias) = column_aliases.and_then(|aliases| aliases.get(index)) {
                    base_column_types.insert(alias.to_ascii_lowercase(), DataType::Text);
                }
            }
        }
    }

    let mut expressions: Vec<&Expr> = Vec::new();
    expressions.extend(recursive_term.items.iter().map(|item| &item.expr));
    if let Some(selection) = &recursive_term.selection {
        expressions.push(selection);
    }
    if let Some(having) = &recursive_term.having {
        expressions.push(having);
    }
    expressions.extend(recursive_term.group_by.iter());
    expressions.extend(recursive_term.order_by.iter().map(|item| &item.expr));
    if let Some(limit) = &recursive_term.limit {
        expressions.push(limit);
    }
    if let Some(offset) = &recursive_term.offset {
        expressions.push(offset);
    }
    for join in &recursive_term.joins {
        if let Some(condition) = &join.condition {
            expressions.push(condition);
        }
    }
    for window in &recursive_term.window_definitions {
        expressions.extend(window.partition_by.iter());
        expressions.extend(window.order_by.iter().map(|item| &item.expr));
    }

    expressions
        .into_iter()
        .find_map(|expr| find_text_arithmetic_in_expr(expr, cte_name, &base_column_types))
}

fn find_text_arithmetic_in_expr(
    expr: &Expr,
    cte_name: &str,
    base_column_types: &std::collections::HashMap<String, DataType>,
) -> Option<(String, &'static str, String, usize)> {
    match expr {
        Expr::BinaryOp {
            left, op, right, ..
        } => {
            if let Some(op_symbol) = arithmetic_operator_symbol(*op) {
                let left_type = recursive_operand_type_name(left, cte_name, base_column_types);
                let right_type = recursive_operand_type_name(right, cte_name, base_column_types);
                if matches!(left_type.as_deref(), Some("text"))
                    && right_type
                        .as_deref()
                        .is_some_and(is_recursive_numeric_type_name)
                {
                    return Some((
                        left_type.unwrap_or_else(|| "text".to_owned()),
                        op_symbol,
                        right_type.unwrap_or_else(|| "unknown".to_owned()),
                        right.span().start + 1,
                    ));
                }
                if matches!(right_type.as_deref(), Some("text"))
                    && left_type
                        .as_deref()
                        .is_some_and(is_recursive_numeric_type_name)
                {
                    return Some((
                        left_type.unwrap_or_else(|| "unknown".to_owned()),
                        op_symbol,
                        right_type.unwrap_or_else(|| "text".to_owned()),
                        right.span().start + 1,
                    ));
                }
            }
            find_text_arithmetic_in_expr(left, cte_name, base_column_types)
                .or_else(|| find_text_arithmetic_in_expr(right, cte_name, base_column_types))
        }
        Expr::UnaryOp { expr, .. } | Expr::IsNull { expr, .. } | Expr::Cast { expr, .. } => {
            find_text_arithmetic_in_expr(expr, cte_name, base_column_types)
        }
        Expr::FunctionCall { args, filter, .. } => args
            .iter()
            .find_map(|arg| find_text_arithmetic_in_expr(arg, cte_name, base_column_types))
            .or_else(|| {
                filter.as_ref().and_then(|expr| {
                    find_text_arithmetic_in_expr(expr, cte_name, base_column_types)
                })
            }),
        Expr::IsDistinctFrom { left, right, .. }
        | Expr::Like {
            expr: left,
            pattern: right,
            ..
        } => find_text_arithmetic_in_expr(left, cte_name, base_column_types)
            .or_else(|| find_text_arithmetic_in_expr(right, cte_name, base_column_types)),
        Expr::InList { expr, list, .. } => {
            find_text_arithmetic_in_expr(expr, cte_name, base_column_types).or_else(|| {
                list.iter().find_map(|item| {
                    find_text_arithmetic_in_expr(item, cte_name, base_column_types)
                })
            })
        }
        Expr::Between {
            expr, low, high, ..
        } => find_text_arithmetic_in_expr(expr, cte_name, base_column_types)
            .or_else(|| find_text_arithmetic_in_expr(low, cte_name, base_column_types))
            .or_else(|| find_text_arithmetic_in_expr(high, cte_name, base_column_types)),
        Expr::CaseWhen {
            operand,
            conditions,
            results,
            else_result,
            ..
        } => operand
            .as_ref()
            .and_then(|value| find_text_arithmetic_in_expr(value, cte_name, base_column_types))
            .or_else(|| {
                conditions.iter().find_map(|value| {
                    find_text_arithmetic_in_expr(value, cte_name, base_column_types)
                })
            })
            .or_else(|| {
                results.iter().find_map(|value| {
                    find_text_arithmetic_in_expr(value, cte_name, base_column_types)
                })
            })
            .or_else(|| {
                else_result.as_ref().and_then(|value| {
                    find_text_arithmetic_in_expr(value, cte_name, base_column_types)
                })
            }),
        Expr::Array { elements, .. } => elements
            .iter()
            .find_map(|value| find_text_arithmetic_in_expr(value, cte_name, base_column_types)),
        Expr::WindowFunction {
            function,
            partition_by,
            order_by,
            ..
        } => find_text_arithmetic_in_expr(function, cte_name, base_column_types)
            .or_else(|| {
                partition_by.iter().find_map(|value| {
                    find_text_arithmetic_in_expr(value, cte_name, base_column_types)
                })
            })
            .or_else(|| {
                order_by.iter().find_map(|item| {
                    find_text_arithmetic_in_expr(&item.expr, cte_name, base_column_types)
                })
            }),
        Expr::ArraySubquery { query, .. }
        | Expr::Subquery { query, .. }
        | Expr::Exists { query, .. } => query
            .items
            .iter()
            .find_map(|item| find_text_arithmetic_in_expr(&item.expr, cte_name, base_column_types)),
        Expr::InSubquery { expr, query, .. } => {
            find_text_arithmetic_in_expr(expr, cte_name, base_column_types).or_else(|| {
                query.items.iter().find_map(|item| {
                    find_text_arithmetic_in_expr(&item.expr, cte_name, base_column_types)
                })
            })
        }
        Expr::Literal(_, _)
        | Expr::Identifier(_)
        | Expr::Parameter { .. }
        | Expr::Default { .. }
        | Expr::CypherExists { .. }
        | Expr::CypherPatternComprehension { .. } => None,
    }
}

fn arithmetic_operator_symbol(op: aiondb_parser::BinaryOperator) -> Option<&'static str> {
    match op {
        aiondb_parser::BinaryOperator::Add => Some("+"),
        aiondb_parser::BinaryOperator::Sub => Some("-"),
        aiondb_parser::BinaryOperator::Mul => Some("*"),
        aiondb_parser::BinaryOperator::Div => Some("/"),
        aiondb_parser::BinaryOperator::Mod => Some("%"),
        _ => None,
    }
}

fn recursive_operand_type_name(
    expr: &Expr,
    cte_name: &str,
    base_column_types: &std::collections::HashMap<String, DataType>,
) -> Option<String> {
    match expr {
        Expr::Identifier(name) => {
            let column = name.parts.last()?.to_ascii_lowercase();
            if name.parts.len() >= 2 {
                let qualifier = &name.parts[name.parts.len().saturating_sub(2)];
                if !qualifier.eq_ignore_ascii_case(cte_name) {
                    return None;
                }
            }
            base_column_types
                .get(&column)
                .map(|data_type| data_type.pg_type_name().to_owned())
        }
        Expr::Literal(Literal::Integer(_), _) => Some("integer".to_owned()),
        Expr::Literal(Literal::NumericLit(_), _) => Some("numeric".to_owned()),
        Expr::Literal(Literal::String(_), _) => Some("text".to_owned()),
        Expr::Literal(Literal::Boolean(_), _) => Some("boolean".to_owned()),
        Expr::Literal(Literal::Null, _) => None,
        Expr::Cast { data_type, .. } => Some(data_type.pg_type_name().to_owned()),
        _ => None,
    }
}

fn is_recursive_numeric_type_name(type_name: &str) -> bool {
    matches!(
        type_name,
        "smallint" | "integer" | "bigint" | "numeric" | "real" | "double precision"
    )
}

fn recursive_result_field_type_name(field: &aiondb_plan::ResultField) -> String {
    match (&field.data_type, field.text_type_modifier) {
        (DataType::Text, Some(modifier)) => modifier.pg_display_name().to_string(),
        (data_type, _) => data_type.pg_type_name().to_owned(),
    }
}

fn validate_recursive_cte_dependency_cycles(select: &SelectStatement) -> DbResult<()> {
    use std::collections::{HashMap, HashSet};

    let mut name_to_idx = HashMap::new();
    for (idx, cte) in select.ctes.iter().enumerate() {
        name_to_idx.insert(cte.name.to_ascii_lowercase(), idx);
    }

    let mut deps: Vec<HashSet<usize>> = vec![HashSet::new(); select.ctes.len()];
    for (idx, cte) in select.ctes.iter().enumerate() {
        collect_cte_dependencies_in_statement(&cte.query, &name_to_idx, &mut deps[idx]);
        if let Some(term) = &cte.recursive_term {
            collect_cte_dependencies_in_select(term, &name_to_idx, &mut deps[idx]);
        }
        deps[idx].remove(&idx);
    }

    for (idx, cte) in select.ctes.iter().enumerate() {
        let mut visited = HashSet::new();
        let has_cycle = deps[idx]
            .iter()
            .copied()
            .any(|dep| path_reaches(dep, idx, &deps, &mut visited));
        if has_cycle {
            return Err(DbError::bind_error(
                SqlState::FeatureNotSupported,
                "mutual recursion between WITH items is not implemented",
            )
            .with_position(cte.span.start + 1));
        }
    }

    Ok(())
}

fn path_reaches(
    start: usize,
    target: usize,
    deps: &[std::collections::HashSet<usize>],
    visited: &mut std::collections::HashSet<usize>,
) -> bool {
    if start == target {
        return true;
    }
    if !visited.insert(start) {
        return false;
    }
    deps[start]
        .iter()
        .copied()
        .any(|next| path_reaches(next, target, deps, visited))
}

fn collect_cte_dependencies_in_statement(
    statement: &Statement,
    names: &std::collections::HashMap<String, usize>,
    deps: &mut std::collections::HashSet<usize>,
) {
    match statement {
        Statement::Select(select) => collect_cte_dependencies_in_select(select, names, deps),
        Statement::Insert(insert) => {
            if let Some(query) = &insert.query {
                collect_cte_dependencies_in_select(query, names, deps);
            }
            for row in &insert.rows {
                for expr in row {
                    collect_cte_dependencies_in_expr(expr, names, deps);
                }
            }
            for item in &insert.returning {
                collect_cte_dependencies_in_expr(&item.expr, names, deps);
            }
            if let Some(on_conflict) = &insert.on_conflict {
                match &on_conflict.action {
                    aiondb_parser::OnConflictAction::DoNothing => {}
                    aiondb_parser::OnConflictAction::DoUpdate {
                        assignments,
                        where_clause,
                    } => {
                        for assignment in assignments {
                            collect_cte_dependencies_in_expr(&assignment.expr, names, deps);
                        }
                        if let Some(where_clause) = where_clause {
                            collect_cte_dependencies_in_expr(where_clause, names, deps);
                        }
                    }
                }
            }
        }
        Statement::Update(update) => {
            for assignment in &update.assignments {
                collect_cte_dependencies_in_expr(&assignment.expr, names, deps);
            }
            if let Some(selection) = &update.selection {
                collect_cte_dependencies_in_expr(selection, names, deps);
            }
            for item in &update.returning {
                collect_cte_dependencies_in_expr(&item.expr, names, deps);
            }
        }
        Statement::Delete(delete) => {
            if let Some(selection) = &delete.selection {
                collect_cte_dependencies_in_expr(selection, names, deps);
            }
            for item in &delete.returning {
                collect_cte_dependencies_in_expr(&item.expr, names, deps);
            }
        }
        Statement::Merge(merge) => {
            if let aiondb_parser::MergeSource::Subquery(query) = &merge.source {
                collect_cte_dependencies_in_select(query, names, deps);
            }
            collect_cte_dependencies_in_expr(&merge.on_condition, names, deps);
            for when in &merge.when_clauses {
                if let Some(condition) = &when.condition {
                    collect_cte_dependencies_in_expr(condition, names, deps);
                }
                match &when.action {
                    aiondb_parser::MergeAction::Update { assignments } => {
                        for assignment in assignments {
                            collect_cte_dependencies_in_expr(&assignment.expr, names, deps);
                        }
                    }
                    aiondb_parser::MergeAction::Insert { values, .. } => {
                        for value in values {
                            collect_cte_dependencies_in_expr(value, names, deps);
                        }
                    }
                    aiondb_parser::MergeAction::Delete
                    | aiondb_parser::MergeAction::InsertDefaultValues
                    | aiondb_parser::MergeAction::DoNothing => {}
                }
            }
        }
        Statement::Copy(copy) => {
            if let Some(query) = &copy.query {
                collect_cte_dependencies_in_statement(query, names, deps);
            }
        }
        Statement::SetOperation(set_op) => {
            collect_cte_dependencies_in_statement(&set_op.left, names, deps);
            collect_cte_dependencies_in_statement(&set_op.right, names, deps);
            for item in &set_op.order_by {
                collect_cte_dependencies_in_expr(&item.expr, names, deps);
            }
            if let Some(limit) = &set_op.limit {
                collect_cte_dependencies_in_expr(limit, names, deps);
            }
            if let Some(offset) = &set_op.offset {
                collect_cte_dependencies_in_expr(offset, names, deps);
            }
        }
        Statement::Explain { statement, .. } => {
            collect_cte_dependencies_in_statement(statement, names, deps);
        }
        _ => {}
    }
}

fn collect_cte_dependencies_in_select(
    select: &SelectStatement,
    names: &std::collections::HashMap<String, usize>,
    deps: &mut std::collections::HashSet<usize>,
) {
    if let Some(from) = &select.from {
        if from.parts.len() == 1 {
            if let Some(idx) = names.get(&from.parts[0].to_ascii_lowercase()) {
                deps.insert(*idx);
            }
        }
    }
    for join in &select.joins {
        if join.table.parts.len() == 1 {
            if let Some(idx) = names.get(&join.table.parts[0].to_ascii_lowercase()) {
                deps.insert(*idx);
            }
        }
        if let Some(condition) = &join.condition {
            collect_cte_dependencies_in_expr(condition, names, deps);
        }
    }
    for cte in &select.ctes {
        collect_cte_dependencies_in_statement(&cte.query, names, deps);
        if let Some(term) = &cte.recursive_term {
            collect_cte_dependencies_in_select(term, names, deps);
        }
    }
    for item in &select.items {
        collect_cte_dependencies_in_expr(&item.expr, names, deps);
    }
    if let Some(selection) = &select.selection {
        collect_cte_dependencies_in_expr(selection, names, deps);
    }
    for expr in &select.group_by {
        collect_cte_dependencies_in_expr(expr, names, deps);
    }
    if let Some(having) = &select.having {
        collect_cte_dependencies_in_expr(having, names, deps);
    }
    for window in &select.window_definitions {
        for expr in &window.partition_by {
            collect_cte_dependencies_in_expr(expr, names, deps);
        }
        for item in &window.order_by {
            collect_cte_dependencies_in_expr(&item.expr, names, deps);
        }
    }
    for order in &select.order_by {
        collect_cte_dependencies_in_expr(&order.expr, names, deps);
    }
    if let Some(limit) = &select.limit {
        collect_cte_dependencies_in_expr(limit, names, deps);
    }
    if let Some(offset) = &select.offset {
        collect_cte_dependencies_in_expr(offset, names, deps);
    }
}

fn collect_cte_dependencies_in_expr(
    expr: &Expr,
    names: &std::collections::HashMap<String, usize>,
    deps: &mut std::collections::HashSet<usize>,
) {
    match expr {
        Expr::InSubquery { expr, query, .. } => {
            collect_cte_dependencies_in_expr(expr, names, deps);
            collect_cte_dependencies_in_select(query, names, deps);
        }
        Expr::ArraySubquery { query, .. }
        | Expr::Subquery { query, .. }
        | Expr::Exists { query, .. } => {
            collect_cte_dependencies_in_select(query, names, deps);
        }
        Expr::FunctionCall { args, filter, .. } => {
            for arg in args {
                collect_cte_dependencies_in_expr(arg, names, deps);
            }
            if let Some(filter) = filter {
                collect_cte_dependencies_in_expr(filter, names, deps);
            }
        }
        Expr::UnaryOp { expr, .. } | Expr::IsNull { expr, .. } | Expr::Cast { expr, .. } => {
            collect_cte_dependencies_in_expr(expr, names, deps);
        }
        Expr::BinaryOp { left, right, .. } | Expr::IsDistinctFrom { left, right, .. } => {
            collect_cte_dependencies_in_expr(left, names, deps);
            collect_cte_dependencies_in_expr(right, names, deps);
        }
        Expr::Like { expr, pattern, .. } => {
            collect_cte_dependencies_in_expr(expr, names, deps);
            collect_cte_dependencies_in_expr(pattern, names, deps);
        }
        Expr::InList { expr, list, .. } => {
            collect_cte_dependencies_in_expr(expr, names, deps);
            for item in list {
                collect_cte_dependencies_in_expr(item, names, deps);
            }
        }
        Expr::Between {
            expr, low, high, ..
        } => {
            collect_cte_dependencies_in_expr(expr, names, deps);
            collect_cte_dependencies_in_expr(low, names, deps);
            collect_cte_dependencies_in_expr(high, names, deps);
        }
        Expr::CaseWhen {
            operand,
            conditions,
            results,
            else_result,
            ..
        } => {
            if let Some(operand) = operand {
                collect_cte_dependencies_in_expr(operand, names, deps);
            }
            for condition in conditions {
                collect_cte_dependencies_in_expr(condition, names, deps);
            }
            for result in results {
                collect_cte_dependencies_in_expr(result, names, deps);
            }
            if let Some(else_result) = else_result {
                collect_cte_dependencies_in_expr(else_result, names, deps);
            }
        }
        Expr::Array { elements, .. } => {
            for element in elements {
                collect_cte_dependencies_in_expr(element, names, deps);
            }
        }
        Expr::WindowFunction {
            function,
            partition_by,
            order_by,
            ..
        } => {
            collect_cte_dependencies_in_expr(function, names, deps);
            for expr in partition_by {
                collect_cte_dependencies_in_expr(expr, names, deps);
            }
            for item in order_by {
                collect_cte_dependencies_in_expr(&item.expr, names, deps);
            }
        }
        Expr::Literal(_, _)
        | Expr::Parameter { .. }
        | Expr::Identifier(_)
        | Expr::Default { .. }
        | Expr::CypherExists { .. }
        | Expr::CypherPatternComprehension { .. } => {}
    }
}

fn collect_recursive_refs_in_statement(
    statement: &Statement,
    cte_name: &str,
    ctx: RecursiveRefContext,
) -> Vec<RecursiveRefUse> {
    let mut refs = Vec::new();
    collect_recursive_refs_in_statement_mut(statement, cte_name, ctx, &mut refs);
    refs
}

fn collect_recursive_refs_in_statement_mut(
    statement: &Statement,
    cte_name: &str,
    ctx: RecursiveRefContext,
    refs: &mut Vec<RecursiveRefUse>,
) {
    match statement {
        Statement::Select(select) => {
            collect_recursive_refs_in_select_mut(select, cte_name, ctx, refs)
        }
        Statement::SetOperation(set_op) => {
            let branch_ctx = RecursiveRefContext {
                in_except: ctx.in_except
                    || matches!(set_op.op, aiondb_parser::SetOperationType::Except),
                ..ctx
            };
            collect_recursive_refs_in_statement_mut(&set_op.left, cte_name, branch_ctx, refs);
            collect_recursive_refs_in_statement_mut(&set_op.right, cte_name, branch_ctx, refs);
            for item in &set_op.order_by {
                collect_recursive_refs_in_expr(&item.expr, cte_name, ctx, refs);
            }
            if let Some(limit) = &set_op.limit {
                collect_recursive_refs_in_expr(limit, cte_name, ctx, refs);
            }
            if let Some(offset) = &set_op.offset {
                collect_recursive_refs_in_expr(offset, cte_name, ctx, refs);
            }
        }
        _ => {}
    }
}

fn collect_recursive_refs_in_select(
    select: &SelectStatement,
    cte_name: &str,
    ctx: RecursiveRefContext,
) -> Vec<RecursiveRefUse> {
    let mut refs = Vec::new();
    collect_recursive_refs_in_select_mut(select, cte_name, ctx, &mut refs);
    refs
}

fn collect_recursive_refs_in_recursive_term(
    select: &SelectStatement,
    cte_name: &str,
) -> Vec<RecursiveRefUse> {
    if let Some(wrapped_statement) = parser_wrapped_recursive_term_statement(select) {
        collect_recursive_refs_in_statement(
            wrapped_statement,
            cte_name,
            RecursiveRefContext::default(),
        )
    } else {
        collect_recursive_refs_in_select(select, cte_name, RecursiveRefContext::default())
    }
}

fn recursive_term_contains_set_operation(select: &SelectStatement) -> bool {
    let mut stack = vec![select];
    while let Some(select) = stack.pop() {
        for cte in &select.ctes {
            match cte.query.as_ref() {
                Statement::SetOperation(_) => return true,
                Statement::Select(select) => stack.push(select),
                _ => {}
            }
        }
    }
    false
}

fn parser_wrapped_recursive_term_statement(select: &SelectStatement) -> Option<&Statement> {
    let from = select.from.as_ref()?;
    if from.parts.len() != 1 || select.ctes.len() != 1 {
        return None;
    }
    let local_cte = &select.ctes[0];
    if !local_cte.name.eq_ignore_ascii_case(&from.parts[0]) {
        return None;
    }
    if local_cte.name != "__aiondb_recursive_term" {
        return None;
    }
    Some(local_cte.query.as_ref())
}

fn collect_recursive_refs_in_select_mut(
    select: &SelectStatement,
    cte_name: &str,
    ctx: RecursiveRefContext,
    refs: &mut Vec<RecursiveRefUse>,
) {
    // Inner WITH items shadow outer names for this SELECT scope.
    if select
        .ctes
        .iter()
        .any(|cte| cte.name.eq_ignore_ascii_case(cte_name))
    {
        return;
    }

    let left_side_outer = select.from.as_ref().is_some_and(|_| {
        select
            .joins
            .iter()
            .any(|j| matches!(j.join_type, AstJoinType::Right | AstJoinType::Full))
    });
    if let Some(from) = &select.from {
        maybe_push_recursive_ref(
            from,
            cte_name,
            RecursiveRefContext {
                in_outer_join: ctx.in_outer_join || left_side_outer,
                ..ctx
            },
            refs,
        );
    }

    for join in &select.joins {
        let join_outer = matches!(
            join.join_type,
            AstJoinType::Left | AstJoinType::Right | AstJoinType::Full
        );
        let join_ctx = RecursiveRefContext {
            in_outer_join: ctx.in_outer_join || join_outer,
            ..ctx
        };
        maybe_push_recursive_ref(&join.table, cte_name, join_ctx, refs);
        if let Some(condition) = &join.condition {
            collect_recursive_refs_in_expr(condition, cte_name, join_ctx, refs);
        }
    }

    for cte in &select.ctes {
        let nested_ctx = RecursiveRefContext {
            in_subquery: true,
            ..ctx
        };
        collect_recursive_refs_in_statement_mut(&cte.query, cte_name, nested_ctx, refs);
        if let Some(term) = &cte.recursive_term {
            collect_recursive_refs_in_select_mut(term, cte_name, nested_ctx, refs);
        }
    }
    for item in &select.items {
        collect_recursive_refs_in_expr(&item.expr, cte_name, ctx, refs);
    }
    if let Some(selection) = &select.selection {
        collect_recursive_refs_in_expr(selection, cte_name, ctx, refs);
    }
    for expr in &select.group_by {
        collect_recursive_refs_in_expr(expr, cte_name, ctx, refs);
    }
    if let Some(having) = &select.having {
        collect_recursive_refs_in_expr(having, cte_name, ctx, refs);
    }
    for window in &select.window_definitions {
        for expr in &window.partition_by {
            collect_recursive_refs_in_expr(expr, cte_name, ctx, refs);
        }
        for item in &window.order_by {
            collect_recursive_refs_in_expr(&item.expr, cte_name, ctx, refs);
        }
    }
    for item in &select.order_by {
        collect_recursive_refs_in_expr(&item.expr, cte_name, ctx, refs);
    }
    if let Some(limit) = &select.limit {
        collect_recursive_refs_in_expr(limit, cte_name, ctx, refs);
    }
    if let Some(offset) = &select.offset {
        collect_recursive_refs_in_expr(offset, cte_name, ctx, refs);
    }
}

fn collect_recursive_refs_in_expr(
    expr: &Expr,
    cte_name: &str,
    ctx: RecursiveRefContext,
    refs: &mut Vec<RecursiveRefUse>,
) {
    match expr {
        Expr::ArraySubquery { query, .. }
        | Expr::Subquery { query, .. }
        | Expr::Exists { query, .. } => {
            collect_recursive_refs_in_select_mut(
                query,
                cte_name,
                RecursiveRefContext {
                    in_subquery: true,
                    ..ctx
                },
                refs,
            );
        }
        Expr::InSubquery { expr, query, .. } => {
            collect_recursive_refs_in_expr(expr, cte_name, ctx, refs);
            collect_recursive_refs_in_select_mut(
                query,
                cte_name,
                RecursiveRefContext {
                    in_subquery: true,
                    ..ctx
                },
                refs,
            );
        }
        Expr::FunctionCall { args, filter, .. } => {
            for arg in args {
                collect_recursive_refs_in_expr(arg, cte_name, ctx, refs);
            }
            if let Some(filter) = filter {
                collect_recursive_refs_in_expr(filter, cte_name, ctx, refs);
            }
        }
        Expr::UnaryOp { expr, .. } | Expr::IsNull { expr, .. } | Expr::Cast { expr, .. } => {
            collect_recursive_refs_in_expr(expr, cte_name, ctx, refs);
        }
        Expr::BinaryOp { left, right, .. } | Expr::IsDistinctFrom { left, right, .. } => {
            collect_recursive_refs_in_expr(left, cte_name, ctx, refs);
            collect_recursive_refs_in_expr(right, cte_name, ctx, refs);
        }
        Expr::Like { expr, pattern, .. } => {
            collect_recursive_refs_in_expr(expr, cte_name, ctx, refs);
            collect_recursive_refs_in_expr(pattern, cte_name, ctx, refs);
        }
        Expr::InList { expr, list, .. } => {
            collect_recursive_refs_in_expr(expr, cte_name, ctx, refs);
            for item in list {
                collect_recursive_refs_in_expr(item, cte_name, ctx, refs);
            }
        }
        Expr::Between {
            expr, low, high, ..
        } => {
            collect_recursive_refs_in_expr(expr, cte_name, ctx, refs);
            collect_recursive_refs_in_expr(low, cte_name, ctx, refs);
            collect_recursive_refs_in_expr(high, cte_name, ctx, refs);
        }
        Expr::CaseWhen {
            operand,
            conditions,
            results,
            else_result,
            ..
        } => {
            if let Some(operand) = operand {
                collect_recursive_refs_in_expr(operand, cte_name, ctx, refs);
            }
            for condition in conditions {
                collect_recursive_refs_in_expr(condition, cte_name, ctx, refs);
            }
            for result in results {
                collect_recursive_refs_in_expr(result, cte_name, ctx, refs);
            }
            if let Some(else_result) = else_result {
                collect_recursive_refs_in_expr(else_result, cte_name, ctx, refs);
            }
        }
        Expr::Array { elements, .. } => {
            for element in elements {
                collect_recursive_refs_in_expr(element, cte_name, ctx, refs);
            }
        }
        Expr::WindowFunction {
            function,
            partition_by,
            order_by,
            ..
        } => {
            collect_recursive_refs_in_expr(function, cte_name, ctx, refs);
            for expr in partition_by {
                collect_recursive_refs_in_expr(expr, cte_name, ctx, refs);
            }
            for item in order_by {
                collect_recursive_refs_in_expr(&item.expr, cte_name, ctx, refs);
            }
        }
        Expr::Literal(_, _)
        | Expr::Identifier(_)
        | Expr::Parameter { .. }
        | Expr::Default { .. }
        | Expr::CypherExists { .. }
        | Expr::CypherPatternComprehension { .. } => {}
    }
}

fn maybe_push_recursive_ref(
    name: &ObjectName,
    cte_name: &str,
    ctx: RecursiveRefContext,
    refs: &mut Vec<RecursiveRefUse>,
) {
    if name.parts.len() == 1 && name.parts[0].eq_ignore_ascii_case(cte_name) {
        refs.push(RecursiveRefUse {
            position: name.span.start + 1,
            in_subquery: ctx.in_subquery,
            in_outer_join: ctx.in_outer_join,
            in_except: ctx.in_except,
        });
    }
}

fn find_recursive_ref_in_immediate_local_ctes_in_statement(
    statement: &Statement,
    cte_name: &str,
) -> Option<usize> {
    match statement {
        Statement::Select(select) => {
            find_recursive_ref_in_immediate_local_ctes_in_select(select, cte_name)
        }
        Statement::SetOperation(set_op) => {
            find_recursive_ref_in_immediate_local_ctes_in_statement(&set_op.left, cte_name).or_else(
                || find_recursive_ref_in_immediate_local_ctes_in_statement(&set_op.right, cte_name),
            )
        }
        _ => None,
    }
}

fn find_recursive_ref_in_referenced_immediate_local_ctes_in_statement(
    statement: &Statement,
    cte_name: &str,
) -> Option<usize> {
    match statement {
        Statement::Select(select) => {
            find_recursive_ref_in_referenced_immediate_local_ctes_in_select(select, cte_name)
        }
        Statement::SetOperation(set_op) => {
            find_recursive_ref_in_referenced_immediate_local_ctes_in_statement(
                &set_op.left,
                cte_name,
            )
            .or_else(|| {
                find_recursive_ref_in_referenced_immediate_local_ctes_in_statement(
                    &set_op.right,
                    cte_name,
                )
            })
        }
        _ => None,
    }
}

fn find_recursive_ref_in_referenced_immediate_local_ctes_in_select(
    select: &SelectStatement,
    cte_name: &str,
) -> Option<usize> {
    for local_cte in &select.ctes {
        if !select_directly_references_local_cte(select, &local_cte.name) {
            continue;
        }

        let refs = collect_recursive_refs_in_statement(
            &local_cte.query,
            cte_name,
            RecursiveRefContext::default(),
        );
        if let Some(r#ref) = refs.first() {
            return Some(r#ref.position);
        }
        if let Some(term) = &local_cte.recursive_term {
            let refs =
                collect_recursive_refs_in_select(term, cte_name, RecursiveRefContext::default());
            if let Some(r#ref) = refs.first() {
                return Some(r#ref.position);
            }
        }
    }
    None
}

fn select_directly_references_local_cte(select: &SelectStatement, local_cte_name: &str) -> bool {
    let matches_name = |name: &ObjectName| {
        name.parts.len() == 1 && name.parts[0].eq_ignore_ascii_case(local_cte_name)
    };

    select.from.as_ref().is_some_and(matches_name)
        || select.joins.iter().any(|join| matches_name(&join.table))
}

fn statement_directly_references_any_local_cte(statement: &Statement) -> bool {
    match statement {
        Statement::Select(select) => select
            .ctes
            .iter()
            .any(|local_cte| select_directly_references_local_cte(select, &local_cte.name)),
        Statement::SetOperation(set_op) => {
            statement_directly_references_any_local_cte(set_op.left.as_ref())
                || statement_directly_references_any_local_cte(set_op.right.as_ref())
        }
        _ => false,
    }
}

fn find_recursive_ref_in_used_local_ctes_in_non_recursive_term(
    statement: &Statement,
    cte_name: &str,
) -> Option<usize> {
    match statement {
        Statement::Select(select) => {
            let mut used_local_ctes = std::collections::HashSet::new();
            if let Some(from) = &select.from {
                if from.parts.len() == 1 {
                    used_local_ctes.insert(from.parts[0].to_ascii_lowercase());
                }
            }
            for join in &select.joins {
                if join.table.parts.len() == 1 {
                    used_local_ctes.insert(join.table.parts[0].to_ascii_lowercase());
                }
            }

            for local_cte in &select.ctes {
                if !used_local_ctes.contains(&local_cte.name.to_ascii_lowercase()) {
                    continue;
                }
                let refs = collect_recursive_refs_in_statement(
                    &local_cte.query,
                    cte_name,
                    RecursiveRefContext::default(),
                );
                if let Some(r#ref) = refs.first() {
                    return Some(r#ref.position);
                }
                if let Some(term) = &local_cte.recursive_term {
                    let refs = collect_recursive_refs_in_select(
                        term,
                        cte_name,
                        RecursiveRefContext::default(),
                    );
                    if let Some(r#ref) = refs.first() {
                        return Some(r#ref.position);
                    }
                }
            }
            None
        }
        Statement::SetOperation(set_op) => {
            find_recursive_ref_in_used_local_ctes_in_non_recursive_term(&set_op.left, cte_name)
                .or_else(|| {
                    find_recursive_ref_in_used_local_ctes_in_non_recursive_term(
                        &set_op.right,
                        cte_name,
                    )
                })
        }
        _ => None,
    }
}

fn find_recursive_ref_in_immediate_local_ctes_in_select(
    select: &SelectStatement,
    cte_name: &str,
) -> Option<usize> {
    for local_cte in &select.ctes {
        let refs = collect_recursive_refs_in_statement(
            &local_cte.query,
            cte_name,
            RecursiveRefContext::default(),
        );
        if let Some(r#ref) = refs.first() {
            return Some(r#ref.position);
        }
        if let Some(term) = &local_cte.recursive_term {
            let refs =
                collect_recursive_refs_in_select(term, cte_name, RecursiveRefContext::default());
            if let Some(r#ref) = refs.first() {
                return Some(r#ref.position);
            }
        }
    }
    None
}

fn find_first_aggregate_in_select(select: &SelectStatement) -> Option<usize> {
    for item in &select.items {
        if let Some(pos) = find_first_aggregate_in_expr(&item.expr) {
            return Some(pos);
        }
    }
    if let Some(selection) = &select.selection {
        if let Some(pos) = find_first_aggregate_in_expr(selection) {
            return Some(pos);
        }
    }
    if let Some(having) = &select.having {
        if let Some(pos) = find_first_aggregate_in_expr(having) {
            return Some(pos);
        }
    }
    for expr in &select.group_by {
        if let Some(pos) = find_first_aggregate_in_expr(expr) {
            return Some(pos);
        }
    }
    for item in &select.order_by {
        if let Some(pos) = find_first_aggregate_in_expr(&item.expr) {
            return Some(pos);
        }
    }
    None
}

fn find_first_aggregate_in_expr(expr: &Expr) -> Option<usize> {
    match expr {
        Expr::FunctionCall {
            name,
            args,
            filter,
            span,
            ..
        } => {
            let function_name = name.parts.last().map_or("", String::as_str);
            if is_aggregate_function_name(function_name) {
                return Some(span.start + 1);
            }
            for arg in args {
                if let Some(pos) = find_first_aggregate_in_expr(arg) {
                    return Some(pos);
                }
            }
            if let Some(filter) = filter {
                return find_first_aggregate_in_expr(filter);
            }
            None
        }
        Expr::UnaryOp { expr, .. } | Expr::IsNull { expr, .. } | Expr::Cast { expr, .. } => {
            find_first_aggregate_in_expr(expr)
        }
        Expr::BinaryOp { left, right, .. } | Expr::IsDistinctFrom { left, right, .. } => {
            find_first_aggregate_in_expr(left).or_else(|| find_first_aggregate_in_expr(right))
        }
        Expr::Like { expr, pattern, .. } => {
            find_first_aggregate_in_expr(expr).or_else(|| find_first_aggregate_in_expr(pattern))
        }
        Expr::InList { expr, list, .. } => find_first_aggregate_in_expr(expr)
            .or_else(|| list.iter().find_map(find_first_aggregate_in_expr)),
        Expr::Between {
            expr, low, high, ..
        } => find_first_aggregate_in_expr(expr)
            .or_else(|| find_first_aggregate_in_expr(low))
            .or_else(|| find_first_aggregate_in_expr(high)),
        Expr::CaseWhen {
            operand,
            conditions,
            results,
            else_result,
            ..
        } => operand
            .as_deref()
            .and_then(find_first_aggregate_in_expr)
            .or_else(|| conditions.iter().find_map(find_first_aggregate_in_expr))
            .or_else(|| results.iter().find_map(find_first_aggregate_in_expr))
            .or_else(|| {
                else_result
                    .as_deref()
                    .and_then(find_first_aggregate_in_expr)
            }),
        Expr::Array { elements, .. } => elements.iter().find_map(find_first_aggregate_in_expr),
        Expr::WindowFunction {
            function,
            partition_by,
            order_by,
            ..
        } => find_first_aggregate_in_expr(function)
            .or_else(|| partition_by.iter().find_map(find_first_aggregate_in_expr))
            .or_else(|| {
                order_by
                    .iter()
                    .find_map(|item| find_first_aggregate_in_expr(&item.expr))
            }),
        Expr::ArraySubquery { .. }
        | Expr::Subquery { .. }
        | Expr::InSubquery { .. }
        | Expr::Exists { .. }
        | Expr::CypherExists { .. }
        | Expr::CypherPatternComprehension { .. }
        | Expr::Literal(_, _)
        | Expr::Identifier(_)
        | Expr::Parameter { .. }
        | Expr::Default { .. } => None,
    }
}

fn is_aggregate_function_name(name: &str) -> bool {
    matches!(
        name.to_ascii_lowercase().as_str(),
        "count"
            | "sum"
            | "avg"
            | "min"
            | "max"
            | "string_agg"
            | "array_agg"
            | "bool_and"
            | "bool_or"
            | "every"
            | "variance"
            | "stddev"
            | "stddev_pop"
            | "stddev_samp"
            | "var_pop"
            | "var_samp"
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

fn recursive_ref_in_non_recursive_term_error(name: &str, position: usize) -> DbError {
    DbError::bind_error(
        SqlState::SyntaxError,
        format!(
            "recursive reference to query \"{name}\" must not appear within its non-recursive term"
        ),
    )
    .with_position(position)
}

fn recursive_ref_in_subquery_error(name: &str, position: usize) -> DbError {
    DbError::bind_error(
        SqlState::SyntaxError,
        format!("recursive reference to query \"{name}\" must not appear within a subquery"),
    )
    .with_position(position)
}

fn recursive_ref_in_outer_join_error(name: &str, position: usize) -> DbError {
    DbError::bind_error(
        SqlState::SyntaxError,
        format!("recursive reference to query \"{name}\" must not appear within an outer join"),
    )
    .with_position(position)
}

fn recursive_ref_more_than_once_error(name: &str, position: usize) -> DbError {
    DbError::bind_error(
        SqlState::SyntaxError,
        format!("recursive reference to query \"{name}\" must not appear more than once"),
    )
    .with_position(position)
}

fn recursive_ref_in_except_error(name: &str, position: usize) -> DbError {
    DbError::bind_error(
        SqlState::SyntaxError,
        format!("recursive reference to query \"{name}\" must not appear within EXCEPT"),
    )
    .with_position(position)
}

#[cfg(test)]
mod tests {
    use super::*;
    use aiondb_parser::parse_prepared_statement;
    use std::sync::Arc;

    #[test]
    fn recursive_outer_join_error_does_not_poison_following_bind() {
        let binder = Binder::new(Arc::new(crate::EmptyCatalog));

        let failing = parse_prepared_statement(
            "WITH RECURSIVE x(n) AS ( \
                 SELECT a FROM y WHERE a = 1 \
                 UNION ALL \
                 SELECT x.n + 1 FROM y LEFT JOIN x ON x.n = y.a WHERE n < 10 \
             ) \
             SELECT * FROM x",
        )
        .expect("parse failing recursive statement");
        let err = binder
            .bind(&failing, TxnId::default(), None)
            .expect_err("expected recursive outer-join validation error");
        assert!(
            err.to_string().contains(
                "recursive reference to query \"x\" must not appear within an outer join"
            ),
            "unexpected error: {err}"
        );

        // leak state into subsequent statements.
        let succeeding = parse_prepared_statement(
            "WITH RECURSIVE t(n) AS ( \
                 VALUES (1) \
                 UNION ALL \
                 SELECT n + 1 FROM t WHERE n < 3 \
             ) \
             SELECT * FROM t",
        )
        .expect("parse succeeding recursive statement");
        let bound = binder
            .bind(&succeeding, TxnId::default(), None)
            .expect("bind should still succeed after prior error");
        assert!(matches!(bound, BoundStatement::Select(_)));
    }
}

pub(super) fn parent_ctes_without_self(
    parent_ctes: &[aiondb_parser::CteDefinition],
    cte_name: &str,
) -> Vec<aiondb_parser::CteDefinition> {
    if let Some((self_idx, self_cte)) = parent_ctes
        .iter()
        .enumerate()
        .find(|(_, cte)| cte.name.eq_ignore_ascii_case(cte_name))
    {
        if self_cte.recursive
            || matches!(
                self_cte.query.as_ref(),
                Statement::Insert(_) | Statement::Update(_) | Statement::Delete(_)
            )
        {
            // WITH RECURSIVE permits forward references between sibling CTEs,
            // but only when they are actually referenced by this CTE. Injecting
            // all siblings can leak unrelated data-modifying CTEs into the scope.
            let mut result = parent_ctes[..self_idx].to_vec();
            let name_to_idx: std::collections::HashMap<String, usize> = parent_ctes
                .iter()
                .enumerate()
                .map(|(idx, cte)| (cte.name.to_ascii_lowercase(), idx))
                .collect();
            let mut deps = std::collections::HashSet::new();
            collect_cte_dependencies_in_statement(&self_cte.query, &name_to_idx, &mut deps);
            if let Some(term) = &self_cte.recursive_term {
                collect_cte_dependencies_in_select(term, &name_to_idx, &mut deps);
            }

            let mut forward_idxs: Vec<usize> =
                deps.into_iter().filter(|idx| *idx > self_idx).collect();
            forward_idxs.sort_unstable();
            for idx in forward_idxs {
                result.push(parent_ctes[idx].clone());
            }
            return result;
        }
        return parent_ctes[..self_idx].to_vec();
    }

    parent_ctes.to_vec()
}

/// Find a CTE definition by matching its name against an `ObjectName` (unqualified).
pub(super) fn find_cte<'a>(
    ctes: &'a [aiondb_parser::CteDefinition],
    name: &ObjectName,
) -> Option<&'a aiondb_parser::CteDefinition> {
    if name.parts.len() != 1 {
        return None;
    }
    let target = &name.parts[0];
    ctes.iter()
        .find(|cte| cte.name.eq_ignore_ascii_case(target))
}

/// Build a map from CTE output column name -> inner projection expression.
/// This includes both the original column names and any column_aliases.
/// Also registers qualified names (cte_name\0col) for two-part references.
#[allow(dead_code)]
fn build_cte_column_map(
    projections: &[BoundProjection],
    column_aliases: &Option<Vec<String>>,
    cte_name: &str,
) -> Vec<(String, Expr)> {
    let mut map = Vec::new();
    for (i, proj) in projections.iter().enumerate() {
        // The inner projection's "name" is its alias or the identifier name.
        let inner_name = proj
            .alias
            .clone()
            .or_else(|| identifier_name(&proj.expr))
            .unwrap_or_else(|| format!("?column{i}?"));
        map.push((inner_name.clone(), proj.expr.clone()));
        // Register qualified form: cte_name\0col_name
        map.push((format!("{}\x00{}", cte_name, inner_name), proj.expr.clone()));
        // If there are column_aliases, also register the alias
        if let Some(aliases) = column_aliases {
            if let Some(alias) = aliases.get(i) {
                if !map.iter().any(|(n, _)| n.eq_ignore_ascii_case(alias)) {
                    map.push((alias.clone(), proj.expr.clone()));
                }
                // Also register qualified form for the alias
                let qualified_alias = format!("{}\x00{}", cte_name, alias);
                if !map
                    .iter()
                    .any(|(n, _)| n.eq_ignore_ascii_case(&qualified_alias))
                {
                    map.push((qualified_alias, proj.expr.clone()));
                }
            }
        }
    }
    // For single-column CTEs (typically from function calls like generate_series),
    // also register the CTE name itself as a column name.  In PostgreSQL,
    // `FROM generate_series(1,10) AS i` allows `SELECT i` to reference the
    // single output column.
    if projections.len() == 1 {
        let expr = &projections[0].expr;
        if !map.iter().any(|(n, _)| n.eq_ignore_ascii_case(cte_name)) {
            map.push((cte_name.to_owned(), expr.clone()));
        }
    }
    map
}

#[allow(dead_code)]
fn build_cte_identity_column_map(
    table: &TableDescriptor,
    cte_name: &str,
    span: Span,
) -> Vec<(String, Expr)> {
    let mut map = Vec::with_capacity(table.columns.len() * 2 + 1);
    for column in &table.columns {
        let expr = Expr::Identifier(ObjectName {
            parts: vec![column.name.clone()],
            span,
        });
        map.push((column.name.clone(), expr.clone()));
        map.push((format!("{}\x00{}", cte_name, column.name), expr));
    }
    if table.columns.len() == 1
        && !map
            .iter()
            .any(|(name, _)| name.eq_ignore_ascii_case(cte_name))
    {
        map.push((
            cte_name.to_owned(),
            Expr::Identifier(ObjectName {
                parts: vec![table.columns[0].name.clone()],
                span,
            }),
        ));
    }
    map
}

/// Extract the simple identifier name from an expression, if it is one.
#[allow(dead_code)]
fn identifier_name(expr: &Expr) -> Option<String> {
    if let Expr::Identifier(name) = expr {
        if name.parts.len() == 1 {
            return Some(name.parts[0].clone());
        }
    }
    None
}

/// If `expr` is a simple identifier, return its name for use as a
/// projection alias.  This preserves the CTE column name in the output.
#[allow(dead_code)]
fn cte_alias_for(expr: &Expr) -> Option<String> {
    identifier_name(expr)
}

/// Recursively rewrite column identifier references that match a CTE column
/// name to the underlying inner expression.  Handles both unqualified (`x`)
/// and qualified (`v.x`) references where `v` is the CTE/alias name.
#[allow(dead_code)]
fn rewrite_cte_refs(expr: &Expr, column_map: &[(String, Expr)]) -> Expr {
    match expr {
        // Unqualified identifier: look up directly in column_map
        Expr::Identifier(name) if name.parts.len() == 1 => {
            let col = &name.parts[0];
            for (cte_name, inner_expr) in column_map {
                if cte_name.eq_ignore_ascii_case(col) {
                    return inner_expr.clone();
                }
            }
            expr.clone()
        }
        // Qualified identifier (e.g. v.x): look up "table\0col" key in the map
        Expr::Identifier(name) if name.parts.len() == 2 => {
            let qualified = format!("{}\x00{}", name.parts[0], name.parts[1]);
            for (cte_name, inner_expr) in column_map {
                if cte_name.eq_ignore_ascii_case(&qualified) {
                    return inner_expr.clone();
                }
            }
            // Also try just the column part (last element) for cases where
            // the qualifier is the CTE alias and the column is in the map
            let col = &name.parts[1];
            for (cte_name, inner_expr) in column_map {
                if cte_name.eq_ignore_ascii_case(col) {
                    return inner_expr.clone();
                }
            }
            expr.clone()
        }
        // Recurse into compound expressions
        Expr::FunctionCall {
            name,
            args,
            distinct,
            filter,
            span,
        } => Expr::FunctionCall {
            name: name.clone(),
            args: args
                .iter()
                .map(|a| rewrite_cte_refs(a, column_map))
                .collect(),
            distinct: *distinct,
            filter: filter
                .as_ref()
                .map(|f| Box::new(rewrite_cte_refs(f, column_map))),
            span: *span,
        },
        Expr::UnaryOp { op, expr: e, span } => Expr::UnaryOp {
            op: *op,
            expr: Box::new(rewrite_cte_refs(e, column_map)),
            span: *span,
        },
        Expr::BinaryOp {
            left,
            op,
            right,
            span,
        } => Expr::BinaryOp {
            left: Box::new(rewrite_cte_refs(left, column_map)),
            op: *op,
            right: Box::new(rewrite_cte_refs(right, column_map)),
            span: *span,
        },
        Expr::IsNull {
            expr: e,
            negated,
            span,
        } => Expr::IsNull {
            expr: Box::new(rewrite_cte_refs(e, column_map)),
            negated: *negated,
            span: *span,
        },
        Expr::IsDistinctFrom {
            left,
            right,
            negated,
            span,
        } => Expr::IsDistinctFrom {
            left: Box::new(rewrite_cte_refs(left, column_map)),
            right: Box::new(rewrite_cte_refs(right, column_map)),
            negated: *negated,
            span: *span,
        },
        Expr::Like {
            expr: e,
            pattern,
            negated,
            case_insensitive,
            span,
        } => Expr::Like {
            expr: Box::new(rewrite_cte_refs(e, column_map)),
            pattern: Box::new(rewrite_cte_refs(pattern, column_map)),
            negated: *negated,
            case_insensitive: *case_insensitive,
            span: *span,
        },
        Expr::InList {
            expr: e,
            list,
            negated,
            span,
        } => Expr::InList {
            expr: Box::new(rewrite_cte_refs(e, column_map)),
            list: list
                .iter()
                .map(|l| rewrite_cte_refs(l, column_map))
                .collect(),
            negated: *negated,
            span: *span,
        },
        Expr::Between {
            expr: e,
            low,
            high,
            negated,
            span,
        } => Expr::Between {
            expr: Box::new(rewrite_cte_refs(e, column_map)),
            low: Box::new(rewrite_cte_refs(low, column_map)),
            high: Box::new(rewrite_cte_refs(high, column_map)),
            negated: *negated,
            span: *span,
        },
        Expr::Cast {
            expr: e,
            data_type,
            span,
        } => Expr::Cast {
            expr: Box::new(rewrite_cte_refs(e, column_map)),
            data_type: data_type.clone(),
            span: *span,
        },
        Expr::CaseWhen {
            operand,
            conditions,
            results,
            else_result,
            span,
        } => Expr::CaseWhen {
            operand: operand
                .as_ref()
                .map(|o| Box::new(rewrite_cte_refs(o, column_map))),
            conditions: conditions
                .iter()
                .map(|c| rewrite_cte_refs(c, column_map))
                .collect(),
            results: results
                .iter()
                .map(|r| rewrite_cte_refs(r, column_map))
                .collect(),
            else_result: else_result
                .as_ref()
                .map(|e| Box::new(rewrite_cte_refs(e, column_map))),
            span: *span,
        },
        Expr::Array { elements, span } => Expr::Array {
            elements: elements
                .iter()
                .map(|e| rewrite_cte_refs(e, column_map))
                .collect(),
            span: *span,
        },
        Expr::InSubquery {
            expr: e,
            query,
            negated,
            span,
        } => Expr::InSubquery {
            expr: Box::new(rewrite_cte_refs(e, column_map)),
            query: query.clone(),
            negated: *negated,
            span: *span,
        },
        Expr::WindowFunction {
            function,
            partition_by,
            order_by,
            window_name,
            span,
        } => Expr::WindowFunction {
            function: Box::new(rewrite_cte_refs(function, column_map)),
            partition_by: partition_by
                .iter()
                .map(|p| rewrite_cte_refs(p, column_map))
                .collect(),
            order_by: order_by.clone(),
            window_name: window_name.clone(),
            span: *span,
        },
        // Leaves: literals, parameters, defaults, subqueries, exists - no rewriting
        _ => expr.clone(),
    }
}

/// Like `rewrite_cte_refs`, but only rewrites unqualified (1-part) identifier
/// references.  Qualified references (e.g. `emp.dept_id`) are left intact so
/// that `rewrite_table_aliases` can handle them against the combined relation.
/// This is used for join ON conditions where the condition may mix CTE alias
/// references with table-qualified column references.
#[allow(dead_code)]
fn rewrite_cte_refs_unqualified_only(expr: &Expr, column_map: &[(String, Expr)]) -> Expr {
    match expr {
        Expr::Identifier(name) if name.parts.len() == 1 => {
            let col = &name.parts[0];
            for (cte_name, inner_expr) in column_map {
                if !cte_name.contains('\0') && cte_name.eq_ignore_ascii_case(col) {
                    return inner_expr.clone();
                }
            }
            expr.clone()
        }
        // Leave qualified identifiers (2+ parts) untouched
        Expr::Identifier(_) => expr.clone(),
        // Recurse into compound expressions
        Expr::BinaryOp {
            left,
            op,
            right,
            span,
        } => Expr::BinaryOp {
            left: Box::new(rewrite_cte_refs_unqualified_only(left, column_map)),
            op: *op,
            right: Box::new(rewrite_cte_refs_unqualified_only(right, column_map)),
            span: *span,
        },
        Expr::UnaryOp { op, expr: e, span } => Expr::UnaryOp {
            op: *op,
            expr: Box::new(rewrite_cte_refs_unqualified_only(e, column_map)),
            span: *span,
        },
        Expr::IsNull {
            expr: e,
            negated,
            span,
        } => Expr::IsNull {
            expr: Box::new(rewrite_cte_refs_unqualified_only(e, column_map)),
            negated: *negated,
            span: *span,
        },
        Expr::FunctionCall {
            name,
            args,
            distinct,
            filter,
            span,
        } => Expr::FunctionCall {
            name: name.clone(),
            args: args
                .iter()
                .map(|a| rewrite_cte_refs_unqualified_only(a, column_map))
                .collect(),
            distinct: *distinct,
            filter: filter
                .as_ref()
                .map(|f| Box::new(rewrite_cte_refs_unqualified_only(f, column_map))),
            span: *span,
        },
        Expr::InList {
            expr: e,
            list,
            negated,
            span,
        } => Expr::InList {
            expr: Box::new(rewrite_cte_refs_unqualified_only(e, column_map)),
            list: list
                .iter()
                .map(|l| rewrite_cte_refs_unqualified_only(l, column_map))
                .collect(),
            negated: *negated,
            span: *span,
        },
        Expr::Between {
            expr: e,
            low,
            high,
            negated,
            span,
        } => Expr::Between {
            expr: Box::new(rewrite_cte_refs_unqualified_only(e, column_map)),
            low: Box::new(rewrite_cte_refs_unqualified_only(low, column_map)),
            high: Box::new(rewrite_cte_refs_unqualified_only(high, column_map)),
            negated: *negated,
            span: *span,
        },
        Expr::Cast {
            expr: e,
            data_type,
            span,
        } => Expr::Cast {
            expr: Box::new(rewrite_cte_refs_unqualified_only(e, column_map)),
            data_type: data_type.clone(),
            span: *span,
        },
        _ => expr.clone(),
    }
}

/// Inject parent CTEs into a CTE's inner query so that references to
/// sibling/parent CTEs (e.g. `WITH v(...) ... FROM v AS v1(x1), v AS v2(x2)`)
/// can be resolved inside wrapper CTEs.
pub(super) fn inject_parent_ctes(
    query: &aiondb_parser::Statement,
    parent_ctes: &[aiondb_parser::CteDefinition],
) -> aiondb_parser::Statement {
    fn with_parent_ctes_for_select(
        select: &aiondb_parser::SelectStatement,
        parent_ctes: &[aiondb_parser::CteDefinition],
    ) -> aiondb_parser::SelectStatement {
        if select.ctes.is_empty() {
            let mut select = select.clone();
            select.ctes = parent_ctes.to_vec();
            select
        } else {
            // Preserve lexical shadowing: local CTE names must win over outer ones.
            let mut select = select.clone();
            let mut all = select.ctes;
            all.extend(parent_ctes.iter().cloned());
            select.ctes = all;
            select
        }
    }

    if parent_ctes.is_empty() {
        return query.clone();
    }
    match query {
        Statement::Select(s) => Statement::Select(with_parent_ctes_for_select(s, parent_ctes)),
        Statement::SetOperation(set_op) => {
            let mut set_op = set_op.clone();
            set_op.left = Box::new(inject_parent_ctes(&set_op.left, parent_ctes));
            set_op.right = Box::new(inject_parent_ctes(&set_op.right, parent_ctes));
            Statement::SetOperation(set_op)
        }
        Statement::Insert(insert) => {
            let mut insert = insert.clone();
            if let Some(query) = insert.query.as_mut() {
                *query = with_parent_ctes_for_select(query, parent_ctes);
            }
            Statement::Insert(insert)
        }
        Statement::Merge(merge) => {
            let mut merge = merge.clone();
            if let aiondb_parser::MergeSource::Subquery(query) = &mut merge.source {
                **query = with_parent_ctes_for_select(query, parent_ctes);
            }
            Statement::Merge(merge)
        }
        Statement::Copy(copy) => {
            let mut copy = copy.clone();
            if let Some(inner) = copy.query.as_mut() {
                **inner = inject_parent_ctes(inner, parent_ctes);
            }
            Statement::Copy(copy)
        }
        _ => query.clone(),
    }
}

/// Build a synthetic `TableDescriptor` from a CTE's bound output columns.
/// If the CTE defines column aliases, they override the inner query's names.
#[allow(dead_code)]
pub(super) fn build_cte_table_descriptor(
    cte: &aiondb_parser::CteDefinition,
    binder: &Binder,
    txn_id: TxnId,
    default_schema: Option<&str>,
) -> DbResult<TableDescriptor> {
    build_cte_table_descriptor_with_outer_and_ctes(
        cte,
        binder,
        txn_id,
        default_schema,
        Vec::new(),
        &[],
    )
}

/// Build a CTE table descriptor, passing parent CTEs for resolution.
#[allow(dead_code)]
pub(super) fn build_cte_table_descriptor_with_parent_ctes(
    cte: &aiondb_parser::CteDefinition,
    binder: &Binder,
    txn_id: TxnId,
    default_schema: Option<&str>,
    parent_ctes: &[aiondb_parser::CteDefinition],
) -> DbResult<TableDescriptor> {
    build_cte_table_descriptor_with_outer_and_ctes(
        cte,
        binder,
        txn_id,
        default_schema,
        Vec::new(),
        parent_ctes,
    )
}

#[allow(dead_code)]
pub(super) fn build_cte_table_descriptor_with_outer(
    cte: &aiondb_parser::CteDefinition,
    binder: &Binder,
    txn_id: TxnId,
    default_schema: Option<&str>,
    outer_columns: Vec<ColumnDescriptor>,
) -> DbResult<TableDescriptor> {
    build_cte_table_descriptor_with_outer_and_ctes(
        cte,
        binder,
        txn_id,
        default_schema,
        outer_columns,
        &[],
    )
}

pub(super) fn build_cte_table_descriptor_with_outer_and_ctes(
    cte: &aiondb_parser::CteDefinition,
    binder: &Binder,
    txn_id: TxnId,
    default_schema: Option<&str>,
    outer_columns: Vec<ColumnDescriptor>,
    parent_ctes: &[aiondb_parser::CteDefinition],
) -> DbResult<TableDescriptor> {
    let effective_query = inject_parent_ctes(&cte.query, parent_ctes);
    let bound = binder.bind(&effective_query, txn_id, default_schema)?;
    build_cte_table_descriptor_from_bound(cte, binder, &bound, outer_columns)
}

pub(super) fn build_cte_table_descriptor_from_bound(
    cte: &aiondb_parser::CteDefinition,
    binder: &Binder,
    bound: &BoundStatement,
    outer_columns: Vec<ColumnDescriptor>,
) -> DbResult<TableDescriptor> {
    use crate::type_check::TypeChecker;

    let search_path_schemas = aiondb_eval::current_search_path_schemas();
    let (current_user, session_user, current_schema, current_database) =
        aiondb_eval::with_current_session_context(|ctx| {
            (
                ctx.current_user.clone(),
                ctx.session_user.clone(),
                ctx.current_schema.clone(),
                ctx.current_database.clone(),
            )
        });
    let tc = if outer_columns.is_empty() {
        TypeChecker::new(Arc::clone(&binder.catalog))
            .with_session_context(
                current_user.clone(),
                session_user.clone(),
                current_schema.clone(),
                current_database.clone(),
            )
            .with_search_path_schemas(Arc::clone(&search_path_schemas))
    } else {
        TypeChecker::new(Arc::clone(&binder.catalog))
            .with_session_context(current_user, session_user, current_schema, current_database)
            .with_search_path_schemas(search_path_schemas)
            .with_outer_columns(outer_columns)
    };
    let (output_fields, table_id, schema_id) = match bound {
        BoundStatement::Select(select) => {
            let typed = tc.type_check_select(select)?;
            (
                typed
                    .outputs
                    .into_iter()
                    .map(|output| output.field)
                    .collect::<Vec<_>>(),
                select
                    .relation
                    .as_ref()
                    .map_or(RelationId::new(0), |relation| relation.table_id),
                select
                    .relation
                    .as_ref()
                    .map_or(aiondb_core::SchemaId::new(0), |relation| relation.schema_id),
            )
        }
        BoundStatement::SetOperation(set_op) => {
            let typed = tc.type_check_set_operation(set_op)?;
            (
                typed.output_fields,
                RelationId::new(0),
                aiondb_core::SchemaId::new(0),
            )
        }
        BoundStatement::Insert(insert) => {
            // CTE wrapping INSERT ... RETURNING: type-check the INSERT to
            // extract the RETURNING output fields.
            if insert.returning.is_empty() {
                return Err(DbError::feature_not_supported(format!(
                    "WITH query \"{}\" does not have a RETURNING clause",
                    cte.name
                )));
            }
            let typed = tc.type_check_insert(insert)?;
            (
                typed
                    .returning
                    .into_iter()
                    .map(|p| p.field)
                    .collect::<Vec<_>>(),
                insert.relation.table_id,
                insert.relation.schema_id,
            )
        }
        BoundStatement::Update(update) => {
            if update.returning.is_empty() {
                return Err(DbError::feature_not_supported(format!(
                    "WITH query \"{}\" does not have a RETURNING clause",
                    cte.name
                )));
            }
            let typed = tc.type_check_update(update)?;
            (
                typed
                    .returning
                    .into_iter()
                    .map(|p| p.field)
                    .collect::<Vec<_>>(),
                update.relation.table_id,
                update.relation.schema_id,
            )
        }
        BoundStatement::Delete(delete) => {
            if delete.returning.is_empty() {
                return Err(DbError::feature_not_supported(format!(
                    "WITH query \"{}\" does not have a RETURNING clause",
                    cte.name
                )));
            }
            let typed = tc.type_check_delete(delete)?;
            (
                typed
                    .returning
                    .into_iter()
                    .map(|p| p.field)
                    .collect::<Vec<_>>(),
                delete.relation.table_id,
                delete.relation.schema_id,
            )
        }
        _ => {
            return Err(DbError::feature_not_supported(
                "CTE in FROM must resolve to a SELECT, set operation, or data-modifying statement",
            ));
        }
    };

    if let Some(aliases) = &cte.column_aliases {
        if aliases.len() > output_fields.len() {
            return Err(DbError::bind_error(
                SqlState::SyntaxError,
                format!(
                    "WITH query \"{}\" has {} columns available but {} columns specified",
                    cte.name,
                    output_fields.len(),
                    aliases.len()
                ),
            )
            .with_position(cte.span.start + 1));
        }
    }

    let columns = output_fields
        .iter()
        .enumerate()
        .map(|(i, output)| {
            let visible_name = output
                .name
                .rsplit('\0')
                .next()
                .unwrap_or(&output.name)
                .to_owned();
            let name = if let Some(aliases) = &cte.column_aliases {
                aliases
                    .get(i)
                    .cloned()
                    .unwrap_or_else(|| visible_name.clone())
            } else {
                visible_name
            };
            ColumnDescriptor {
                column_id: aiondb_core::ColumnId::new(
                    u64::try_from(i).unwrap_or(u64::MAX).saturating_add(1),
                ),
                name,
                data_type: output.data_type.clone(),
                raw_type_name: None,
                text_type_modifier: None,
                nullable: output.nullable,
                ordinal_position: u32::try_from(i).unwrap_or(u32::MAX).saturating_add(1),
                default_value: None,
            }
        })
        .collect();

    Ok(TableDescriptor {
        table_id,
        schema_id,
        name: QualifiedName::unqualified(&cte.name),
        columns,
        primary_key: None,
        foreign_keys: Vec::new(),
        check_constraints: Vec::new(),
        shard_config: None,
        identity_columns: Vec::new(),
        owner: None,
    })
}

fn fallback_internal_from_function_cte_table_descriptor(
    cte: &aiondb_parser::CteDefinition,
) -> Option<TableDescriptor> {
    let Statement::Select(select) = cte.query.as_ref() else {
        return None;
    };
    if !select.joins.is_empty()
        || select.selection.is_some()
        || !select.group_by.is_empty()
        || select.having.is_some()
        || !select.order_by.is_empty()
        || select.limit.is_some()
        || select.offset.is_some()
    {
        return None;
    }
    if let [item] = select.items.as_slice() {
        if select.from.is_none() && select.ctes.is_empty() {
            let Expr::FunctionCall { name, .. } = &item.expr else {
                return None;
            };

            let function_name = name.parts.join(".");
            let info = aiondb_eval::FunctionRegistry::lookup(&function_name)?;
            let visible_name = item
                .alias
                .clone()
                .or_else(|| name.parts.last().cloned())
                .unwrap_or_else(|| "?column?".to_owned());
            let column_name = cte
                .column_aliases
                .as_ref()
                .and_then(|aliases| aliases.first().cloned())
                .unwrap_or(visible_name);

            return Some(TableDescriptor {
                table_id: RelationId::new(0),
                schema_id: aiondb_core::SchemaId::new(0),
                name: QualifiedName::unqualified(&cte.name),
                columns: vec![ColumnDescriptor {
                    column_id: aiondb_core::ColumnId::new(1),
                    name: column_name,
                    data_type: info.return_type,
                    raw_type_name: None,
                    text_type_modifier: None,
                    nullable: true,
                    ordinal_position: 1,
                    default_value: None,
                }],
                primary_key: None,
                foreign_keys: Vec::new(),
                check_constraints: Vec::new(),
                shard_config: None,
                identity_columns: Vec::new(),
                owner: None,
            });
        }
    }

    let columns = select
        .items
        .iter()
        .enumerate()
        .map(|(index, item)| {
            let name = cte
                .column_aliases
                .as_ref()
                .and_then(|aliases| aliases.get(index).cloned())
                .or_else(|| item.alias.clone())?;
            let data_type = infer_fallback_cte_item_type(&item.expr)?;
            Some(ColumnDescriptor {
                column_id: aiondb_core::ColumnId::new(
                    u64::try_from(index).unwrap_or(u64::MAX).saturating_add(1),
                ),
                name,
                data_type,
                raw_type_name: None,
                text_type_modifier: None,
                nullable: true,
                ordinal_position: u32::try_from(index).unwrap_or(u32::MAX).saturating_add(1),
                default_value: None,
            })
        })
        .collect::<Option<Vec<_>>>()?;
    if columns.is_empty() {
        return None;
    }

    Some(TableDescriptor {
        table_id: RelationId::new(0),
        schema_id: aiondb_core::SchemaId::new(0),
        name: QualifiedName::unqualified(&cte.name),
        columns,
        primary_key: None,
        foreign_keys: Vec::new(),
        check_constraints: Vec::new(),
        shard_config: None,
        identity_columns: Vec::new(),
        owner: None,
    })
}

fn infer_fallback_cte_item_type(expr: &Expr) -> Option<DataType> {
    match expr {
        Expr::Cast { data_type, .. } => Some(data_type.clone()),
        Expr::FunctionCall { name, .. } => {
            let function_name = name.parts.join(".");
            aiondb_eval::FunctionRegistry::lookup(&function_name).map(|info| info.return_type)
        }
        Expr::Literal(literal, _) => match literal {
            Literal::Null => Some(DataType::Text),
            Literal::Boolean(_) => Some(DataType::Boolean),
            Literal::Integer(_) => Some(DataType::Int),
            Literal::NumericLit(_) => Some(DataType::Numeric),
            Literal::String(_) => Some(DataType::Text),
        },
        _ => None,
    }
}

pub(super) fn build_outer_scope_columns(
    primary_relation: Option<&TableDescriptor>,
    primary_alias: Option<&str>,
    joins: &[BoundJoin],
) -> Vec<ColumnDescriptor> {
    let mut columns = Vec::new();
    let mut alias_entries: Vec<(String, usize)> = Vec::new();

    if let Some(relation) = primary_relation {
        let relation_name = relation.name.object_name().to_owned();
        alias_entries.push((relation_name.clone(), columns.len()));
        if let Some(alias) = primary_alias.map(str::to_owned) {
            if !alias.eq_ignore_ascii_case(&relation_name) {
                alias_entries.push((alias, columns.len()));
            }
        }
        append_scope_columns(&mut columns, &relation.columns);
    }

    for join in joins {
        let relation_name = join.relation.name.object_name().to_owned();
        alias_entries.push((relation_name.clone(), columns.len()));
        if let Some(alias) = join.alias.clone() {
            if !alias.eq_ignore_ascii_case(&relation_name) {
                alias_entries.push((alias, columns.len()));
            }
        }
        append_scope_columns(&mut columns, &join.relation.columns);
    }

    let base_len = columns.len();
    for (idx, (alias, start)) in alias_entries.iter().enumerate() {
        let end = alias_entries
            .iter()
            .skip(idx + 1)
            .find(|(_, next_start)| *next_start > *start)
            .map_or(base_len, |(_, next_start)| *next_start);
        let qualified_columns: Vec<ColumnDescriptor> = columns[*start..end]
            .iter()
            .map(|col| {
                let bare_name = col.name.rsplit('\0').next().unwrap_or(&col.name);
                ColumnDescriptor {
                    column_id: col.column_id,
                    name: format!("{alias}\x00{bare_name}"),
                    data_type: col.data_type.clone(),
                    raw_type_name: None,
                    text_type_modifier: col.text_type_modifier,
                    nullable: col.nullable,
                    ordinal_position: col.ordinal_position,
                    default_value: col.default_value.clone(),
                }
            })
            .collect();
        for col in qualified_columns {
            columns.push(col);
        }
    }

    columns
}

fn append_scope_columns(out: &mut Vec<ColumnDescriptor>, source: &[ColumnDescriptor]) {
    for col in source {
        out.push(ColumnDescriptor {
            column_id: col.column_id,
            name: col.name.clone(),
            data_type: col.data_type.clone(),
            raw_type_name: None,
            text_type_modifier: col.text_type_modifier,
            nullable: col.nullable,
            ordinal_position: u32::try_from(out.len())
                .unwrap_or(u32::MAX)
                .saturating_add(1),
            default_value: col.default_value.clone(),
        });
    }
}
