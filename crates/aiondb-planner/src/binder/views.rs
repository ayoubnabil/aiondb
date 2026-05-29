#![allow(clippy::map_unwrap_or, clippy::assigning_clones, clippy::clone_on_copy)]

use super::*;
use aiondb_catalog::CheckConstraint;
use aiondb_parser::ast::ViewCheckOptionClause;
use aiondb_parser::identifier::is_system_column_name;
mod view_sql;
use self::view_sql::{format_expr, reconstruct_select_sql};

impl Binder {
    pub(super) fn bind_create_view(
        &self,
        create_view: &CreateViewStatement,
        txn_id: TxnId,
        default_schema: Option<&str>,
    ) -> DbResult<BoundCreateView> {
        let view_name =
            qualified_create_view_name(&create_view.name, default_schema, create_view.temporary)?;
        // Bind the query to validate it (for column type inference)
        let query = self.bind_select(&create_view.query, txn_id, default_schema)?;
        let stored_query = canonicalize_select_for_view_storage(
            self.catalog.as_ref(),
            txn_id,
            &create_view.query,
            default_schema,
        )?;
        // Use override_sql if present (for complex query forms like multi-row VALUES),
        // otherwise reconstruct from the AST.
        let query_sql = if let Some(ref sql) = create_view.override_sql {
            sql.clone()
        } else {
            reconstruct_select_sql(&stored_query)
        };
        let check_option = match create_view.check_option {
            Some(ViewCheckOptionClause::Local) => Some(ViewCheckOption::Local),
            Some(ViewCheckOptionClause::Cascaded) => Some(ViewCheckOption::Cascaded),
            None => None,
        };
        Ok(BoundCreateView {
            view_name,
            query_sql,
            creation_search_path_schemas: aiondb_eval::current_search_path_schemas()
                .as_ref()
                .clone(),
            query,
            or_replace: create_view.or_replace,
            column_aliases: create_view.column_aliases.clone(),
            check_option,
        })
    }

    pub(super) fn bind_drop_view(
        &self,
        drop_view: &DropViewStatement,
        txn_id: TxnId,
        default_schema: Option<&str>,
    ) -> DbResult<BoundDropView> {
        let view_error_name = relation_error_name(&drop_view.name, default_schema)?;
        let (_, view) = resolve_view_in_search_path(
            self.catalog.as_ref(),
            txn_id,
            &drop_view.name,
            default_schema,
        )?
        .ok_or_else(|| undefined_view(&view_error_name))?;
        Ok(BoundDropView { view })
    }

    pub(super) fn bind_view_select(
        &self,
        select: &SelectStatement,
        view: &ViewDescriptor,
        txn_id: TxnId,
        default_schema: Option<&str>,
    ) -> DbResult<BoundSelect> {
        let depth = self.view_depth.load(std::sync::atomic::Ordering::Relaxed);
        if depth >= 64 {
            return Err(DbError::internal(
                "view recursion depth limit exceeded (possible circular view definition)",
            ));
        }
        self.view_depth
            .store(depth + 1, std::sync::atomic::Ordering::Relaxed);
        let result = self.bind_view_select_inner(select, view, txn_id, default_schema);
        self.view_depth
            .store(depth, std::sync::atomic::Ordering::Relaxed);
        result
    }

    fn bind_view_select_inner(
        &self,
        select: &SelectStatement,
        view: &ViewDescriptor,
        txn_id: TxnId,
        default_schema: Option<&str>,
    ) -> DbResult<BoundSelect> {
        // Parse and bind the underlying view query
        let view_stmts = aiondb_parser::parse_sql(&view.query_sql)?;
        let first_stmt = view_stmts
            .into_iter()
            .next()
            .ok_or_else(|| DbError::internal("view query SQL is empty"))?;

        // If the view is a set operation, extract the leftmost SELECT
        // for binding (this handles multi-row VALUES views).
        let first_stmt = match first_stmt {
            Statement::SetOperation(ref set_op) => {
                // Extract leftmost SELECT from set operation chain
                fn leftmost_select(stmt: &Statement) -> Option<&SelectStatement> {
                    match stmt {
                        Statement::Select(s) => Some(s),
                        Statement::SetOperation(set_op) => leftmost_select(&set_op.left),
                        _ => None,
                    }
                }
                if let Some(sel) = leftmost_select(&Statement::SetOperation(set_op.clone())) {
                    Statement::Select(sel.clone())
                } else {
                    first_stmt
                }
            }
            other => other,
        };

        let Statement::Select(view_select) = first_stmt else {
            return Err(DbError::internal(
                "view query SQL did not parse to a SELECT",
            ));
        };
        let mut view_session = current_session_context();
        let view_search_path = if view.creation_search_path_schemas.is_empty() {
            view.name
                .schema_name()
                .or(default_schema)
                .map(str::to_owned)
                .into_iter()
                .collect::<Vec<_>>()
        } else {
            view.creation_search_path_schemas.clone()
        };
        if let Some(current_schema) = view_search_path.first().cloned() {
            view_session.current_schema = Some(current_schema);
            view_session.search_path_schemas = std::sync::Arc::new(view_search_path.clone());
        }
        let view_default_schema = view_search_path
            .first()
            .map(String::as_str)
            .or(view.name.schema_name())
            .or(default_schema);
        let mut bound = with_session_context(view_session, || {
            self.bind_select(&view_select, txn_id, view_default_schema)
        })?;

        // Build two parallel name vectors for the inner projections:
        // - `inner_proj_orig_names`: the original column name from the expression
        //    (e.g., for `b AS bb` this is "b").
        // - `inner_proj_display_names`: the output column name as the user sees
        //    it (alias if present, else expression name).
        // The distinction matters for alias_to_inner: we need to map
        // view aliases → original column names so the type checker can
        // resolve columns against the underlying table.
        let view_columns = &view.columns;
        let inner_proj_orig_names: Vec<String> = bound
            .projections
            .iter()
            .map(|p| {
                if let Expr::Identifier(name) = &p.expr {
                    name.parts.last().cloned().unwrap_or_default()
                } else {
                    // For non-identifier expressions, fall back to the alias
                    p.alias.clone().unwrap_or_default()
                }
            })
            .collect();
        let inner_proj_display_names: Vec<String> = bound
            .projections
            .iter()
            .map(|p| {
                if let Some(alias) = &p.alias {
                    alias.clone()
                } else if let Expr::Identifier(name) = &p.expr {
                    name.parts.last().cloned().unwrap_or_default()
                } else {
                    String::new()
                }
            })
            .collect();

        // Apply view column aliases to the inner projections so that
        // SELECT * returns the aliased column names.
        for (i, proj) in bound.projections.iter_mut().enumerate() {
            if i < view_columns.len()
                && view_columns[i].name
                    != inner_proj_display_names
                        .get(i)
                        .map(|s| s.as_str())
                        .unwrap_or("")
            {
                proj.alias = Some(view_columns[i].name.clone());
            }
        }

        // Apply the outer query's projections, filter, etc. on top of the view
        // For now, we re-bind the outer select using the view's column descriptors
        // as a synthetic table descriptor.
        let view_table = build_view_table_descriptor(view);

        // Build expression-level map: view alias → inner expression.
        // For identifier-based projections (e.g., `SELECT col FROM t`), the map
        // value is an Identifier so the normal column-name rewrite works.
        // For non-identifier projections (e.g., literals from VALUES), the map
        // value is the actual expression so it gets inlined directly -- this
        // avoids producing an unresolvable column reference like `column1`.
        let alias_to_expr: std::collections::HashMap<String, Expr> = view_columns
            .iter()
            .enumerate()
            .filter_map(|(i, col)| {
                let proj = bound.projections.get(i)?;
                let inner_expr = if let Expr::Identifier(_) = &proj.expr {
                    // Normal column reference -- keep as identifier rewrite
                    let inner_name = inner_proj_orig_names.get(i)?.clone();
                    if inner_name.to_lowercase() == col.name.to_lowercase() {
                        return None; // same name, no rewrite needed
                    }
                    Expr::Identifier(ObjectName {
                        parts: vec![inner_name],
                        span: proj.expr.span(),
                    })
                } else {
                    // Non-identifier (literal, function call, etc.) -- inline
                    // the actual expression so we don't create an unresolvable
                    // column reference.
                    proj.expr.clone()
                };
                Some((col.name.to_lowercase(), inner_expr))
            })
            .collect();

        // Also build the string map for contexts that still need it
        // (two-part identifiers, DML paths via rewrite_view_expr).
        let alias_to_inner: std::collections::HashMap<String, String> = view_columns
            .iter()
            .enumerate()
            .filter_map(|(i, col)| {
                inner_proj_orig_names
                    .get(i)
                    .map(|inner| (col.name.to_lowercase(), inner.clone()))
            })
            .collect();

        // Re-resolve projections from the outer SELECT against the view columns.
        // For SELECT *, keep the inner query's projections since they already
        // contain the correct expressions (with aliases applied above).
        let is_all_star = select.items.len() == 1 && is_star_expr(&select.items[0].expr);
        if is_all_star {
            // SELECT * path - still need to rewrite WHERE, GROUP BY, HAVING
            // from the outer query through the view alias mapping.
            if let Some(ref sel) = select.selection {
                let rewritten = rewrite_view_expr_inline(sel, &alias_to_inner, &alias_to_expr);
                if expr_contains_aggregate(&rewritten) {
                    bound.having = combine_predicates_with_and(bound.having.take(), rewritten);
                } else {
                    bound.selection =
                        combine_predicates_with_and(bound.selection.take(), rewritten);
                }
            }
            if !select.group_by.is_empty() {
                bound.group_by = select
                    .group_by
                    .iter()
                    .map(|e| rewrite_view_expr_inline(e, &alias_to_inner, &alias_to_expr))
                    .collect();
            }
            if let Some(ref having) = select.having {
                let rewritten = rewrite_view_expr_inline(having, &alias_to_inner, &alias_to_expr);
                bound.having = combine_predicates_with_and(bound.having.take(), rewritten);
            }
        } else {
            let mut projections = Vec::new();
            for item in &select.items {
                if is_star_expr(&item.expr) {
                    for column in &view_table.columns {
                        projections.push(BoundProjection {
                            alias: None,
                            expr: Expr::Identifier(ObjectName {
                                parts: vec![column.name.clone()],
                                span: item.span,
                            }),
                        });
                    }
                } else {
                    let rewritten =
                        rewrite_view_expr_inline(&item.expr, &alias_to_inner, &alias_to_expr);
                    // If the outer column was a simple view alias reference
                    // (e.g., SELECT a FROM v), preserve the alias name in the
                    // output so the result column is named "a", not the inner
                    // column name "x".
                    let alias = item.alias.clone().or_else(|| {
                        if let Expr::Identifier(ref orig) = item.expr {
                            let orig_name = orig.parts.last()?;
                            // Rewrite changed the expression -- preserve the
                            // original name as an alias so the output column
                            // keeps the view alias.
                            if let Expr::Identifier(ref rew) = rewritten {
                                let rew_name = rew.parts.last()?;
                                if orig_name != rew_name {
                                    return Some(orig_name.clone());
                                }
                            } else {
                                // Inlined to a non-identifier expression
                                // (e.g., literal from VALUES) -- need the alias.
                                if alias_to_expr.contains_key(&orig_name.to_lowercase()) {
                                    return Some(orig_name.clone());
                                }
                            }
                        }
                        None
                    });
                    projections.push(BoundProjection {
                        alias,
                        expr: rewritten,
                    });
                }
            }
            bound.projections = projections;

            // Also rewrite WHERE, GROUP BY, HAVING, ORDER BY expressions
            if let Some(ref sel) = select.selection {
                let rewritten = rewrite_view_expr_inline(sel, &alias_to_inner, &alias_to_expr);
                if expr_contains_aggregate(&rewritten) {
                    bound.having = combine_predicates_with_and(bound.having.take(), rewritten);
                } else {
                    bound.selection =
                        combine_predicates_with_and(bound.selection.take(), rewritten);
                }
            }
            if !select.group_by.is_empty() {
                bound.group_by = select
                    .group_by
                    .iter()
                    .map(|e| rewrite_view_expr_inline(e, &alias_to_inner, &alias_to_expr))
                    .collect();
            }
            if let Some(ref having) = select.having {
                let rewritten = rewrite_view_expr_inline(having, &alias_to_inner, &alias_to_expr);
                bound.having = combine_predicates_with_and(bound.having.take(), rewritten);
            }
        }

        if !select.order_by.is_empty() {
            bound.order_by = select
                .order_by
                .iter()
                .map(|item| BoundOrderBy {
                    expr: rewrite_view_expr_inline(&item.expr, &alias_to_inner, &alias_to_expr),
                    descending: item.descending,
                    nulls_first: item.nulls_first,
                })
                .collect();
        }
        if select.limit.is_some() {
            bound.limit = select.limit.clone();
        }
        if select.offset.is_some() {
            bound.offset = select.offset.clone();
        }
        if !matches!(select.distinct, DistinctKind::All) {
            bound.distinct = select.distinct.clone();
        }
        Ok(bound)
    }
}

/// Resolve a view's underlying table descriptor by re-parsing the view SQL.
/// Returns `Some(table)` for simple views that select from a single table.
/// Returns `None` if the view query is too complex (e.g. joins, subqueries).
pub(super) fn resolve_view_underlying_table(
    view: &ViewDescriptor,
    catalog: &dyn CatalogReader,
    txn_id: TxnId,
    depth: usize,
) -> DbResult<Option<TableDescriptor>> {
    if depth > 64 {
        return Ok(None);
    }
    let stmts = aiondb_parser::parse_sql(&view.query_sql)?;
    let Some(Statement::Select(select)) = stmts.into_iter().next() else {
        return Ok(None);
    };
    let Some(from) = &select.from else {
        return Ok(None);
    };
    for table_name in view_relation_lookup_candidates(view, from)? {
        if let Some(table) = catalog.get_table(txn_id, &table_name)? {
            return Ok(Some(table));
        }
        if let Some(inner_view) = catalog.get_view(txn_id, &table_name)? {
            return resolve_view_underlying_table(&inner_view, catalog, txn_id, depth + 1);
        }
    }
    Ok(None)
}

/// Result of resolving a view as a DML target.
///
/// Holds the underlying table and a mapping from view column name
/// (lowercased) to the underlying table column name.
pub(super) struct ViewDmlTarget {
    pub table: TableDescriptor,
    pub col_map: std::collections::HashMap<String, String>,
    /// View columns (lowercased) that are not updatable, with the reason.
    /// Includes columns that reference system columns (ctid, oid, ...) or
    /// non-identifier expressions (e.g. `upper(b)`).
    pub non_updatable: std::collections::HashMap<String, NonUpdatableColReason>,
    /// Base-table predicates that restrict which rows are visible through
    /// the view and therefore writable by UPDATE/DELETE through the view.
    pub qualifier_predicates: Vec<Expr>,
    /// Base-table predicates to enforce for view CHECK OPTION.
    pub check_predicates: Vec<Expr>,
}

#[derive(Debug, Clone, Copy)]
pub(crate) enum NonUpdatableColReason {
    SystemColumn,
    Expression,
}

impl NonUpdatableColReason {
    pub(crate) fn detail(&self) -> &'static str {
        match self {
            Self::SystemColumn => "View columns that refer to system columns are not updatable.",
            Self::Expression => {
                "View columns that are not columns of their base relation are not updatable."
            }
        }
    }
}

/// Resolve a view for DML (INSERT/UPDATE/DELETE) by finding the underlying
/// table and building a column-name mapping from view aliases to table columns.
///
/// Returns `None` if the view is too complex to be automatically updatable
/// (e.g., joins, aggregates, set operations).
pub(super) fn resolve_view_for_dml(
    view: &ViewDescriptor,
    catalog: &dyn CatalogReader,
    txn_id: TxnId,
    required_event: TriggerEventDescriptor,
) -> DbResult<Option<ViewDmlTarget>> {
    resolve_view_for_dml_inner(view, catalog, txn_id, required_event, 0)
}

fn resolve_view_for_dml_inner(
    view: &ViewDescriptor,
    catalog: &dyn CatalogReader,
    txn_id: TxnId,
    required_event: TriggerEventDescriptor,
    depth: usize,
) -> DbResult<Option<ViewDmlTarget>> {
    if depth > 64 {
        return Ok(None);
    }
    let stmts = aiondb_parser::parse_sql(&view.query_sql)?;
    let Some(Statement::Select(ref select)) = stmts.first() else {
        return Ok(None);
    };
    let Some(ref from) = select.from else {
        return Ok(None);
    };
    // Reject views that are not automatically updatable per PostgreSQL rules.
    // A view is NOT automatically updatable if it uses DISTINCT, CTEs, joins,
    // GROUP BY, HAVING, LIMIT, or OFFSET.
    if !matches!(select.distinct, DistinctKind::All)
        || !select.ctes.is_empty()
        || !select.joins.is_empty()
        || !select.group_by.is_empty()
        || select.having.is_some()
        || select.limit.is_some()
        || select.offset.is_some()
    {
        return Ok(None);
    }
    // Reject views whose target list contains aggregate, window, or
    // set-returning functions - these are not automatically updatable.
    for item in &select.items {
        if expr_has_aggregate(&item.expr) || expr_has_window(&item.expr) || expr_has_srf(&item.expr)
        {
            return Ok(None);
        }
    }
    let mut resolved = None;
    for table_name in view_relation_lookup_candidates(view, from)? {
        if let Some(tbl) = catalog.get_table(txn_id, &table_name)? {
            resolved = Some((
                tbl,
                None,
                std::collections::HashMap::new(),
                Vec::<Expr>::new(),
                Vec::<Expr>::new(),
            ));
            break;
        }
        if let Some(inner_view) = catalog.get_view(txn_id, &table_name)? {
            if let Some(target) =
                resolve_view_for_dml_inner(&inner_view, catalog, txn_id, required_event, depth + 1)?
            {
                resolved = Some((
                    target.table,
                    Some(target.col_map),
                    target.non_updatable,
                    target.qualifier_predicates,
                    target.check_predicates,
                ));
                break;
            }
            if view_has_instead_of_trigger(catalog, txn_id, &inner_view, required_event)? {
                if let Some(inner_table) =
                    resolve_view_underlying_table(&inner_view, catalog, txn_id, depth + 1)?
                {
                    resolved = Some((
                        inner_table,
                        Some(identity_view_col_map(&inner_view)),
                        std::collections::HashMap::new(),
                        Vec::new(),
                        Vec::new(),
                    ));
                    break;
                }
                return Ok(None);
            }
            return Ok(None);
        }
    }
    let Some((
        table,
        inner_col_map,
        inner_non_updatable,
        inner_qualifier_predicates,
        inner_check_predicates,
    )) = resolved
    else {
        return Ok(None);
    };

    // Build the column mapping: view_alias -> table_column_name.
    // For SELECT items, extract the original column names and their aliases.
    // When the FROM is itself a view, we chain through that view's mapping.
    let mut col_map = std::collections::HashMap::new();
    let mut non_updatable: std::collections::HashMap<String, NonUpdatableColReason> =
        std::collections::HashMap::new();
    for (idx, item) in select.items.iter().enumerate() {
        // Determine the original column name from the expression
        let orig_col = match &item.expr {
            Expr::Identifier(name) => {
                let part = name.parts.last().cloned().unwrap_or_default();
                if part == "*" {
                    // SELECT * - map through inner view's col_map if present
                    if let Some(ref icm) = inner_col_map {
                        for (k, v) in icm {
                            col_map.insert(k.clone(), v.clone());
                        }
                    } else {
                        for col in &table.columns {
                            col_map.insert(col.name.to_lowercase(), col.name.clone());
                        }
                    }
                    continue;
                }
                if is_system_column_name(&part) {
                    let alias = item
                        .alias
                        .as_ref()
                        .map(|a| a.to_lowercase())
                        .unwrap_or_else(|| part.to_lowercase());
                    non_updatable.insert(alias, NonUpdatableColReason::SystemColumn);
                    continue;
                }
                part
            }
            _ => {
                // Non-identifier expression in SELECT list - column is not
                // updatable through the view. Record the alias name.
                let alias_name = match &item.alias {
                    Some(a) => a.to_lowercase(),
                    None => synthesize_view_column_name(&item.expr, idx),
                };
                non_updatable.insert(alias_name, NonUpdatableColReason::Expression);
                continue;
            }
        };
        let alias = item
            .alias
            .as_ref()
            .map(|a| a.to_lowercase())
            .unwrap_or_else(|| orig_col.to_lowercase());
        // Chain through inner view mapping if FROM is a view
        let resolved = if let Some(ref icm) = inner_col_map {
            let key = orig_col.to_lowercase();
            if let Some(mapped) = icm.get(&key) {
                mapped.clone()
            } else if inner_non_updatable.contains_key(&key) {
                non_updatable.insert(alias.clone(), NonUpdatableColReason::Expression);
                continue;
            } else {
                orig_col.clone()
            }
        } else {
            orig_col.clone()
        };
        col_map
            .entry(orig_col.to_ascii_lowercase())
            .or_insert_with(|| resolved.clone());
        col_map.insert(alias, resolved);
    }

    let check_mode = view_check_option_mode(view);
    let mut check_predicates = match check_mode {
        ViewCheckOptionMode::Cascaded | ViewCheckOptionMode::None => inner_check_predicates,
        ViewCheckOptionMode::Local => Vec::new(),
    };
    if !matches!(check_mode, ViewCheckOptionMode::None) {
        if let Some(selection) = &select.selection {
            check_predicates.push(rewrite_view_expr(selection, &col_map));
        }
    }
    let mut qualifier_predicates = inner_qualifier_predicates;
    if let Some(selection) = &select.selection {
        qualifier_predicates.push(rewrite_view_expr(selection, &col_map));
    }

    Ok(Some(ViewDmlTarget {
        table,
        col_map,
        non_updatable,
        qualifier_predicates,
        check_predicates,
    }))
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ViewCheckOptionMode {
    None,
    Local,
    Cascaded,
}

pub(crate) fn view_check_option_mode(view: &ViewDescriptor) -> ViewCheckOptionMode {
    let descriptor_mode = match view.check_option {
        Some(ViewCheckOption::Local) => ViewCheckOptionMode::Local,
        Some(ViewCheckOption::Cascaded) => ViewCheckOptionMode::Cascaded,
        None => ViewCheckOptionMode::None,
    };
    // Per-session ALTER VIEW SET CHECK OPTION updates are still tracked in
    // the compat-attrs sidecar; honour them so an ALTER takes
    // precedence over the persisted descriptor value within the same
    // session.
    view_check_option_mode_from_compat_attrs(view).unwrap_or(descriptor_mode)
}

fn view_check_option_mode_from_compat_attrs(view: &ViewDescriptor) -> Option<ViewCheckOptionMode> {
    let target_name = view.name.object_name().to_ascii_lowercase();
    let qualified_name = view.name.to_string().to_ascii_lowercase();
    aiondb_eval::with_current_session_context(|session_context| {
        for ((kind, object_name), (_, _, _, options_joined, _, _)) in
            &*session_context.compat_misc_attrs
        {
            if kind != "CREATE VIEW" {
                continue;
            }
            let object_lc = object_name.to_ascii_lowercase();
            if object_lc != target_name && object_lc != qualified_name {
                continue;
            }
            for pair in options_joined.split(',').map(str::trim) {
                if let Some(value) = pair.strip_prefix("check_option=") {
                    let normalized = value.trim().to_ascii_lowercase();
                    return Some(match normalized.as_str() {
                        "local" => ViewCheckOptionMode::Local,
                        "cascaded" => ViewCheckOptionMode::Cascaded,
                        _ => ViewCheckOptionMode::None,
                    });
                }
            }
        }
        None
    })
}

pub(super) fn relation_with_view_checks(
    mut relation: TableDescriptor,
    checks: &[Expr],
) -> TableDescriptor {
    for (index, check_expr) in checks.iter().enumerate() {
        relation.check_constraints.push(CheckConstraint {
            name: Some(format!("__aiondb_view_check_option_{}", index + 1)),
            expression: format_expr(check_expr),
        });
    }
    relation
}

fn view_has_instead_of_trigger(
    catalog: &dyn CatalogReader,
    txn_id: TxnId,
    view: &ViewDescriptor,
    required_event: TriggerEventDescriptor,
) -> DbResult<bool> {
    let trigger_target = view.name.to_string();
    let mut triggers = catalog.list_triggers(txn_id, &trigger_target)?;
    if triggers.is_empty() {
        triggers = catalog.list_triggers(txn_id, view.name.object_name())?;
    }
    Ok(triggers.iter().any(|trigger| {
        trigger.timing == TriggerTimingDescriptor::InsteadOf && trigger.event == required_event
    }))
}

fn identity_view_col_map(view: &ViewDescriptor) -> std::collections::HashMap<String, String> {
    view.columns
        .iter()
        .map(|column| (column.name.to_ascii_lowercase(), column.name.clone()))
        .collect()
}

fn synthesize_view_column_name(expr: &Expr, idx: usize) -> String {
    match expr {
        Expr::FunctionCall { name, .. } => name
            .parts
            .last()
            .map(|s| s.to_ascii_lowercase())
            .unwrap_or_else(|| format!("?column?{}", idx + 1)),
        Expr::Cast { .. } => "?column?".to_owned(),
        _ => format!("?column?{}", idx + 1),
    }
}

pub(crate) fn view_relation_lookup_candidates(
    view: &ViewDescriptor,
    name: &ObjectName,
) -> DbResult<Vec<QualifiedName>> {
    match name.parts.as_slice() {
        [relation] => {
            let mut candidates = Vec::new();
            let mut seen = std::collections::HashSet::new();
            for schema_name in &view.creation_search_path_schemas {
                if seen.insert(schema_name.to_ascii_lowercase()) {
                    candidates.push(QualifiedName::new(Some(schema_name.as_str()), relation));
                }
            }
            if candidates.is_empty() {
                candidates.push(QualifiedName::new(view.name.schema_name(), relation));
            }
            Ok(candidates)
        }
        _ => Ok(vec![qualified_name_with_default(
            name,
            view.name.schema_name(),
        )?]),
    }
}

fn canonicalize_select_for_view_storage(
    catalog: &dyn CatalogReader,
    txn_id: TxnId,
    select: &SelectStatement,
    default_schema: Option<&str>,
) -> DbResult<SelectStatement> {
    let mut canonical = select.clone();
    for cte in &mut canonical.ctes {
        *cte.query =
            canonicalize_statement_for_view_storage(catalog, txn_id, &cte.query, default_schema)?;
        if let Some(recursive_term) = &mut cte.recursive_term {
            *recursive_term = Box::new(canonicalize_select_for_view_storage(
                catalog,
                txn_id,
                recursive_term,
                default_schema,
            )?);
        }
    }
    if let Some(from) = &canonical.from {
        canonical.from = Some(resolve_relation_name_for_view_storage(
            catalog,
            txn_id,
            from,
            default_schema,
        )?);
    }
    for join in &mut canonical.joins {
        join.table =
            resolve_relation_name_for_view_storage(catalog, txn_id, &join.table, default_schema)?;
    }
    Ok(canonical)
}

fn canonicalize_statement_for_view_storage(
    catalog: &dyn CatalogReader,
    txn_id: TxnId,
    statement: &Statement,
    default_schema: Option<&str>,
) -> DbResult<Statement> {
    match statement {
        Statement::Select(select) => Ok(Statement::Select(canonicalize_select_for_view_storage(
            catalog,
            txn_id,
            select,
            default_schema,
        )?)),
        Statement::SetOperation(set_op) => {
            let mut canonical = set_op.clone();
            canonical.left = Box::new(canonicalize_statement_for_view_storage(
                catalog,
                txn_id,
                &set_op.left,
                default_schema,
            )?);
            canonical.right = Box::new(canonicalize_statement_for_view_storage(
                catalog,
                txn_id,
                &set_op.right,
                default_schema,
            )?);
            Ok(Statement::SetOperation(canonical))
        }
        other => Ok(other.clone()),
    }
}

fn resolve_relation_name_for_view_storage(
    catalog: &dyn CatalogReader,
    txn_id: TxnId,
    name: &ObjectName,
    default_schema: Option<&str>,
) -> DbResult<ObjectName> {
    for candidate in relation_lookup_candidates(name, default_schema)? {
        if catalog.get_table(txn_id, &candidate)?.is_some()
            || catalog.get_view(txn_id, &candidate)?.is_some()
            || catalog.get_sequence(txn_id, &candidate)?.is_some()
            || resolve_virtual_relation(&candidate).is_some()
        {
            return Ok(object_name_from_qualified(&candidate, name.span));
        }
    }
    Ok(name.clone())
}

fn object_name_from_qualified(name: &QualifiedName, span: Span) -> ObjectName {
    let mut parts = Vec::new();
    if let Some(schema) = name.schema_name() {
        parts.push(schema.to_owned());
    }
    parts.push(name.object_name().to_owned());
    ObjectName { parts, span }
}

/// Diagnose why a view is not automatically updatable, returning a
/// PostgreSQL-compatible DETAIL string (e.g. "Views containing DISTINCT are
/// not automatically updatable.").  Returns `None` only if the view appears
/// to be a simple updatable view (should not happen in the error path).
pub(super) fn diagnose_view_non_updatable(view: &ViewDescriptor) -> Option<String> {
    let Ok(stmts) = aiondb_parser::parse_sql(&view.query_sql) else {
        return Some(
            "Views whose definition cannot be parsed are not automatically updatable.".to_owned(),
        );
    };
    let Some(Statement::Select(ref select)) = stmts.first() else {
        return Some(
            "Views that do not select from a single table or view are not automatically updatable."
                .to_owned(),
        );
    };

    if !matches!(select.distinct, DistinctKind::All) {
        return Some("Views containing DISTINCT are not automatically updatable.".to_owned());
    }
    if !select.ctes.is_empty() {
        return Some("Views containing WITH are not automatically updatable.".to_owned());
    }
    if !select.group_by.is_empty() {
        return Some("Views containing GROUP BY are not automatically updatable.".to_owned());
    }
    if select.having.is_some() {
        return Some("Views containing HAVING are not automatically updatable.".to_owned());
    }
    if select.limit.is_some() || select.offset.is_some() {
        return Some(
            "Views containing LIMIT or OFFSET are not automatically updatable.".to_owned(),
        );
    }
    if !select.joins.is_empty() {
        return Some(
            "Views that do not select from a single table or view are not automatically updatable."
                .to_owned(),
        );
    }
    if select.from.is_none() {
        return Some(
            "Views that do not select from a single table or view are not automatically updatable."
                .to_owned(),
        );
    }

    // Check for aggregate/window functions in the target list.
    for item in &select.items {
        if expr_has_aggregate(&item.expr) {
            return Some(
                "Views that return aggregate functions are not automatically updatable.".to_owned(),
            );
        }
        if expr_has_window(&item.expr) {
            return Some(
                "Views that return window functions are not automatically updatable.".to_owned(),
            );
        }
        if expr_has_srf(&item.expr) {
            return Some(
                "Views that return set-returning functions in the target list are not automatically updatable.".to_owned(),
            );
        }
    }

    // Fallback
    Some(
        "Views that do not select from a single table or view are not automatically updatable."
            .to_owned(),
    )
}

/// Check if an expression is an aggregate function call.
fn expr_has_aggregate(expr: &Expr) -> bool {
    match expr {
        Expr::FunctionCall { name, .. } => {
            let func = name
                .parts
                .last()
                .map(|s| s.to_ascii_lowercase())
                .unwrap_or_default();
            matches!(
                func.as_str(),
                "count"
                    | "sum"
                    | "avg"
                    | "min"
                    | "max"
                    | "array_agg"
                    | "string_agg"
                    | "bool_and"
                    | "bool_or"
                    | "every"
                    | "bit_and"
                    | "bit_or"
                    | "xmlagg"
            )
        }
        _ => false,
    }
}

/// Check if an expression is a window function call.
fn expr_has_window(expr: &Expr) -> bool {
    matches!(expr, Expr::WindowFunction { .. })
}

/// Check if an expression is a set-returning function call.
fn expr_has_srf(expr: &Expr) -> bool {
    match expr {
        Expr::FunctionCall { name, .. } => {
            let func = name
                .parts
                .last()
                .map(|s| s.to_ascii_lowercase())
                .unwrap_or_default();
            matches!(
                func.as_str(),
                "generate_series"
                    | "unnest"
                    | "regexp_matches"
                    | "json_each"
                    | "json_array_elements"
                    | "jsonb_each"
                    | "jsonb_array_elements"
            )
        }
        _ => false,
    }
}

fn expr_contains_aggregate(expr: &Expr) -> bool {
    let mut stack = vec![expr];
    while let Some(expr) = stack.pop() {
        if expr_has_aggregate(expr) {
            return true;
        }
        match expr {
            Expr::UnaryOp { expr, .. } | Expr::IsNull { expr, .. } | Expr::Cast { expr, .. } => {
                stack.push(expr);
            }
            Expr::BinaryOp { left, right, .. } | Expr::IsDistinctFrom { left, right, .. } => {
                stack.push(right);
                stack.push(left);
            }
            Expr::Like { expr, pattern, .. } => {
                stack.push(pattern);
                stack.push(expr);
            }
            Expr::InList { expr, list, .. } => {
                stack.extend(list);
                stack.push(expr);
            }
            Expr::Between {
                expr, low, high, ..
            } => {
                stack.push(high);
                stack.push(low);
                stack.push(expr);
            }
            Expr::CaseWhen {
                operand,
                conditions,
                results,
                else_result,
                ..
            } => {
                if let Some(else_result) = else_result {
                    stack.push(else_result);
                }
                stack.extend(results);
                stack.extend(conditions);
                if let Some(operand) = operand {
                    stack.push(operand);
                }
            }
            Expr::FunctionCall { args, filter, .. } => {
                stack.extend(args);
                if let Some(filter) = filter {
                    stack.push(filter);
                }
            }
            Expr::WindowFunction { .. } => return true,
            _ => {}
        }
    }
    false
}

fn combine_predicates_with_and(existing: Option<Expr>, predicate: Expr) -> Option<Expr> {
    match existing {
        Some(existing_expr) => Some(Expr::BinaryOp {
            left: Box::new(existing_expr),
            op: aiondb_parser::BinaryOperator::And,
            right: Box::new(predicate.clone()),
            span: predicate.span(),
        }),
        None => Some(predicate),
    }
}

pub(super) fn undefined_view(view_name: &QualifiedName) -> DbError {
    DbError::bind_error(
        SqlState::UndefinedObject,
        format!("view \"{view_name}\" does not exist"),
    )
}

pub(super) fn qualified_create_view_name(
    name: &ObjectName,
    default_schema: Option<&str>,
    temporary: bool,
) -> DbResult<QualifiedName> {
    if temporary {
        if let [relation] = name.parts.as_slice() {
            return Ok(QualifiedName::qualified(PG_TEMP_SCHEMA_NAME, relation));
        }
    }

    qualified_name_with_default(name, default_schema)
}

pub(super) fn build_view_table_descriptor(view: &ViewDescriptor) -> TableDescriptor {
    TableDescriptor {
        table_id: view.view_id,
        schema_id: view.schema_id,
        name: view.name.clone(),
        columns: view.columns.clone(),
        primary_key: None,
        foreign_keys: Vec::new(),
        check_constraints: Vec::new(),
        shard_config: None,
        identity_columns: Vec::new(),
        owner: None,
    }
}

/// Rewrite column references in an expression, replacing view column aliases
/// with the corresponding inner query column names.
pub(super) fn rewrite_view_expr(
    expr: &Expr,
    alias_map: &std::collections::HashMap<String, String>,
) -> Expr {
    match expr {
        Expr::Identifier(name) => {
            // Only rewrite simple single-part identifiers (column refs)
            if name.parts.len() == 1 {
                let lower = name.parts[0].to_lowercase();
                if let Some(inner_name) = alias_map.get(&lower) {
                    // If inner and alias are the same, no rewrite needed
                    if inner_name.to_lowercase() != lower {
                        return Expr::Identifier(ObjectName {
                            parts: vec![inner_name.clone()],
                            span: name.span,
                        });
                    }
                }
            }
            // Two-part identifiers like `t1.a` - rewrite the column part
            if name.parts.len() == 2 {
                let lower = name.parts[1].to_lowercase();
                if let Some(inner_name) = alias_map.get(&lower) {
                    if inner_name.to_lowercase() != lower {
                        return Expr::Identifier(ObjectName {
                            parts: vec![name.parts[0].clone(), inner_name.clone()],
                            span: name.span,
                        });
                    }
                }
            }
            expr.clone()
        }
        Expr::BinaryOp {
            left,
            op,
            right,
            span,
        } => Expr::BinaryOp {
            left: Box::new(rewrite_view_expr(left, alias_map)),
            op: op.clone(),
            right: Box::new(rewrite_view_expr(right, alias_map)),
            span: *span,
        },
        Expr::UnaryOp { op, expr: e, span } => Expr::UnaryOp {
            op: op.clone(),
            expr: Box::new(rewrite_view_expr(e, alias_map)),
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
                .map(|a| rewrite_view_expr(a, alias_map))
                .collect(),
            distinct: *distinct,
            filter: filter
                .as_ref()
                .map(|f| Box::new(rewrite_view_expr(f, alias_map))),
            span: *span,
        },
        Expr::IsNull {
            expr: e,
            negated,
            span,
        } => Expr::IsNull {
            expr: Box::new(rewrite_view_expr(e, alias_map)),
            negated: *negated,
            span: *span,
        },
        Expr::Cast {
            expr: e,
            data_type,
            span,
        } => Expr::Cast {
            expr: Box::new(rewrite_view_expr(e, alias_map)),
            data_type: data_type.clone(),
            span: *span,
        },
        Expr::Between {
            expr: e,
            low,
            high,
            negated,
            span,
        } => Expr::Between {
            expr: Box::new(rewrite_view_expr(e, alias_map)),
            low: Box::new(rewrite_view_expr(low, alias_map)),
            high: Box::new(rewrite_view_expr(high, alias_map)),
            negated: *negated,
            span: *span,
        },
        Expr::InList {
            expr: e,
            list,
            negated,
            span,
        } => Expr::InList {
            expr: Box::new(rewrite_view_expr(e, alias_map)),
            list: list
                .iter()
                .map(|l| rewrite_view_expr(l, alias_map))
                .collect(),
            negated: *negated,
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
                .map(|o| Box::new(rewrite_view_expr(o, alias_map))),
            conditions: conditions
                .iter()
                .map(|c| rewrite_view_expr(c, alias_map))
                .collect(),
            results: results
                .iter()
                .map(|r| rewrite_view_expr(r, alias_map))
                .collect(),
            else_result: else_result
                .as_ref()
                .map(|e| Box::new(rewrite_view_expr(e, alias_map))),
            span: *span,
        },
        // For other expression kinds, return as-is
        _ => expr.clone(),
    }
}

/// Like [`rewrite_view_expr`] but with an additional expression-level map.
///
/// When a single-part identifier matches a key in `expr_map`, the corresponding
/// expression is inlined directly.  This handles views defined over VALUES
/// (or other non-FROM queries) where the inner projection is a literal or
/// expression rather than a column reference.  Without this, the rewrite would
/// produce an unresolvable identifier like `column1`.
///
/// The `alias_map` is still consulted for two-part identifiers and as a
/// fallback for single-part identifiers not present in `expr_map`.
fn rewrite_view_expr_inline(
    expr: &Expr,
    alias_map: &std::collections::HashMap<String, String>,
    expr_map: &std::collections::HashMap<String, Expr>,
) -> Expr {
    match expr {
        Expr::Identifier(name) => {
            if name.parts.len() == 1 {
                let lower = name.parts[0].to_lowercase();
                // First try the expression map (handles VALUES literals etc.)
                if let Some(inner_expr) = expr_map.get(&lower) {
                    return inner_expr.clone();
                }
                // Fall back to the string alias map
                if let Some(inner_name) = alias_map.get(&lower) {
                    if inner_name.to_lowercase() != lower {
                        return Expr::Identifier(ObjectName {
                            parts: vec![inner_name.clone()],
                            span: name.span,
                        });
                    }
                }
            }
            if name.parts.len() == 2 {
                let lower = name.parts[1].to_lowercase();
                // First try the expression map (handles VALUES literals etc.)
                // For two-part references like `t1.a` where `a` maps to a
                // non-identifier expression (e.g. a literal from VALUES),
                // inline the expression directly instead of creating an
                // unresolvable column reference like `t1.column1`.
                if let Some(inner_expr) = expr_map.get(&lower) {
                    return inner_expr.clone();
                }
                if let Some(inner_name) = alias_map.get(&lower) {
                    if inner_name.to_lowercase() != lower {
                        return Expr::Identifier(ObjectName {
                            parts: vec![name.parts[0].clone(), inner_name.clone()],
                            span: name.span,
                        });
                    }
                }
            }
            expr.clone()
        }
        Expr::BinaryOp {
            left,
            op,
            right,
            span,
        } => Expr::BinaryOp {
            left: Box::new(rewrite_view_expr_inline(left, alias_map, expr_map)),
            op: op.clone(),
            right: Box::new(rewrite_view_expr_inline(right, alias_map, expr_map)),
            span: *span,
        },
        Expr::UnaryOp { op, expr: e, span } => Expr::UnaryOp {
            op: op.clone(),
            expr: Box::new(rewrite_view_expr_inline(e, alias_map, expr_map)),
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
                .map(|a| rewrite_view_expr_inline(a, alias_map, expr_map))
                .collect(),
            distinct: *distinct,
            filter: filter
                .as_ref()
                .map(|f| Box::new(rewrite_view_expr_inline(f, alias_map, expr_map))),
            span: *span,
        },
        Expr::IsNull {
            expr: e,
            negated,
            span,
        } => Expr::IsNull {
            expr: Box::new(rewrite_view_expr_inline(e, alias_map, expr_map)),
            negated: *negated,
            span: *span,
        },
        Expr::Cast {
            expr: e,
            data_type,
            span,
        } => Expr::Cast {
            expr: Box::new(rewrite_view_expr_inline(e, alias_map, expr_map)),
            data_type: data_type.clone(),
            span: *span,
        },
        Expr::Between {
            expr: e,
            low,
            high,
            negated,
            span,
        } => Expr::Between {
            expr: Box::new(rewrite_view_expr_inline(e, alias_map, expr_map)),
            low: Box::new(rewrite_view_expr_inline(low, alias_map, expr_map)),
            high: Box::new(rewrite_view_expr_inline(high, alias_map, expr_map)),
            negated: *negated,
            span: *span,
        },
        Expr::InList {
            expr: e,
            list,
            negated,
            span,
        } => Expr::InList {
            expr: Box::new(rewrite_view_expr_inline(e, alias_map, expr_map)),
            list: list
                .iter()
                .map(|l| rewrite_view_expr_inline(l, alias_map, expr_map))
                .collect(),
            negated: *negated,
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
                .map(|o| Box::new(rewrite_view_expr_inline(o, alias_map, expr_map))),
            conditions: conditions
                .iter()
                .map(|c| rewrite_view_expr_inline(c, alias_map, expr_map))
                .collect(),
            results: results
                .iter()
                .map(|r| rewrite_view_expr_inline(r, alias_map, expr_map))
                .collect(),
            else_result: else_result
                .as_ref()
                .map(|e| Box::new(rewrite_view_expr_inline(e, alias_map, expr_map))),
            span: *span,
        },
        _ => expr.clone(),
    }
}
