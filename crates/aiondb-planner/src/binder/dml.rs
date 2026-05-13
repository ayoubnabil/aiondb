use super::views::{
    build_view_table_descriptor, diagnose_view_non_updatable, relation_with_view_checks,
    resolve_view_for_dml, rewrite_view_expr, NonUpdatableColReason,
};
use super::*;
use aiondb_core::ErrorReport;
use aiondb_parser::{OnConflict, OnConflictAction, UpdateAssignment};

/// The kind of DML operation, used for PG-compatible error messages.
#[derive(Clone, Copy)]
enum DmlKind {
    Insert,
    Update,
    Delete,
}

/// View metadata flowing back from `resolve_dml_target` for column-level checks.
pub(super) struct ViewMappingInfo {
    pub col_map: std::collections::HashMap<String, String>,
    pub non_updatable: std::collections::HashMap<String, NonUpdatableColReason>,
    pub view_name: String,
    pub qualifier_predicates: Vec<Expr>,
}

fn reject_non_updatable_column(
    info: &ViewMappingInfo,
    column_name: &str,
    is_update: bool,
) -> DbResult<()> {
    let key = column_name.to_ascii_lowercase();
    if let Some(reason) = info.non_updatable.get(&key) {
        let action = if is_update { "update" } else { "insert into" };
        return Err(DbError::feature_not_supported(format!(
            "cannot {action} column \"{column_name}\" of view \"{}\"",
            info.view_name
        ))
        .with_client_detail(reason.detail().to_owned()));
    }
    Ok(())
}

impl Binder {
    /// Resolve a DML target: try as a table first, then as an updatable view.
    /// Returns the underlying `TableDescriptor` and an optional column-name
    /// mapping (view alias -> table column).  When the target is a plain
    /// table the mapping is `None`.
    fn resolve_dml_target(
        &self,
        name: &ObjectName,
        txn_id: TxnId,
        default_schema: Option<&str>,
        dml_kind: DmlKind,
    ) -> DbResult<(TableDescriptor, Option<ViewMappingInfo>)> {
        let mut relation_name = relation_error_name(name, default_schema)?;
        for candidate in relation_lookup_candidates(name, default_schema)? {
            relation_name = candidate;
            if let Some(table) = self.catalog.get_table(txn_id, &relation_name)? {
                return Ok((table, None));
            }
            // Not a table - try as an updatable view.
            if let Some(view) = self.catalog.get_view(txn_id, &relation_name)? {
                let required_event = match dml_kind {
                    DmlKind::Insert => TriggerEventDescriptor::Insert,
                    DmlKind::Update => TriggerEventDescriptor::Update,
                    DmlKind::Delete => TriggerEventDescriptor::Delete,
                };
                if let Some(target) =
                    resolve_view_for_dml(&view, self.catalog.as_ref(), txn_id, required_event)?
                {
                    let relation =
                        relation_with_view_checks(target.table, &target.check_predicates);
                    let info = ViewMappingInfo {
                        col_map: target.col_map,
                        non_updatable: target.non_updatable,
                        view_name: relation_name.object_name().to_owned(),
                        qualifier_predicates: target.qualifier_predicates,
                    };
                    return Ok((relation, Some(info)));
                }
                // View is not automatically updatable - check for INSTEAD OF triggers
                // that would allow DML on this view.
                let required_event = match dml_kind {
                    DmlKind::Insert => TriggerEventDescriptor::Insert,
                    DmlKind::Update => TriggerEventDescriptor::Update,
                    DmlKind::Delete => TriggerEventDescriptor::Delete,
                };
                let view_name = relation_name.object_name();
                let trigger_target = relation_name.to_string();
                let mut triggers = self.catalog.list_triggers(txn_id, &trigger_target)?;
                if triggers.is_empty() {
                    triggers = self.catalog.list_triggers(txn_id, view_name)?;
                }
                let has_instead_of = triggers.iter().any(|t| {
                    t.timing == TriggerTimingDescriptor::InsteadOf && t.event == required_event
                });
                if has_instead_of {
                    let desc = build_view_table_descriptor(&view);
                    return Ok((desc, None));
                }
                let (action_verb, hint_action, hint_trigger) = match dml_kind {
                    DmlKind::Insert => ("insert into", "inserting into", "INSTEAD OF INSERT"),
                    DmlKind::Update => ("update", "updating", "INSTEAD OF UPDATE"),
                    DmlKind::Delete => ("delete from", "deleting from", "INSTEAD OF DELETE"),
                };
                let detail = diagnose_view_non_updatable(&view).unwrap_or_else(|| {
                    "Views that do not select from a single table or view are not automatically updatable.".to_owned()
                });
                let hint = format!(
                    "To enable {} the view, provide an {} trigger or an unconditional ON {} DO INSTEAD rule.",
                    hint_action,
                    hint_trigger,
                    match dml_kind {
                        DmlKind::Insert => "INSERT",
                        DmlKind::Update => "UPDATE",
                        DmlKind::Delete => "DELETE",
                    },
                );
                return Err(DbError::feature_not_supported(format!(
                    "cannot {action_verb} view \"{view_name}\""
                ))
                .with_client_detail(detail)
                .with_client_hint(hint));
            }
            if let Some(virtual_relation) = resolve_virtual_relation(&relation_name) {
                return Ok((virtual_relation, None));
            }
        }
        Err(undefined_table(name, &relation_name))
    }

    /// Resolve additional table references from UPDATE ... FROM or DELETE ... USING.
    fn resolve_dml_extra_tables(
        &self,
        tables: &[(ObjectName, Option<String>)],
        txn_id: TxnId,
        default_schema: Option<&str>,
    ) -> DbResult<Vec<(TableDescriptor, Option<String>)>> {
        self.resolve_dml_extra_tables_with_ctes(tables, &[], txn_id, default_schema)
    }

    /// Same as [`resolve_dml_extra_tables`] but also matches each
    /// `(ObjectName, alias)` entry against `cte_sources` (already
    /// bound via `bind_select`). When a CTE name wins, returns a
    /// synthetic [`TableDescriptor`] whose columns mirror the CTE's
    /// projected output. The planner detects this case via
    /// `BoundUpdate.cte_sources` and lowers the descriptor into an
    /// in-memory materialisation instead of a heap scan; until that
    /// landing, the planner short-circuits with `feature_not_supported`
    /// and the synthetic descriptor never reaches the executor.
    fn resolve_dml_extra_tables_with_ctes(
        &self,
        tables: &[(ObjectName, Option<String>)],
        cte_sources: &[(String, BoundSelect)],
        txn_id: TxnId,
        default_schema: Option<&str>,
    ) -> DbResult<Vec<(TableDescriptor, Option<String>)>> {
        let mut result = Vec::with_capacity(tables.len());
        for (name, alias) in tables {
            if let Some((cte_name, cte_select)) = cte_sources.iter().find(|(cte_name, _)| {
                let last = name.parts.last().map_or("", String::as_str);
                cte_name.eq_ignore_ascii_case(last) && name.parts.len() == 1
            }) {
                let descriptor =
                    self.build_merge_subquery_source_relation(cte_select, Some(cte_name.as_str()))?;
                result.push((descriptor, alias.clone()));
                continue;
            }
            let mut relation_name = relation_error_name(name, default_schema)?;
            let mut resolved = None;
            for candidate in relation_lookup_candidates(name, default_schema)? {
                relation_name = candidate;
                if let Some(table) = self.catalog.get_table(txn_id, &relation_name)? {
                    resolved = Some((table, alias.clone()));
                    break;
                }
                if let Some(view) = self.catalog.get_view(txn_id, &relation_name)? {
                    let desc = super::views::resolve_view_underlying_table(
                        &view,
                        &*self.catalog,
                        txn_id,
                        0,
                    )?
                    .unwrap_or_else(|| super::views::build_view_table_descriptor(&view));
                    resolved = Some((desc, alias.clone()));
                    break;
                }
                let pg_name = name.parts.last().map_or("", String::as_str);
                if let Some(desc) = crate::pg_catalog::build_table_descriptor(pg_name) {
                    resolved = Some((desc, alias.clone()));
                    break;
                }
            }
            result.push(resolved.ok_or_else(|| undefined_table(name, &relation_name))?);
        }
        Ok(result)
    }

    pub(super) fn bind_insert(
        &self,
        insert: &InsertStatement,
        txn_id: TxnId,
        default_schema: Option<&str>,
    ) -> DbResult<BoundInsert> {
        let (relation, view_info) =
            self.resolve_dml_target(&insert.table, txn_id, default_schema, DmlKind::Insert)?;
        let col_map = view_info.as_ref().map(|i| i.col_map.clone());
        if let Some(ref info) = view_info {
            // Implicit-column INSERT (no column list): every view column is targeted.
            // Reject if any view column is not updatable.
            if insert.columns.is_empty() {
                if let Some((key, reason)) = info.non_updatable.iter().next() {
                    return Err(DbError::feature_not_supported(format!(
                        "cannot insert into column \"{key}\" of view \"{}\"",
                        info.view_name
                    ))
                    .with_client_detail(reason.detail().to_owned()));
                }
            } else {
                for column_name in &insert.columns {
                    let column = unqualified_column_name(column_name)?;
                    reject_non_updatable_column(info, column, false)?;
                }
            }
        }
        let mut columns = Vec::with_capacity(insert.columns.len());
        let mut implicit_input_columns = None;

        if insert.columns.is_empty() {
            if let Some(ref map) = col_map {
                if let Some(view) = find_target_view_descriptor(
                    self.catalog.as_ref(),
                    &insert.table,
                    txn_id,
                    default_schema,
                )? {
                    let ordered = view
                        .columns
                        .iter()
                        .filter_map(|view_col| {
                            map.get(&view_col.name.to_ascii_lowercase())
                                .and_then(|base| relation.column_by_name(base).cloned())
                        })
                        .collect::<Vec<_>>();
                    if !ordered.is_empty() {
                        implicit_input_columns = Some(ordered);
                    }
                }
            }
        }

        for column_name in &insert.columns {
            let column = unqualified_column_name(column_name)?;
            let mapped = if let Some(ref map) = col_map {
                map.get(&column.to_ascii_lowercase())
                    .map_or(column, |s| s.as_str())
            } else {
                column
            };
            // Note: we intentionally do NOT reject duplicate column names here.
            // The parser may produce duplicates when composite-field or
            // array-subscript targets reference the same base column (e.g.
            // f3.if1 and f3.if2, or f2[1] and f2[2]).  PostgreSQL merges these
            // internally.  Keeping duplicates in the columns list ensures the
            // count matches the expressions count for type checking.

            columns.push(relation.column_by_name(mapped).cloned().ok_or_else(|| {
                undefined_column_of_relation(
                    column_name.span.start + 1,
                    column,
                    relation.name.object_name(),
                )
            })?);
        }

        let returning = bind_returning(
            &insert.returning,
            &relation,
            col_map.as_ref(),
            insert.table_alias.as_deref(),
        )?;

        let on_conflict = insert
            .on_conflict
            .as_ref()
            .map(|clause| {
                if let Some(ref map) = col_map {
                    let view = find_target_view_descriptor(
                        self.catalog.as_ref(),
                        &insert.table,
                        txn_id,
                        default_schema,
                    )?;
                    let expr_map = view
                        .as_ref()
                        .map(|descriptor| build_view_on_conflict_expr_map(descriptor, map))
                        .transpose()?
                        .unwrap_or_default();
                    Ok(rewrite_view_on_conflict(
                        clause,
                        map,
                        &expr_map,
                        insert.table_alias.as_deref(),
                        insert.table.parts.last().map(String::as_str),
                        relation.name.object_name(),
                    ))
                } else {
                    Ok(clause.clone())
                }
            })
            .transpose()?;

        Ok(BoundInsert {
            relation,
            columns,
            implicit_input_columns,
            rows: insert.rows.clone(),
            query: insert
                .query
                .as_ref()
                .map(|query| self.bind_select(query, txn_id, default_schema))
                .transpose()?,
            on_conflict,
            returning,
        })
    }

    pub(super) fn bind_copy(
        &self,
        copy: &CopyStatement,
        txn_id: TxnId,
        default_schema: Option<&str>,
    ) -> DbResult<BoundCopy> {
        let mut relation_name = relation_error_name(&copy.table, default_schema)?;
        let mut relation = None;
        for candidate in relation_lookup_candidates(&copy.table, default_schema)? {
            relation_name = candidate;
            if let Some(table) = self.catalog.get_table(txn_id, &relation_name)? {
                relation = Some(table);
                break;
            }
            if let Some(view) = self.catalog.get_view(txn_id, &relation_name)? {
                let view_name = relation_name.object_name();
                if copy.direction == CopyDirection::From {
                    let trigger_target = relation_name.to_string();
                    let mut triggers = self.catalog.list_triggers(txn_id, &trigger_target)?;
                    if triggers.is_empty() {
                        triggers = self.catalog.list_triggers(txn_id, view_name)?;
                    }
                    let has_instead_of_insert = triggers.iter().any(|trigger| {
                        trigger.timing == TriggerTimingDescriptor::InsteadOf
                            && trigger.event == TriggerEventDescriptor::Insert
                    });
                    if has_instead_of_insert {
                        relation = Some(build_view_table_descriptor(&view));
                        break;
                    }
                    return Err(DbError::bind_error(
                        SqlState::WrongObjectType,
                        format!("cannot copy to view \"{view_name}\""),
                    )
                    .with_client_hint(
                        "To enable copying to a view, provide an INSTEAD OF INSERT trigger.",
                    ));
                }
                return Err(DbError::bind_error(
                    SqlState::WrongObjectType,
                    format!("cannot copy from view \"{view_name}\""),
                )
                .with_client_hint("Try the COPY (SELECT ...) TO variant."));
            }
        }
        let relation = relation.ok_or_else(|| undefined_table(&copy.table, &relation_name))?;

        let columns = if copy.columns.is_empty() {
            relation.columns.clone()
        } else {
            let mut cols = Vec::with_capacity(copy.columns.len());
            let mut seen = std::collections::HashSet::new();
            for col_name in &copy.columns {
                if !seen.insert(col_name.to_ascii_lowercase()) {
                    return Err(DbError::bind_error(
                        SqlState::DuplicateColumn,
                        format!("column \"{col_name}\" specified more than once"),
                    ));
                }
                cols.push(relation.column_by_name(col_name).cloned().ok_or_else(|| {
                    DbError::bind_error(
                        SqlState::UndefinedColumn,
                        format!(
                            "column \"{col_name}\" of relation \"{}\" does not exist",
                            relation.name.object_name()
                        ),
                    )
                })?);
            }
            cols
        };

        Ok(BoundCopy {
            relation,
            columns,
            direction: copy.direction,
        })
    }

    pub(super) fn bind_delete(
        &self,
        delete: &DeleteStatement,
        txn_id: TxnId,
        default_schema: Option<&str>,
    ) -> DbResult<BoundDelete> {
        let (relation, view_info) =
            self.resolve_dml_target(&delete.table, txn_id, default_schema, DmlKind::Delete)?;
        let col_map = view_info.as_ref().map(|i| i.col_map.clone());

        let using_tables =
            self.resolve_dml_extra_tables(&delete.using_tables, txn_id, default_schema)?;

        let returning = bind_returning(
            &delete.returning,
            &relation,
            col_map.as_ref(),
            delete.table_alias.as_deref(),
        )?;

        // Rewrite WHERE clause if target is a view - map view aliases to
        // underlying table column names.
        let selection = if let Some(ref info) = view_info {
            let user_selection = delete
                .selection
                .as_ref()
                .map(|sel| rewrite_view_expr(sel, &info.col_map));
            combine_view_dml_predicates(user_selection, &info.qualifier_predicates)
        } else {
            delete.selection.clone()
        };

        Ok(BoundDelete {
            relation,
            table_alias: delete.table_alias.clone(),
            using_tables,
            selection,
            returning,
        })
    }

    pub(super) fn bind_update(
        &self,
        update: &UpdateStatement,
        txn_id: TxnId,
        default_schema: Option<&str>,
    ) -> DbResult<BoundUpdate> {
        // PR1 of multi-PR series wiring CTE-in-UPDATE-FROM end-to-end:
        // the binder now eagerly binds every CTE referenced from the
        // FROM list and stashes the validated `BoundSelect` on
        // `BoundUpdate.cte_sources`. Lowering through LogicalPlan and
        // executor materialisation lands in PR2 — until then, the
        // planner detects this populated field and surfaces a clean
        // `feature_not_supported` with the bound CTE name, instead of
        // the historical "relation does not exist" lookup error.
        let mut cte_sources: Vec<(String, BoundSelect)> = Vec::new();
        if !update.ctes.is_empty() {
            for (from_name, _) in &update.from_tables {
                let last = from_name.parts.last().map_or("", String::as_str);
                if let Some(cte) = update
                    .ctes
                    .iter()
                    .find(|cte| cte.name.eq_ignore_ascii_case(last))
                {
                    if cte.recursive {
                        return Err(DbError::feature_not_supported(format!(
                            "RECURSIVE CTE `{}` cannot be referenced from UPDATE … FROM yet",
                            cte.name
                        )));
                    }
                    let select = match cte.query.as_ref() {
                        Statement::Select(select) => select,
                        _ => {
                            return Err(DbError::feature_not_supported(format!(
                                "CTE `{}` body must be a SELECT to be used as an UPDATE … FROM source",
                                cte.name
                            )));
                        }
                    };
                    let bound = self.bind_select(select, txn_id, default_schema)?;
                    cte_sources.push((cte.name.clone(), bound));
                }
            }
        }
        let (relation, view_info) =
            self.resolve_dml_target(&update.table, txn_id, default_schema, DmlKind::Update)?;
        if let Some(ref info) = view_info {
            for assignment in &update.assignments {
                reject_non_updatable_column(info, &assignment.column, true)?;
            }
            // Detect "multiple assignments to same column" through view aliases that
            // map to the same base column.
            let mut seen: std::collections::HashMap<String, String> =
                std::collections::HashMap::new();
            for assignment in &update.assignments {
                let key = assignment.column.to_ascii_lowercase();
                let target = info
                    .col_map
                    .get(&key)
                    .cloned()
                    .unwrap_or_else(|| key.clone())
                    .to_ascii_lowercase();
                if let Some(prev_alias) = seen.get(&target) {
                    return Err(DbError::feature_not_supported(format!(
                        "multiple assignments to same column \"{prev_alias}\""
                    )));
                }
                seen.insert(target, assignment.column.clone());
            }
        }
        let col_map = view_info.as_ref().map(|i| i.col_map.clone());

        let assignments = update
            .assignments
            .iter()
            .map(|assignment| {
                let col_name = &assignment.column;
                let mapped = if let Some(ref map) = col_map {
                    map.get(&col_name.to_ascii_lowercase())
                        .map_or(col_name.as_str(), |s| s.as_str())
                } else {
                    col_name.as_str()
                };
                let column = relation.column_by_name(mapped).cloned().ok_or_else(|| {
                    undefined_column_of_relation(
                        assignment.span.start + 1,
                        &assignment.column,
                        relation.name.object_name(),
                    )
                })?;
                // Rewrite value expressions if target is a view
                let expr = if let Some(ref map) = col_map {
                    rewrite_view_expr(&assignment.expr, map)
                } else {
                    assignment.expr.clone()
                };
                Ok(BoundUpdateAssignment { column, expr })
            })
            .collect::<DbResult<Vec<_>>>()?;

        let from_tables = self.resolve_dml_extra_tables_with_ctes(
            &update.from_tables,
            &cte_sources,
            txn_id,
            default_schema,
        )?;

        let returning = bind_returning(
            &update.returning,
            &relation,
            col_map.as_ref(),
            update.table_alias.as_deref(),
        )?;

        // Rewrite WHERE clause if target is a view - map view aliases to
        // underlying table column names.
        let selection = if let Some(ref info) = view_info {
            let user_selection = update
                .selection
                .as_ref()
                .map(|sel| rewrite_view_expr(sel, &info.col_map));
            combine_view_dml_predicates(user_selection, &info.qualifier_predicates)
        } else {
            update.selection.clone()
        };

        Ok(BoundUpdate {
            relation,
            table_alias: update.table_alias.clone(),
            assignments,
            from_tables,
            selection,
            returning,
            cte_sources,
        })
    }

    pub(super) fn bind_merge(
        &self,
        merge: &aiondb_parser::MergeStatement,
        txn_id: TxnId,
        default_schema: Option<&str>,
    ) -> DbResult<BoundMerge> {
        let mut target_name = relation_error_name(&merge.target_table, default_schema)?;
        let mut target = None;
        for candidate in relation_lookup_candidates(&merge.target_table, default_schema)? {
            target_name = candidate;
            if let Some(table) = self.catalog.get_table(txn_id, &target_name)? {
                target = Some(table);
                break;
            }
        }
        let target = target.ok_or_else(|| undefined_table(&merge.target_table, &target_name))?;

        let source = match &merge.source {
            aiondb_parser::ast::MergeSource::Table(source_table) => {
                let mut source_name = relation_error_name(source_table, default_schema)?;
                let mut source = None;
                for candidate in relation_lookup_candidates(source_table, default_schema)? {
                    source_name = candidate;
                    if let Some(table) = self.catalog.get_table(txn_id, &source_name)? {
                        source = Some(table);
                        break;
                    }
                }
                BoundMergeSource::Table(
                    source.ok_or_else(|| undefined_table(source_table, &source_name))?,
                )
            }
            aiondb_parser::ast::MergeSource::Subquery(subquery) => {
                let query = self.bind_select(subquery, txn_id, default_schema)?;
                let relation = self
                    .build_merge_subquery_source_relation(&query, merge.source_alias.as_deref())?;
                BoundMergeSource::Subquery { relation, query }
            }
        };

        let mut when_clauses = Vec::with_capacity(merge.when_clauses.len());
        for clause in &merge.when_clauses {
            if let Some(condition) = &clause.condition {
                if let Some((column_name, position)) = find_merge_system_column(condition) {
                    return Err(DbError::Bind(Box::new(
                        ErrorReport::new(
                            SqlState::SyntaxError,
                            format!(
                                "cannot use system column \"{column_name}\" in MERGE WHEN condition"
                            ),
                        )
                        .with_position(position),
                    )));
                }
            }
            let action = match &clause.action {
                MergeAction::Update { assignments } => {
                    let bound_assignments = assignments
                        .iter()
                        .map(|assignment| {
                            let column = target
                                .column_by_name(&assignment.column)
                                .cloned()
                                .ok_or_else(|| {
                                    undefined_column(assignment.span.start + 1, &assignment.column)
                                })?;
                            Ok(BoundUpdateAssignment {
                                column,
                                expr: assignment.expr.clone(),
                            })
                        })
                        .collect::<DbResult<Vec<_>>>()?;
                    BoundMergeAction::Update {
                        assignments: bound_assignments,
                    }
                }
                MergeAction::Delete => BoundMergeAction::Delete,
                MergeAction::InsertDefaultValues => BoundMergeAction::InsertDefaultValues,
                MergeAction::DoNothing => BoundMergeAction::DoNothing,
                MergeAction::Insert { columns, values } => {
                    let bound_columns = if columns.is_empty() {
                        target.columns.clone()
                    } else {
                        columns
                            .iter()
                            .map(|col_name| {
                                target
                                    .column_by_name(col_name)
                                    .cloned()
                                    .ok_or_else(|| undefined_column(merge.span.start + 1, col_name))
                            })
                            .collect::<DbResult<Vec<_>>>()?
                    };
                    BoundMergeAction::Insert {
                        columns: bound_columns,
                        values: values.clone(),
                    }
                }
            };
            when_clauses.push(BoundMergeWhenClause {
                matched: clause.matched,
                condition: clause.condition.clone(),
                action,
            });
        }

        Ok(BoundMerge {
            target,
            source,
            target_alias: merge.target_alias.clone(),
            source_alias: merge.source_alias.clone(),
            on_condition: merge.on_condition.clone(),
            when_clauses,
        })
    }

    fn build_merge_subquery_source_relation(
        &self,
        query: &BoundSelect,
        source_alias: Option<&str>,
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
        let typed = TypeChecker::new(Arc::clone(&self.catalog))
            .with_session_context(current_user, session_user, current_schema, current_database)
            .with_search_path_schemas(search_path_schemas)
            .type_check_select(query)?;
        let columns = typed
            .outputs
            .iter()
            .enumerate()
            .map(|(i, output)| {
                let visible_name = output
                    .field
                    .name
                    .rsplit('\0')
                    .next()
                    .unwrap_or(&output.field.name)
                    .to_owned();
                Ok(ColumnDescriptor {
                    column_id: aiondb_core::ColumnId::new(
                        u64::try_from(i).unwrap_or(u64::MAX).saturating_add(1),
                    ),
                    name: visible_name,
                    data_type: output.field.data_type.clone(),
                    raw_type_name: None,
                    text_type_modifier: None,
                    nullable: output.field.nullable,
                    ordinal_position: u32::try_from(i).unwrap_or(u32::MAX).saturating_add(1),
                    default_value: None,
                })
            })
            .collect::<DbResult<Vec<_>>>()?;

        let base_relation = query.relation.as_ref();
        let source_name = source_alias.unwrap_or_else(|| {
            base_relation
                .map(|relation| relation.name.object_name())
                .unwrap_or("__merge_source")
        });

        Ok(TableDescriptor {
            table_id: base_relation.map_or(RelationId::new(0), |relation| relation.table_id),
            schema_id: base_relation.map_or(SchemaId::new(0), |relation| relation.schema_id),
            name: QualifiedName::unqualified(source_name),
            columns,
            identity_columns: Vec::new(),
            primary_key: None,
            foreign_keys: Vec::new(),
            check_constraints: Vec::new(),
            shard_config: None,
            owner: None,
        })
    }

    pub(super) fn bind_set_operation(
        &self,
        set_op: &SetOperationStatement,
        txn_id: TxnId,
        default_schema: Option<&str>,
    ) -> DbResult<BoundSetOperation> {
        fn leftmost_ctes(statement: &Statement) -> Option<&[aiondb_parser::CteDefinition]> {
            match statement {
                Statement::Select(select) if !select.ctes.is_empty() => Some(&select.ctes),
                Statement::SetOperation(inner) => leftmost_ctes(inner.left.as_ref()),
                _ => None,
            }
        }

        let left = self.bind(&set_op.left, txn_id, default_schema)?;
        let right_statement = if let Some(parent_ctes) = leftmost_ctes(set_op.left.as_ref()) {
            cte::inject_parent_ctes(set_op.right.as_ref(), parent_ctes)
        } else {
            set_op.right.as_ref().clone()
        };
        let right = self.bind(&right_statement, txn_id, default_schema)?;
        let order_by = set_op
            .order_by
            .iter()
            .map(|item| BoundOrderBy {
                expr: item.expr.clone(),
                descending: item.descending,
                nulls_first: item.nulls_first,
            })
            .collect();
        Ok(BoundSetOperation {
            op: set_op.op,
            all: set_op.all,
            left: Box::new(left),
            right: Box::new(right),
            order_by,
            limit: set_op.limit.clone(),
            offset: set_op.offset.clone(),
        })
    }
}

fn combine_view_dml_predicates(selection: Option<Expr>, qualifiers: &[Expr]) -> Option<Expr> {
    qualifiers
        .iter()
        .cloned()
        .fold(selection, |existing, qualifier| match existing {
            Some(existing_expr) => Some(Expr::BinaryOp {
                left: Box::new(existing_expr),
                op: aiondb_parser::BinaryOperator::And,
                right: Box::new(qualifier.clone()),
                span: qualifier.span(),
            }),
            None => Some(qualifier),
        })
}

fn find_merge_system_column(expr: &Expr) -> Option<(String, usize)> {
    match expr {
        Expr::Identifier(name) => {
            let candidate = name.parts.last()?;
            if matches!(candidate.to_ascii_lowercase().as_str(), "xmin" | "xmax") {
                return Some((candidate.clone(), name.span.start + 1));
            }
            None
        }
        Expr::UnaryOp { expr, .. } | Expr::IsNull { expr, .. } => find_merge_system_column(expr),
        Expr::BinaryOp { left, right, .. }
        | Expr::IsDistinctFrom { left, right, .. }
        | Expr::Like {
            expr: left,
            pattern: right,
            ..
        } => find_merge_system_column(left).or_else(|| find_merge_system_column(right)),
        Expr::InList { expr, list, .. } => find_merge_system_column(expr)
            .or_else(|| list.iter().find_map(find_merge_system_column)),
        Expr::Between {
            expr, low, high, ..
        } => find_merge_system_column(expr)
            .or_else(|| find_merge_system_column(low))
            .or_else(|| find_merge_system_column(high)),
        Expr::Cast { expr, .. } => find_merge_system_column(expr),
        Expr::CaseWhen {
            operand,
            conditions,
            results,
            else_result,
            ..
        } => operand
            .as_deref()
            .and_then(find_merge_system_column)
            .or_else(|| conditions.iter().find_map(find_merge_system_column))
            .or_else(|| results.iter().find_map(find_merge_system_column))
            .or_else(|| else_result.as_deref().and_then(find_merge_system_column)),
        Expr::Array { elements, .. } => elements.iter().find_map(find_merge_system_column),
        Expr::FunctionCall { args, filter, .. } => args
            .iter()
            .find_map(find_merge_system_column)
            .or_else(|| filter.as_deref().and_then(find_merge_system_column)),
        Expr::WindowFunction {
            function,
            partition_by,
            order_by,
            ..
        } => find_merge_system_column(function)
            .or_else(|| partition_by.iter().find_map(find_merge_system_column))
            .or_else(|| {
                order_by
                    .iter()
                    .find_map(|item| find_merge_system_column(&item.expr))
            }),
        Expr::InSubquery { expr, .. } => find_merge_system_column(expr),
        Expr::Literal(_, _)
        | Expr::Parameter { .. }
        | Expr::Default { .. }
        | Expr::ArraySubquery { .. }
        | Expr::Subquery { .. }
        | Expr::Exists { .. }
        | Expr::CypherExists { .. }
        | Expr::CypherPatternComprehension { .. } => None,
    }
}

fn find_target_view_descriptor(
    catalog: &dyn CatalogReader,
    name: &ObjectName,
    txn_id: TxnId,
    default_schema: Option<&str>,
) -> DbResult<Option<ViewDescriptor>> {
    for candidate in relation_lookup_candidates(name, default_schema)? {
        if let Some(view) = catalog.get_view(txn_id, &candidate)? {
            return Ok(Some(view));
        }
    }
    Ok(None)
}

fn build_view_on_conflict_expr_map(
    view: &ViewDescriptor,
    col_map: &std::collections::HashMap<String, String>,
) -> DbResult<std::collections::HashMap<String, Expr>> {
    let stmts = aiondb_parser::parse_sql(&view.query_sql)?;
    let Some(Statement::Select(select)) = stmts.first() else {
        return Ok(std::collections::HashMap::new());
    };
    let mut expr_map = std::collections::HashMap::new();
    for (index, item) in select.items.iter().enumerate() {
        let Some(view_col) = view.columns.get(index) else {
            continue;
        };
        expr_map.insert(
            view_col.name.to_ascii_lowercase(),
            rewrite_view_expr(&item.expr, col_map),
        );
    }
    Ok(expr_map)
}

fn rewrite_view_on_conflict(
    clause: &OnConflict,
    col_map: &std::collections::HashMap<String, String>,
    expr_map: &std::collections::HashMap<String, Expr>,
    table_alias: Option<&str>,
    view_name: Option<&str>,
    base_table_name: &str,
) -> OnConflict {
    let mut view_qualifiers = std::collections::HashSet::new();
    if let Some(alias) = table_alias {
        view_qualifiers.insert(alias.to_ascii_lowercase());
    }
    if let Some(name) = view_name {
        view_qualifiers.insert(name.to_ascii_lowercase());
    }

    let columns = clause
        .columns
        .iter()
        .map(|column: &String| {
            col_map
                .get(&column.to_ascii_lowercase())
                .cloned()
                .unwrap_or_else(|| column.clone())
        })
        .collect();

    let action = match &clause.action {
        OnConflictAction::DoNothing => OnConflictAction::DoNothing,
        OnConflictAction::DoUpdate {
            assignments,
            where_clause,
        } => OnConflictAction::DoUpdate {
            assignments: assignments
                .iter()
                .map(|assignment| UpdateAssignment {
                    column: col_map
                        .get(&assignment.column.to_ascii_lowercase())
                        .cloned()
                        .unwrap_or_else(|| assignment.column.clone()),
                    expr: rewrite_view_on_conflict_expr(
                        &assignment.expr,
                        col_map,
                        expr_map,
                        &view_qualifiers,
                        base_table_name,
                    ),
                    span: assignment.span,
                })
                .collect(),
            where_clause: where_clause.as_ref().map(|expr| {
                rewrite_view_on_conflict_expr(
                    expr,
                    col_map,
                    expr_map,
                    &view_qualifiers,
                    base_table_name,
                )
            }),
        },
    };

    OnConflict { columns, action }
}

fn rewrite_view_on_conflict_expr(
    expr: &Expr,
    col_map: &std::collections::HashMap<String, String>,
    expr_map: &std::collections::HashMap<String, Expr>,
    view_qualifiers: &std::collections::HashSet<String>,
    base_table_name: &str,
) -> Expr {
    match expr {
        Expr::Identifier(name) => {
            if name.parts.len() == 1 {
                let lower = name.parts[0].to_ascii_lowercase();
                if let Some(mapped) = col_map.get(&lower) {
                    return Expr::Identifier(ObjectName {
                        parts: vec![mapped.clone()],
                        span: name.span,
                    });
                }
                if let Some(mapped_expr) = expr_map.get(&lower) {
                    return qualify_view_projection_expr(mapped_expr, None, base_table_name);
                }
                return expr.clone();
            }
            if name.parts.len() == 2 {
                let qualifier = name.parts[0].to_ascii_lowercase();
                let column = name.parts[1].to_ascii_lowercase();
                if qualifier == "excluded" {
                    if let Some(mapped_expr) = expr_map.get(&column) {
                        return qualify_view_projection_expr(
                            mapped_expr,
                            Some("excluded"),
                            base_table_name,
                        );
                    }
                    if let Some(mapped) = col_map.get(&column) {
                        return Expr::Identifier(ObjectName {
                            parts: vec!["excluded".to_owned(), mapped.clone()],
                            span: name.span,
                        });
                    }
                }
                if view_qualifiers.contains(&qualifier) {
                    if let Some(mapped_expr) = expr_map.get(&column) {
                        return qualify_view_projection_expr(
                            mapped_expr,
                            Some(base_table_name),
                            base_table_name,
                        );
                    }
                    if let Some(mapped) = col_map.get(&column) {
                        return Expr::Identifier(ObjectName {
                            parts: vec![base_table_name.to_owned(), mapped.clone()],
                            span: name.span,
                        });
                    }
                }
                if let Some(mapped) = col_map.get(&column) {
                    return Expr::Identifier(ObjectName {
                        parts: vec![name.parts[0].clone(), mapped.clone()],
                        span: name.span,
                    });
                }
                return expr.clone();
            }
            expr.clone()
        }
        Expr::BinaryOp {
            left,
            op,
            right,
            span,
        } => Expr::BinaryOp {
            left: Box::new(rewrite_view_on_conflict_expr(
                left,
                col_map,
                expr_map,
                view_qualifiers,
                base_table_name,
            )),
            op: op.clone(),
            right: Box::new(rewrite_view_on_conflict_expr(
                right,
                col_map,
                expr_map,
                view_qualifiers,
                base_table_name,
            )),
            span: *span,
        },
        Expr::UnaryOp {
            op,
            expr: inner,
            span,
        } => Expr::UnaryOp {
            op: op.clone(),
            expr: Box::new(rewrite_view_on_conflict_expr(
                inner,
                col_map,
                expr_map,
                view_qualifiers,
                base_table_name,
            )),
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
                .map(|arg| {
                    rewrite_view_on_conflict_expr(
                        arg,
                        col_map,
                        expr_map,
                        view_qualifiers,
                        base_table_name,
                    )
                })
                .collect(),
            distinct: *distinct,
            filter: filter.as_ref().map(|expr| {
                Box::new(rewrite_view_on_conflict_expr(
                    expr,
                    col_map,
                    expr_map,
                    view_qualifiers,
                    base_table_name,
                ))
            }),
            span: *span,
        },
        Expr::Cast {
            expr: inner,
            data_type,
            span,
        } => Expr::Cast {
            expr: Box::new(rewrite_view_on_conflict_expr(
                inner,
                col_map,
                expr_map,
                view_qualifiers,
                base_table_name,
            )),
            data_type: data_type.clone(),
            span: *span,
        },
        Expr::IsNull {
            expr: inner,
            negated,
            span,
        } => Expr::IsNull {
            expr: Box::new(rewrite_view_on_conflict_expr(
                inner,
                col_map,
                expr_map,
                view_qualifiers,
                base_table_name,
            )),
            negated: *negated,
            span: *span,
        },
        Expr::IsDistinctFrom {
            left,
            right,
            negated,
            span,
        } => Expr::IsDistinctFrom {
            left: Box::new(rewrite_view_on_conflict_expr(
                left,
                col_map,
                expr_map,
                view_qualifiers,
                base_table_name,
            )),
            right: Box::new(rewrite_view_on_conflict_expr(
                right,
                col_map,
                expr_map,
                view_qualifiers,
                base_table_name,
            )),
            negated: *negated,
            span: *span,
        },
        Expr::Like {
            expr: inner,
            pattern,
            negated,
            case_insensitive,
            span,
        } => Expr::Like {
            expr: Box::new(rewrite_view_on_conflict_expr(
                inner,
                col_map,
                expr_map,
                view_qualifiers,
                base_table_name,
            )),
            pattern: Box::new(rewrite_view_on_conflict_expr(
                pattern,
                col_map,
                expr_map,
                view_qualifiers,
                base_table_name,
            )),
            negated: *negated,
            case_insensitive: *case_insensitive,
            span: *span,
        },
        Expr::Between {
            expr: inner,
            low,
            high,
            negated,
            span,
        } => Expr::Between {
            expr: Box::new(rewrite_view_on_conflict_expr(
                inner,
                col_map,
                expr_map,
                view_qualifiers,
                base_table_name,
            )),
            low: Box::new(rewrite_view_on_conflict_expr(
                low,
                col_map,
                expr_map,
                view_qualifiers,
                base_table_name,
            )),
            high: Box::new(rewrite_view_on_conflict_expr(
                high,
                col_map,
                expr_map,
                view_qualifiers,
                base_table_name,
            )),
            negated: *negated,
            span: *span,
        },
        Expr::InList {
            expr: inner,
            list,
            negated,
            span,
        } => Expr::InList {
            expr: Box::new(rewrite_view_on_conflict_expr(
                inner,
                col_map,
                expr_map,
                view_qualifiers,
                base_table_name,
            )),
            list: list
                .iter()
                .map(|item| {
                    rewrite_view_on_conflict_expr(
                        item,
                        col_map,
                        expr_map,
                        view_qualifiers,
                        base_table_name,
                    )
                })
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
            operand: operand.as_ref().map(|expr| {
                Box::new(rewrite_view_on_conflict_expr(
                    expr,
                    col_map,
                    expr_map,
                    view_qualifiers,
                    base_table_name,
                ))
            }),
            conditions: conditions
                .iter()
                .map(|expr| {
                    rewrite_view_on_conflict_expr(
                        expr,
                        col_map,
                        expr_map,
                        view_qualifiers,
                        base_table_name,
                    )
                })
                .collect(),
            results: results
                .iter()
                .map(|expr| {
                    rewrite_view_on_conflict_expr(
                        expr,
                        col_map,
                        expr_map,
                        view_qualifiers,
                        base_table_name,
                    )
                })
                .collect(),
            else_result: else_result.as_ref().map(|expr| {
                Box::new(rewrite_view_on_conflict_expr(
                    expr,
                    col_map,
                    expr_map,
                    view_qualifiers,
                    base_table_name,
                ))
            }),
            span: *span,
        },
        Expr::Array { elements, span } => Expr::Array {
            elements: elements
                .iter()
                .map(|expr| {
                    rewrite_view_on_conflict_expr(
                        expr,
                        col_map,
                        expr_map,
                        view_qualifiers,
                        base_table_name,
                    )
                })
                .collect(),
            span: *span,
        },
        _ => expr.clone(),
    }
}

fn qualify_view_projection_expr(
    expr: &Expr,
    qualifier: Option<&str>,
    base_table_name: &str,
) -> Expr {
    match expr {
        Expr::Identifier(name) => {
            if let Some(qualifier) = qualifier {
                if name.parts.len() == 1 {
                    if name.parts[0].eq_ignore_ascii_case(base_table_name) {
                        return Expr::Identifier(ObjectName {
                            parts: vec![qualifier.to_owned(), "*".to_owned()],
                            span: name.span,
                        });
                    }
                    return Expr::Identifier(ObjectName {
                        parts: vec![qualifier.to_owned(), name.parts[0].clone()],
                        span: name.span,
                    });
                }
                if name.parts.len() == 2 && name.parts[0].eq_ignore_ascii_case(base_table_name) {
                    return Expr::Identifier(ObjectName {
                        parts: vec![qualifier.to_owned(), name.parts[1].clone()],
                        span: name.span,
                    });
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
            left: Box::new(qualify_view_projection_expr(
                left,
                qualifier,
                base_table_name,
            )),
            op: op.clone(),
            right: Box::new(qualify_view_projection_expr(
                right,
                qualifier,
                base_table_name,
            )),
            span: *span,
        },
        Expr::UnaryOp {
            op,
            expr: inner,
            span,
        } => Expr::UnaryOp {
            op: op.clone(),
            expr: Box::new(qualify_view_projection_expr(
                inner,
                qualifier,
                base_table_name,
            )),
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
                .map(|arg| qualify_view_projection_expr(arg, qualifier, base_table_name))
                .collect(),
            distinct: *distinct,
            filter: filter.as_ref().map(|expr| {
                Box::new(qualify_view_projection_expr(
                    expr,
                    qualifier,
                    base_table_name,
                ))
            }),
            span: *span,
        },
        Expr::Cast {
            expr: inner,
            data_type,
            span,
        } => Expr::Cast {
            expr: Box::new(qualify_view_projection_expr(
                inner,
                qualifier,
                base_table_name,
            )),
            data_type: data_type.clone(),
            span: *span,
        },
        Expr::IsNull {
            expr: inner,
            negated,
            span,
        } => Expr::IsNull {
            expr: Box::new(qualify_view_projection_expr(
                inner,
                qualifier,
                base_table_name,
            )),
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
            operand: operand.as_ref().map(|expr| {
                Box::new(qualify_view_projection_expr(
                    expr,
                    qualifier,
                    base_table_name,
                ))
            }),
            conditions: conditions
                .iter()
                .map(|expr| qualify_view_projection_expr(expr, qualifier, base_table_name))
                .collect(),
            results: results
                .iter()
                .map(|expr| qualify_view_projection_expr(expr, qualifier, base_table_name))
                .collect(),
            else_result: else_result.as_ref().map(|expr| {
                Box::new(qualify_view_projection_expr(
                    expr,
                    qualifier,
                    base_table_name,
                ))
            }),
            span: *span,
        },
        _ => expr.clone(),
    }
}
