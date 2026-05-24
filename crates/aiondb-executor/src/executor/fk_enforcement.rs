use std::cell::RefCell;
use std::collections::HashMap;

use aiondb_catalog::ForeignKeyConstraint;

use super::*;

/// Pre-resolved metadata for a single child-table FK pointing back at
/// the relation being UPDATEd. Built once per UPDATE statement when
/// `has_referencing_update_fks` is true, then consumed in the per-row
/// hot path to skip the catalog walk and column-name resolution that
/// the original API paid on every tuple.
#[derive(Clone)]
pub(crate) struct ReferencingUpdateFkEntry {
    pub child_table: TableDescriptor,
    pub fk: ForeignKeyConstraint,
    pub parent_ordinals: Vec<usize>,
    pub child_ordinals: Vec<usize>,
}

/// Pre-resolved metadata for a single child-side FK on the relation
/// being UPDATEd. Built once per statement so the per-row check skips
/// the `resolve_column_ordinals` linear search, the catalog
/// `get_table` for the referenced table, and the second
/// `resolve_column_ordinals` against the referenced table that
/// `check_fk_values_exist` would otherwise repeat on every modified
/// tuple. Mirrors the PostgreSQL `RI_QueryHashEntry` cache that holds
/// the same shape per FK constraint.
#[derive(Clone)]
pub(crate) struct CompiledChildFkCheck {
    pub fk: ForeignKeyConstraint,
    pub fk_ordinals: Vec<usize>,
    pub referenced_table: TableDescriptor,
    pub referenced_ordinals: Vec<usize>,
}

thread_local! {
    /// Per-thread cache for `table_has_referencing_update_foreign_keys`.
    /// Keyed by `(catalog_revision, table_id)` so it auto-invalidates as
    /// soon as any DDL bumps the catalog revision. The original probe
    /// listed every schema and every table on every UPDATE just to find
    /// out whether any other relation had a FK pointing at the table
    /// being updated - a constant cost that scaled with the size of the
    /// catalog, not with the workload, and was the dominant cost in the
    /// OLTP UPDATE path before this cache.
    static REFERENCING_UPDATE_FK_CACHE: RefCell<HashMap<(u64, RelationId), bool>> =
        RefCell::new(HashMap::new());

    /// Same idea for DELETE: cache "are there any FKs pointing at this
    /// table at all?" so the heavy `enforce_fk_on_delete` loop becomes
    /// a cheap negative-result early-exit when the table has no
    /// referencing FKs (the typical case in OLTP workloads).
    static REFERENCING_DELETE_FK_CACHE: RefCell<HashMap<(u64, RelationId), bool>> =
        RefCell::new(HashMap::new());
}

impl Executor {
    pub(super) fn table_has_referencing_update_foreign_keys(
        &self,
        table_id: RelationId,
        context: &ExecutionContext,
    ) -> DbResult<bool> {
        let revision = self.catalog_reader.catalog_revision(context.txn_id)?;
        if let Some(cached) = REFERENCING_UPDATE_FK_CACHE
            .with(|cache| cache.borrow().get(&(revision, table_id)).copied())
        {
            return Ok(cached);
        }

        let table = self
            .catalog_reader
            .get_table_by_id(context.txn_id, table_id)?
            .ok_or_else(|| DbError::internal("table not found for FK parent-update probe"))?;

        let all_tables = self.list_all_tables(context)?;
        let result = all_tables.iter().any(|child_table| {
            child_table.foreign_keys.iter().any(|fk| {
                fk_references_table(fk, &table.name)
                    && matches!(
                        fk.on_update,
                        aiondb_core::FkAction::Restrict
                            | aiondb_core::FkAction::NoAction
                            | aiondb_core::FkAction::Cascade
                            | aiondb_core::FkAction::SetNull
                            | aiondb_core::FkAction::SetDefault
                    )
            })
        });

        REFERENCING_UPDATE_FK_CACHE.with(|cache| {
            let mut cache = cache.borrow_mut();
            // Bound the cache so a long-lived session that sees a steady
            // stream of revisions/tables doesn't grow it unboundedly. 256
            // entries comfortably cover any realistic schema; older
            // entries are dropped wholesale once we exceed the cap.
            if cache.len() >= 256 {
                cache.clear();
            }
            cache.insert((revision, table_id), result);
        });

        Ok(result)
    }

    pub(super) fn table_has_referencing_delete_foreign_keys(
        &self,
        table_id: RelationId,
        context: &ExecutionContext,
    ) -> DbResult<bool> {
        let revision = self.catalog_reader.catalog_revision(context.txn_id)?;
        if let Some(cached) = REFERENCING_DELETE_FK_CACHE
            .with(|cache| cache.borrow().get(&(revision, table_id)).copied())
        {
            return Ok(cached);
        }

        let table = self
            .catalog_reader
            .get_table_by_id(context.txn_id, table_id)?
            .ok_or_else(|| DbError::internal("table not found for FK parent-delete probe"))?;

        let all_tables = self.list_all_tables(context)?;
        let result = all_tables.iter().any(|child_table| {
            child_table
                .foreign_keys
                .iter()
                .any(|fk| fk_references_table(fk, &table.name))
        });

        REFERENCING_DELETE_FK_CACHE.with(|cache| {
            let mut cache = cache.borrow_mut();
            if cache.len() >= 256 {
                cache.clear();
            }
            cache.insert((revision, table_id), result);
        });

        Ok(result)
    }

    /// Enforce foreign key constraints on INSERT.
    ///
    /// For each FK constraint on the table being inserted into, verify that
    /// the values in the FK columns exist in the referenced table's referenced
    /// columns.
    pub(super) fn enforce_fk_on_insert(
        &self,
        table_id: RelationId,
        row_values: &[Value],
        context: &ExecutionContext,
    ) -> DbResult<()> {
        let table = self
            .catalog_reader
            .get_table_by_id(context.txn_id, table_id)?
            .ok_or_else(|| DbError::internal("table not found for FK check"))?;

        if table.foreign_keys.is_empty() {
            return Ok(());
        }

        for fk in &table.foreign_keys {
            self.check_fk_values_exist(&table, fk, row_values, context)?;
        }

        Ok(())
    }

    /// Enforce foreign key constraints on UPDATE.
    ///
    /// For each FK constraint on the table being updated, verify that the
    /// new values in any modified FK columns still exist in the referenced table.
    pub(super) fn enforce_fk_on_update(
        &self,
        table_id: RelationId,
        new_values: &[Value],
        context: &ExecutionContext,
    ) -> DbResult<()> {
        // Same logic as INSERT: the full new row must satisfy FK constraints.
        self.enforce_fk_on_insert(table_id, new_values, context)
    }

    /// Compile the child-side FK metadata that `check_fk_values_exist`
    /// re-derives on every modified tuple: child column ordinals,
    /// referenced table descriptor, referenced column ordinals.
    /// PostgreSQL caches the same shape in `RI_QueryHashEntry`; we
    /// build the cache once at executor-start time and consume it via
    /// `enforce_fk_on_update_with_compiled_diff`.
    pub(super) fn compile_child_fk_checks(
        &self,
        table: &TableDescriptor,
        context: &ExecutionContext,
    ) -> DbResult<Vec<CompiledChildFkCheck>> {
        let mut out = Vec::with_capacity(table.foreign_keys.len());
        for fk in &table.foreign_keys {
            let fk_ordinals = resolve_column_ordinals(table, &fk.columns);
            if fk_ordinals.len() != fk.columns.len() {
                continue;
            }
            let ref_table_name = parse_qualified_name(&fk.referenced_table);
            let referenced_table = match self
                .catalog_reader
                .get_table(context.txn_id, &ref_table_name)?
            {
                Some(t) => t,
                None => continue,
            };
            let referenced_ordinals =
                resolve_column_ordinals(&referenced_table, &fk.referenced_columns);
            if referenced_ordinals.len() != fk.referenced_columns.len() {
                continue;
            }
            out.push(CompiledChildFkCheck {
                fk: fk.clone(),
                fk_ordinals,
                referenced_table,
                referenced_ordinals,
            });
        }
        Ok(out)
    }

    /// Per-row UPDATE FK enforcement using a pre-compiled child-FK
    /// list. Same semantics as `enforce_fk_on_update_with_diff`, but
    /// the catalog walk for each FK's referenced table and the
    /// `resolve_column_ordinals` linear searches have been hoisted
    /// out of the row loop by `compile_child_fk_checks`.
    pub(super) fn enforce_fk_on_update_with_compiled_diff(
        &self,
        compiled: &[CompiledChildFkCheck],
        old_values: &[Value],
        new_values: &[Value],
        context: &ExecutionContext,
    ) -> DbResult<()> {
        if compiled.is_empty() {
            return Ok(());
        }
        for entry in compiled {
            let any_changed = entry
                .fk_ordinals
                .iter()
                .any(|&ord| old_values.get(ord) != new_values.get(ord));
            if !any_changed {
                continue;
            }
            self.check_compiled_child_fk_values_exist(entry, new_values, context)?;
        }
        Ok(())
    }

    /// `check_fk_values_exist` minus the catalog/ordinal resolution that
    /// `compile_child_fk_checks` already performed.
    fn check_compiled_child_fk_values_exist(
        &self,
        entry: &CompiledChildFkCheck,
        row_values: &[Value],
        context: &ExecutionContext,
    ) -> DbResult<()> {
        let fk_values: Vec<&Value> = entry
            .fk_ordinals
            .iter()
            .map(|&ord| &row_values[ord])
            .collect();
        let any_null = fk_values.iter().any(|v| v.is_null());
        let all_null = fk_values.iter().all(|v| v.is_null());
        match entry.fk.match_type {
            aiondb_core::FkMatchType::Full => {
                if all_null {
                    return Ok(());
                }
                if any_null {
                    let constraint_name =
                        entry.fk.effective_name(&entry.referenced_table.name.name);
                    return Err(DbError::constraint_error(
                        SqlState::ForeignKeyViolation,
                        format!(
                            "insert or update on table \"{}\" violates foreign key constraint \"{constraint_name}\"",
                            entry.referenced_table.name.name
                        ),
                    )
                    .with_client_detail(
                        "MATCH FULL does not allow mixing of null and nonnull key values."
                            .to_owned(),
                    ));
                }
            }
            _ => {
                if any_null {
                    return Ok(());
                }
            }
        }
        if self.any_row_matches(
            entry.referenced_table.table_id,
            &entry.fk.referenced_columns,
            &entry.referenced_ordinals,
            &fk_values,
            context,
        )? {
            return Ok(());
        }
        Err(fk_not_present_error(
            &entry.referenced_table,
            &entry.fk,
            &fk_values,
        ))
    }

    /// Pre-check parent-side FK constraints when an UPDATE rewrites a row
    /// whose columns are referenced by another table's FK. For
    /// `RESTRICT`/`NO ACTION`, PostgreSQL rejects the parent-row rewrite when
    /// a child row still references the old key.
    pub(super) fn enforce_fk_referenced_on_parent_update_restrict(
        &self,
        table_id: RelationId,
        old_values: &[Value],
        new_values: &[Value],
        context: &ExecutionContext,
    ) -> DbResult<()> {
        let table = self
            .catalog_reader
            .get_table_by_id(context.txn_id, table_id)?
            .ok_or_else(|| DbError::internal("table not found for FK parent-update check"))?;

        let all_tables = self.list_all_tables(context)?;

        for child_table in &all_tables {
            for fk in &child_table.foreign_keys {
                if !fk_references_table(fk, &table.name) {
                    continue;
                }
                if !matches!(
                    fk.on_update,
                    aiondb_core::FkAction::Restrict | aiondb_core::FkAction::NoAction
                ) {
                    continue;
                }

                let parent_ordinals = resolve_column_ordinals(&table, &fk.referenced_columns);
                if parent_ordinals.len() != fk.referenced_columns.len() {
                    continue;
                }

                // Skip when the UPDATE leaves every referenced column
                // unchanged - the child reference remains valid.
                let any_changed = parent_ordinals
                    .iter()
                    .any(|&ord| old_values.get(ord) != new_values.get(ord));
                if !any_changed {
                    continue;
                }

                let parent_old_values: Vec<&Value> = parent_ordinals
                    .iter()
                    .map(|&ord| &old_values[ord])
                    .collect();

                if parent_old_values.iter().all(|v| v.is_null()) {
                    continue;
                }

                let child_ordinals = resolve_column_ordinals(child_table, &fk.columns);
                if child_ordinals.len() != fk.columns.len() {
                    continue;
                }

                if !self
                    .child_rows_referencing_values(
                        child_table.table_id,
                        &child_ordinals,
                        &parent_old_values,
                        context,
                    )?
                    .is_empty()
                {
                    let constraint_name = fk.effective_name(&child_table.name.name);
                    let detail_key = fk.referenced_columns.join(", ");
                    let detail_value = parent_old_values
                        .iter()
                        .map(|v| v.to_string())
                        .collect::<Vec<_>>()
                        .join(", ");
                    return Err(DbError::constraint_error(
                        SqlState::ForeignKeyViolation,
                        format!(
                            "update or delete on table \"{}\" violates foreign key constraint \"{}\" on table \"{}\"",
                            table.name.name, constraint_name, child_table.name.name
                        ),
                    )
                    .with_client_detail(format!(
                        "Key ({detail_key})=({detail_value}) is still referenced from table \"{}\".",
                        child_table.name.name
                    )));
                }
            }
        }

        Ok(())
    }

    /// Pre-resolve the per-statement metadata that
    /// `enforce_fk_referenced_on_parent_update_restrict` and
    /// `apply_fk_referenced_on_parent_update_actions` re-derived per row:
    /// the catalog walk listing every table and re-resolving column
    /// ordinals against both parent and child schemas. PostgreSQL holds
    /// the same information in `RelationGetFKeyList` / triggerdesc
    /// caches per executor invocation. The caller (UPDATE handler) runs
    /// this once per statement when `has_referencing_update_fks` is
    /// true, then uses the `_with_entries` variants in the per-row hot
    /// path so the catalog is no longer scanned on every tuple.
    pub(super) fn list_referencing_update_fks(
        &self,
        parent_table: &TableDescriptor,
        context: &ExecutionContext,
    ) -> DbResult<Vec<ReferencingUpdateFkEntry>> {
        let all_tables = self.list_all_tables(context)?;
        let mut entries = Vec::new();
        for child_table in all_tables {
            for fk in &child_table.foreign_keys {
                if !fk_references_table(fk, &parent_table.name) {
                    continue;
                }
                if !matches!(
                    fk.on_update,
                    aiondb_core::FkAction::Restrict
                        | aiondb_core::FkAction::NoAction
                        | aiondb_core::FkAction::Cascade
                        | aiondb_core::FkAction::SetNull
                        | aiondb_core::FkAction::SetDefault
                ) {
                    continue;
                }
                let parent_ordinals = resolve_column_ordinals(parent_table, &fk.referenced_columns);
                if parent_ordinals.len() != fk.referenced_columns.len() {
                    continue;
                }
                let child_ordinals = resolve_column_ordinals(&child_table, &fk.columns);
                if child_ordinals.len() != fk.columns.len() {
                    continue;
                }
                entries.push(ReferencingUpdateFkEntry {
                    fk: fk.clone(),
                    parent_ordinals,
                    child_ordinals,
                    child_table: child_table.clone(),
                });
            }
        }
        Ok(entries)
    }

    /// Cached-metadata variant of
    /// `enforce_fk_referenced_on_parent_update_restrict`. Identical
    /// per-row semantics; the only difference is that the catalog walk
    /// + column-name resolution have been hoisted by the caller via
    ///   `list_referencing_update_fks`.
    pub(super) fn enforce_fk_referenced_on_parent_update_restrict_with_entries(
        &self,
        entries: &[ReferencingUpdateFkEntry],
        parent_table: &TableDescriptor,
        old_values: &[Value],
        new_values: &[Value],
        context: &ExecutionContext,
    ) -> DbResult<()> {
        for entry in entries {
            if !matches!(
                entry.fk.on_update,
                aiondb_core::FkAction::Restrict | aiondb_core::FkAction::NoAction
            ) {
                continue;
            }

            let any_changed = entry
                .parent_ordinals
                .iter()
                .any(|&ord| old_values.get(ord) != new_values.get(ord));
            if !any_changed {
                continue;
            }

            let parent_old_values: Vec<&Value> = entry
                .parent_ordinals
                .iter()
                .map(|&ord| &old_values[ord])
                .collect();
            if parent_old_values.iter().all(|v| v.is_null()) {
                continue;
            }

            if !self
                .child_rows_referencing_values(
                    entry.child_table.table_id,
                    &entry.child_ordinals,
                    &parent_old_values,
                    context,
                )?
                .is_empty()
            {
                let constraint_name = entry.fk.effective_name(&entry.child_table.name.name);
                let detail_key = entry.fk.referenced_columns.join(", ");
                let detail_value = parent_old_values
                    .iter()
                    .map(|v| v.to_string())
                    .collect::<Vec<_>>()
                    .join(", ");
                return Err(DbError::constraint_error(
                    SqlState::ForeignKeyViolation,
                    format!(
                        "update or delete on table \"{}\" violates foreign key constraint \"{}\" on table \"{}\"",
                        parent_table.name.name, constraint_name, entry.child_table.name.name
                    ),
                )
                .with_client_detail(format!(
                    "Key ({detail_key})=({detail_value}) is still referenced from table \"{}\".",
                    entry.child_table.name.name
                )));
            }
        }
        Ok(())
    }

    /// Cached-metadata variant of
    /// `apply_fk_referenced_on_parent_update_actions`. Same per-row
    /// semantics as the original; the catalog walk + column-name
    /// resolution have been hoisted by the caller.
    pub(super) fn apply_fk_referenced_on_parent_update_actions_with_entries(
        &self,
        entries: &[ReferencingUpdateFkEntry],
        old_values: &[Value],
        new_values: &[Value],
        context: &ExecutionContext,
    ) -> DbResult<()> {
        for entry in entries {
            if !matches!(
                entry.fk.on_update,
                aiondb_core::FkAction::Cascade
                    | aiondb_core::FkAction::SetNull
                    | aiondb_core::FkAction::SetDefault
            ) {
                continue;
            }

            let any_changed = entry
                .parent_ordinals
                .iter()
                .any(|&ord| old_values.get(ord) != new_values.get(ord));
            if !any_changed {
                continue;
            }

            let parent_old_values: Vec<&Value> = entry
                .parent_ordinals
                .iter()
                .map(|&ord| &old_values[ord])
                .collect();
            if parent_old_values.iter().all(|v| v.is_null()) {
                continue;
            }
            let parent_new_values: Vec<&Value> = entry
                .parent_ordinals
                .iter()
                .map(|&ord| &new_values[ord])
                .collect();

            let action_ordinals = fk_action_target_ordinals(
                &entry.fk.columns,
                &entry.child_ordinals,
                &entry.fk.on_update_set_columns,
            );

            let matching_children = self.child_rows_referencing_values(
                entry.child_table.table_id,
                &entry.child_ordinals,
                &parent_old_values,
                context,
            )?;
            for child in matching_children {
                let mut rewritten = child.row.values.clone();
                for (idx, &child_ord) in entry.child_ordinals.iter().enumerate() {
                    if !action_ordinals.contains(&child_ord) {
                        continue;
                    }
                    rewritten[child_ord] = match entry.fk.on_update {
                        aiondb_core::FkAction::Cascade => (*parent_new_values[idx]).clone(),
                        aiondb_core::FkAction::SetNull => Value::Null,
                        aiondb_core::FkAction::SetDefault => self.fk_column_default_value(
                            &entry.child_table,
                            entry.fk.columns.get(idx).map(String::as_str).unwrap_or(""),
                        )?,
                        _ => rewritten[child_ord].clone(),
                    };
                }
                self.rewrite_child_row_for_fk_action(
                    &entry.child_table,
                    child.tuple_id,
                    &child.row.values,
                    rewritten,
                    context,
                )?;
            }
        }
        Ok(())
    }

    /// Apply post-update parent-side FK actions (`CASCADE`, `SET NULL`,
    /// `SET DEFAULT`) for child rows that reference the old parent key.
    pub(super) fn apply_fk_referenced_on_parent_update_actions(
        &self,
        table_id: RelationId,
        old_values: &[Value],
        new_values: &[Value],
        context: &ExecutionContext,
    ) -> DbResult<()> {
        let table = self
            .catalog_reader
            .get_table_by_id(context.txn_id, table_id)?
            .ok_or_else(|| DbError::internal("table not found for FK parent-update action"))?;

        let all_tables = self.list_all_tables(context)?;
        for child_table in &all_tables {
            for fk in &child_table.foreign_keys {
                if !fk_references_table(fk, &table.name) {
                    continue;
                }
                if !matches!(
                    fk.on_update,
                    aiondb_core::FkAction::Cascade
                        | aiondb_core::FkAction::SetNull
                        | aiondb_core::FkAction::SetDefault
                ) {
                    continue;
                }

                let parent_ordinals = resolve_column_ordinals(&table, &fk.referenced_columns);
                if parent_ordinals.len() != fk.referenced_columns.len() {
                    continue;
                }
                let any_changed = parent_ordinals
                    .iter()
                    .any(|&ord| old_values.get(ord) != new_values.get(ord));
                if !any_changed {
                    continue;
                }

                let parent_old_values: Vec<&Value> = parent_ordinals
                    .iter()
                    .map(|&ord| &old_values[ord])
                    .collect();
                if parent_old_values.iter().all(|v| v.is_null()) {
                    continue;
                }
                let parent_new_values: Vec<&Value> = parent_ordinals
                    .iter()
                    .map(|&ord| &new_values[ord])
                    .collect();

                let child_ordinals = resolve_column_ordinals(child_table, &fk.columns);
                if child_ordinals.len() != fk.columns.len() {
                    continue;
                }
                let action_ordinals = fk_action_target_ordinals(
                    &fk.columns,
                    &child_ordinals,
                    &fk.on_update_set_columns,
                );

                let matching_children = self.child_rows_referencing_values(
                    child_table.table_id,
                    &child_ordinals,
                    &parent_old_values,
                    context,
                )?;
                for child in matching_children {
                    let mut rewritten = child.row.values.clone();
                    for (idx, &child_ord) in child_ordinals.iter().enumerate() {
                        if !action_ordinals.contains(&child_ord) {
                            continue;
                        }
                        rewritten[child_ord] = match fk.on_update {
                            aiondb_core::FkAction::Cascade => (*parent_new_values[idx]).clone(),
                            aiondb_core::FkAction::SetNull => Value::Null,
                            aiondb_core::FkAction::SetDefault => self.fk_column_default_value(
                                child_table,
                                fk.columns.get(idx).map(String::as_str).unwrap_or(""),
                            )?,
                            _ => rewritten[child_ord].clone(),
                        };
                    }
                    self.rewrite_child_row_for_fk_action(
                        child_table,
                        child.tuple_id,
                        &child.row.values,
                        rewritten,
                        context,
                    )?;
                }
            }
        }
        Ok(())
    }

    /// Enforce foreign key constraints on DELETE, including parent-side
    /// referential actions.
    pub(super) fn enforce_fk_on_delete(
        &self,
        table_id: RelationId,
        deleted_row: &[Value],
        context: &ExecutionContext,
    ) -> DbResult<()> {
        // Cap FK CASCADE recursion. A schema with cyclic ON DELETE
        // CASCADE references would otherwise re-enter this function via
        // delete_locked → enforce_fk_on_delete forever, eventually
        // SIGSEGVing. PG's effective working set is ~32 levels.
        const MAX_FK_CASCADE_DEPTH: u32 = 32;
        let depth = context
            .fk_cascade_depth
            .fetch_add(1, std::sync::atomic::Ordering::AcqRel);
        struct FkDepthGuard<'a>(&'a std::sync::atomic::AtomicU32);
        impl<'a> Drop for FkDepthGuard<'a> {
            fn drop(&mut self) {
                self.0.fetch_sub(1, std::sync::atomic::Ordering::AcqRel);
            }
        }
        let _guard = FkDepthGuard(&context.fk_cascade_depth);
        if depth >= MAX_FK_CASCADE_DEPTH {
            return Err(DbError::program_limit(format!(
                "FK cascade depth {} exceeds limit {MAX_FK_CASCADE_DEPTH}",
                depth + 1
            )));
        }
        if !self.table_has_referencing_delete_foreign_keys(table_id, context)? {
            return Ok(());
        }

        let table = self
            .catalog_reader
            .get_table_by_id(context.txn_id, table_id)?
            .ok_or_else(|| DbError::internal("table not found for FK check"))?;

        let all_tables = self.list_all_tables(context)?;
        for child_table in &all_tables {
            for fk in &child_table.foreign_keys {
                // Check if this FK references our table
                if !fk_references_table(fk, &table.name) {
                    continue;
                }

                // Find ordinals of referenced columns in our (parent) table
                let parent_ordinals = resolve_column_ordinals(&table, &fk.referenced_columns);
                if parent_ordinals.len() != fk.referenced_columns.len() {
                    continue; // columns not found, skip
                }

                // Extract the values from the deleted row at those ordinal positions
                let parent_values: Vec<&Value> = parent_ordinals
                    .iter()
                    .map(|&ord| &deleted_row[ord])
                    .collect();

                // Skip if all FK column values in the deleted row are NULL
                if parent_values.iter().all(|v| v.is_null()) {
                    continue;
                }

                // Find ordinals of FK columns in the child table
                let child_ordinals = resolve_column_ordinals(child_table, &fk.columns);
                if child_ordinals.len() != fk.columns.len() {
                    continue;
                }
                let action_ordinals = fk_action_target_ordinals(
                    &fk.columns,
                    &child_ordinals,
                    &fk.on_delete_set_columns,
                );

                let matching_children = self.child_rows_referencing_values(
                    child_table.table_id,
                    &child_ordinals,
                    &parent_values,
                    context,
                )?;
                if matching_children.is_empty() {
                    continue;
                }
                match fk.on_delete {
                    aiondb_core::FkAction::Restrict | aiondb_core::FkAction::NoAction => {
                        let constraint_name = fk.effective_name(&child_table.name.name);
                        let detail_key = fk.referenced_columns.join(", ");
                        let detail_value = parent_values
                            .iter()
                            .map(|value| match value {
                                Value::Null => "null".to_owned(),
                                other => other.to_string(),
                            })
                            .collect::<Vec<_>>()
                            .join(", ");
                        return Err(DbError::constraint_error(
                            SqlState::ForeignKeyViolation,
                            format!(
                                "update or delete on table \"{}\" violates foreign key constraint \"{constraint_name}\" on table \"{}\"",
                                table.name.name, child_table.name.name
                            ),
                        )
                        .with_client_detail(format!(
                            "Key ({detail_key})=({detail_value}) is still referenced from table \"{}\".",
                            child_table.name.name
                        )));
                    }
                    aiondb_core::FkAction::Cascade => {
                        for child in matching_children {
                            if !self.fire_before_delete_triggers(
                                child_table.table_id,
                                &child.row.values,
                                context,
                            )? {
                                continue;
                            }
                            self.delete_locked(
                                context,
                                child_table.table_id,
                                child.tuple_id,
                                Some(&child.row),
                            )?;
                            self.fire_after_delete_triggers(
                                child_table.table_id,
                                &child.row.values,
                                context,
                            )?;
                        }
                    }
                    aiondb_core::FkAction::SetNull | aiondb_core::FkAction::SetDefault => {
                        for child in matching_children {
                            let mut rewritten = child.row.values.clone();
                            for (idx, &child_ord) in child_ordinals.iter().enumerate() {
                                if !action_ordinals.contains(&child_ord) {
                                    continue;
                                }
                                rewritten[child_ord] = match fk.on_delete {
                                    aiondb_core::FkAction::SetNull => Value::Null,
                                    aiondb_core::FkAction::SetDefault => self
                                        .fk_column_default_value(
                                            child_table,
                                            fk.columns.get(idx).map(String::as_str).unwrap_or(""),
                                        )?,
                                    _ => rewritten[child_ord].clone(),
                                };
                            }
                            self.rewrite_child_row_for_fk_action(
                                child_table,
                                child.tuple_id,
                                &child.row.values,
                                rewritten,
                                context,
                            )?;
                        }
                    }
                }
            }
        }

        Ok(())
    }

    /// Find an index whose leading columns cover all the requested column names.
    ///
    /// Returns the best matching index (preferring unique indexes) or `None`
    /// if no suitable index exists.
    fn find_covering_index(
        &self,
        txn_id: TxnId,
        table_id: RelationId,
        column_names: &[String],
        table: &TableDescriptor,
    ) -> DbResult<Option<IndexId>> {
        let indexes = self.catalog_reader.list_indexes(txn_id, table_id)?;
        Ok(select_covering_index(&indexes, column_names, table))
    }

    /// Check that the FK column values in a row exist in the referenced table.
    fn check_fk_values_exist(
        &self,
        table: &TableDescriptor,
        fk: &ForeignKeyConstraint,
        row_values: &[Value],
        context: &ExecutionContext,
    ) -> DbResult<()> {
        // Resolve FK column ordinals in the source (child) table
        let fk_ordinals = resolve_column_ordinals(table, &fk.columns);
        if fk_ordinals.len() != fk.columns.len() {
            return Err(DbError::internal(
                "FK column ordinals could not be resolved",
            ));
        }

        // Extract FK values from the row
        let fk_values: Vec<&Value> = fk_ordinals.iter().map(|&ord| &row_values[ord]).collect();

        // MATCH semantics: SIMPLE/PARTIAL pass when any NULL is present;
        // FULL requires either every key column NULL or none and rejects
        // mixed.  PARTIAL falls back to SIMPLE here since the executor never
        // implemented true partial matching.
        let any_null = fk_values.iter().any(|v| v.is_null());
        let all_null = fk_values.iter().all(|v| v.is_null());
        match fk.match_type {
            aiondb_core::FkMatchType::Full => {
                if all_null {
                    return Ok(());
                }
                if any_null {
                    let constraint_name = fk.effective_name(&table.name.name);
                    return Err(DbError::constraint_error(
                        SqlState::ForeignKeyViolation,
                        format!(
                            "insert or update on table \"{}\" violates foreign key constraint \"{constraint_name}\"",
                            table.name.name
                        ),
                    )
                    .with_client_detail(
                        "MATCH FULL does not allow mixing of null and nonnull key values."
                            .to_owned(),
                    ));
                }
            }
            _ => {
                if any_null {
                    return Ok(());
                }
            }
        }

        // Resolve referenced table
        let ref_table_name = parse_qualified_name(&fk.referenced_table);
        let ref_table = self
            .catalog_reader
            .get_table(context.txn_id, &ref_table_name)?
            .ok_or_else(|| {
                DbError::constraint_error(
                    SqlState::ForeignKeyViolation,
                    format!(
                        "referenced table \"{}\" does not exist",
                        fk.referenced_table
                    ),
                )
            })?;

        // Resolve referenced column ordinals
        let ref_ordinals = resolve_column_ordinals(&ref_table, &fk.referenced_columns);
        if ref_ordinals.len() != fk.referenced_columns.len() {
            return Err(DbError::internal(
                "referenced column ordinals could not be resolved",
            ));
        }

        // Check that at least one matching row exists.
        if self.any_row_matches(
            ref_table.table_id,
            &fk.referenced_columns,
            &ref_ordinals,
            &fk_values,
            context,
        )? {
            return Ok(());
        }
        Err(fk_not_present_error(table, fk, &fk_values))
    }

    fn child_rows_referencing_values(
        &self,
        child_table_id: RelationId,
        child_ordinals: &[usize],
        parent_values: &[&Value],
        context: &ExecutionContext,
    ) -> DbResult<Vec<TupleRecord>> {
        let mut rows = Vec::new();
        let mut stream = self.scan_table_locked(context, child_table_id, None)?;
        let has_interrupts = context.has_execution_interrupts();
        let mut row_counter: u32 = 0;
        while let Some(record) = stream.next()? {
            if has_interrupts {
                row_counter = row_counter.wrapping_add(1);
                if row_counter.trailing_zeros() >= 10 {
                    context.check_deadline()?;
                }
            }
            if child_ordinals
                .iter()
                .zip(parent_values.iter())
                .all(|(&ord, val)| values_equal(&record.row.values[ord], val))
            {
                rows.push(record);
            }
        }
        Ok(rows)
    }

    fn rewrite_child_row_for_fk_action(
        &self,
        child_table: &TableDescriptor,
        tuple_id: aiondb_core::TupleId,
        old_values: &[Value],
        mut new_values: Vec<Value>,
        context: &ExecutionContext,
    ) -> DbResult<()> {
        if !self.fire_before_update_triggers(
            child_table.table_id,
            &mut new_values,
            old_values,
            context,
        )? {
            return Ok(());
        }
        self.enforce_fk_on_update(child_table.table_id, &new_values, context)?;
        self.enforce_check_on_update(child_table.table_id, &new_values, context)?;
        self.enforce_unique_on_update(child_table.table_id, &new_values, tuple_id, context)?;
        self.enforce_fk_referenced_on_parent_update_restrict(
            child_table.table_id,
            old_values,
            &new_values,
            context,
        )?;
        let new_row = Row::new(new_values);
        self.update_locked(
            context,
            child_table.table_id,
            tuple_id,
            Some(&Row::new(old_values.to_vec())),
            new_row.clone(),
        )?;
        self.fire_after_update_triggers(
            child_table.table_id,
            &new_row.values,
            old_values,
            context,
        )?;
        self.apply_fk_referenced_on_parent_update_actions(
            child_table.table_id,
            old_values,
            &new_row.values,
            context,
        )?;
        Ok(())
    }

    fn fk_column_default_value(
        &self,
        child_table: &TableDescriptor,
        child_column_name: &str,
    ) -> DbResult<Value> {
        let column = child_table
            .column_by_name(child_column_name)
            .ok_or_else(|| DbError::internal("FK child column not found for SET DEFAULT action"))?;
        let raw = match column.default_value.as_deref() {
            Some(expression_sql) => {
                let parsed = aiondb_parser::parse_expression(expression_sql).map_err(|error| {
                    DbError::bind_error(
                        SqlState::SyntaxError,
                        format!("invalid default expression: {error}"),
                    )
                })?;
                match parsed {
                    aiondb_parser::Expr::Literal(literal, _) => match literal {
                        aiondb_parser::Literal::Integer(number) => {
                            if let Ok(number) = i32::try_from(number) {
                                Value::Int(number)
                            } else {
                                Value::BigInt(number)
                            }
                        }
                        aiondb_parser::Literal::NumericLit(number) => number
                            .parse::<f64>()
                            .map(Value::Double)
                            .map_err(|_| DbError::invalid_input_syntax("numeric", &number))?,
                        aiondb_parser::Literal::String(text) => Value::Text(text),
                        aiondb_parser::Literal::Boolean(flag) => Value::Boolean(flag),
                        aiondb_parser::Literal::Null => Value::Null,
                    },
                    _ => Value::Null,
                }
            }
            None => Value::Null,
        };
        coerce_assigned_value(
            raw,
            &column.data_type,
            column.nullable,
            column.text_type_modifier,
        )
    }

    fn list_all_tables(&self, context: &ExecutionContext) -> DbResult<Vec<TableDescriptor>> {
        let mut tables = Vec::new();
        for schema in self.catalog_reader.list_schemas(context.txn_id)? {
            tables.extend(
                self.catalog_reader
                    .list_tables(context.txn_id, schema.schema_id)?,
            );
        }
        Ok(tables)
    }

    /// Check whether any row in `table_id` matches the given values at the
    /// specified ordinal positions.  Uses an index when available, falls back
    /// to a sequential scan otherwise.
    fn any_row_matches(
        &self,
        table_id: RelationId,
        column_names: &[String],
        ordinals: &[usize],
        values: &[&Value],
        context: &ExecutionContext,
    ) -> DbResult<bool> {
        let table = self
            .catalog_reader
            .get_table_by_id(context.txn_id, table_id)?
            .ok_or_else(|| DbError::internal("table not found for FK match"))?;
        let projected_column_ids: Option<Vec<ColumnId>> = {
            let ids: Vec<ColumnId> = ordinals
                .iter()
                .filter_map(|&ord| table.columns.get(ord).map(|column| column.column_id))
                .collect();
            (ids.len() == ordinals.len()).then_some(ids)
        };
        let compare_positions: Vec<usize> = (0..values.len()).collect();

        if let Some(index_id) =
            self.find_covering_index(context.txn_id, table_id, column_names, &table)?
        {
            let key_range = exact_lookup_key_range(values[0]);
            let mut stream = self.scan_index_locked(
                context,
                table_id,
                index_id,
                key_range,
                projected_column_ids.clone(),
            )?;
            while let Some(record) = stream.next()? {
                context.check_deadline()?;
                if compare_positions
                    .iter()
                    .zip(values.iter())
                    .all(|(&ord, val)| values_equal(&record.row.values[ord], val))
                {
                    return Ok(true);
                }
            }
            // Fall through to a sequential scan when the index seek returned
            // no records: the index key encoder is type-sensitive (e.g. an
            // `int4` index can't be probed with an `int8` key), so a strict
            // FK lookup that crosses the integer-family boundary needs a
            // value-level match. `values_equal` already handles cross-type
            // numeric equality.
        }

        let mut stream = self.scan_table_locked(context, table_id, projected_column_ids)?;
        while let Some(record) = stream.next()? {
            context.check_deadline()?;
            if compare_positions
                .iter()
                .zip(values.iter())
                .all(|(&ord, val)| values_equal(&record.row.values[ord], val))
            {
                return Ok(true);
            }
        }
        Ok(false)
    }
}

/// Resolve column names to zero-based ordinal positions within a table descriptor.
fn resolve_column_ordinals(table: &TableDescriptor, column_names: &[String]) -> Vec<usize> {
    let col_map: std::collections::HashMap<String, usize> = table
        .columns
        .iter()
        .enumerate()
        .map(|(i, col)| (col.name.to_ascii_lowercase(), i))
        .collect();
    column_names
        .iter()
        .filter_map(|name| col_map.get(&name.to_ascii_lowercase()).copied())
        .collect()
}

fn fk_action_target_ordinals(
    fk_columns: &[String],
    child_ordinals: &[usize],
    action_columns: &[String],
) -> Vec<usize> {
    if action_columns.is_empty() {
        return child_ordinals.to_vec();
    }
    fk_columns
        .iter()
        .zip(child_ordinals.iter())
        .filter_map(|(fk_column, ordinal)| {
            action_columns
                .iter()
                .any(|action_col| action_col.eq_ignore_ascii_case(fk_column))
                .then_some(*ordinal)
        })
        .collect()
}

/// Check if a FK constraint references the given table (by name).
fn fk_references_table(
    fk: &ForeignKeyConstraint,
    table_name: &aiondb_catalog::QualifiedName,
) -> bool {
    let ref_name = parse_qualified_name(&fk.referenced_table);
    ref_name.name.eq_ignore_ascii_case(&table_name.name)
}

/// Compare two Values for equality, treating NULLs as not equal (standard SQL).
pub(super) fn values_equal(a: &Value, b: &Value) -> bool {
    if a.is_null() || b.is_null() {
        return false;
    }
    if a == b {
        return true;
    }
    // Cross-type integer-family equality (PG's b-tree integer opfamily): a
    // child column declared `int8` referencing a parent `int4` column should
    // match `42::int4 == 42::int8`. Same for numeric promotions.
    match (a, b) {
        (Value::Int(x), Value::BigInt(y)) | (Value::BigInt(y), Value::Int(x)) => {
            i64::from(*x) == *y
        }
        (Value::Int(x), Value::Numeric(n)) | (Value::Numeric(n), Value::Int(x)) => {
            n.scale == 0 && n.coefficient == i128::from(*x)
        }
        (Value::BigInt(x), Value::Numeric(n)) | (Value::Numeric(n), Value::BigInt(x)) => {
            n.scale == 0 && n.coefficient == i128::from(*x)
        }
        _ => false,
    }
}

/// Select the best covering index from a list of candidates.
///
/// An index covers a lookup if its leading key columns (prefix) match the
/// requested column IDs in order. When multiple indexes qualify, a unique
/// index is preferred.
fn select_covering_index(
    indexes: &[IndexDescriptor],
    column_names: &[String],
    table: &TableDescriptor,
) -> Option<IndexId> {
    // Resolve column names to ColumnIds via the table descriptor.
    let col_map: std::collections::HashMap<String, ColumnId> = table
        .columns
        .iter()
        .map(|c| (c.name.to_ascii_lowercase(), c.column_id))
        .collect();
    let target_col_ids: Vec<ColumnId> = column_names
        .iter()
        .filter_map(|name| col_map.get(&name.to_ascii_lowercase()).copied())
        .collect();

    // If we couldn't resolve all column names, no index can cover them.
    if target_col_ids.len() != column_names.len() {
        return None;
    }

    let mut best: Option<(IndexId, bool)> = None;

    for idx in indexes {
        // The index must have at least as many key columns as the FK lookup needs.
        if idx.key_columns.len() < target_col_ids.len() {
            continue;
        }

        // Check that the first N index key columns match the target column IDs
        // (prefix match, preserving order).
        let covers = target_col_ids
            .iter()
            .enumerate()
            .all(|(i, &col_id)| idx.key_columns[i].column_id == col_id);

        if !covers {
            continue;
        }

        // Prefer unique indexes for correctness-optimal fast path.
        match best {
            None => best = Some((idx.index_id, idx.unique)),
            Some((_, prev_unique)) if !prev_unique && idx.unique => {
                best = Some((idx.index_id, idx.unique));
            }
            _ => {}
        }
    }

    best.map(|(id, _)| id)
}

/// Build the FK violation error for INSERT/UPDATE when the key is not present.
fn fk_not_present_error(
    table: &TableDescriptor,
    fk: &ForeignKeyConstraint,
    fk_values: &[&Value],
) -> DbError {
    let table_name = &table.name.name;
    let referenced_name = parse_qualified_name(&fk.referenced_table).name;
    let constraint_name = fk.effective_name(table_name);
    let rendered_values = fk_values
        .iter()
        .map(|value| match value {
            Value::Null => "null".to_owned(),
            other => other.to_string(),
        })
        .collect::<Vec<_>>()
        .join(", ");
    DbError::constraint_error(
        SqlState::ForeignKeyViolation,
        format!(
            "insert or update on table \"{table_name}\" violates foreign key constraint \"{constraint_name}\""
        ),
    )
    .with_client_detail(format!(
        "Key ({})=({rendered_values}) is not present in table \"{referenced_name}\".",
        fk.columns.join(", ")
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    // -----------------------------------------------------------------
    // Test helpers
    // -----------------------------------------------------------------

    fn make_table_desc(columns: &[(&str, ColumnId)]) -> TableDescriptor {
        TableDescriptor {
            table_id: RelationId::new(1),
            schema_id: aiondb_core::SchemaId::new(1),
            name: QualifiedName {
                schema: Some("public".to_string()),
                name: "test_table".to_string(),
            },
            columns: columns
                .iter()
                .enumerate()
                .map(|(i, (name, cid))| aiondb_catalog::ColumnDescriptor {
                    column_id: *cid,
                    name: name.to_string(),
                    data_type: DataType::Int,
                    raw_type_name: None,
                    text_type_modifier: None,
                    nullable: true,
                    ordinal_position: i as u32 + 1,
                    default_value: None,
                })
                .collect(),
            identity_columns: Vec::new(),
            primary_key: None,
            check_constraints: Vec::new(),
            shard_config: None,
            foreign_keys: Vec::new(),
            owner: None,
        }
    }

    fn make_index_key_col(col_id: ColumnId) -> IndexKeyColumn {
        IndexKeyColumn {
            column_id: col_id,
            sort_order: SortOrder::Ascending,
            nulls_first: false,
        }
    }

    fn make_index(
        index_id: IndexId,
        table_id: RelationId,
        key_columns: Vec<IndexKeyColumn>,
        unique: bool,
    ) -> IndexDescriptor {
        IndexDescriptor {
            index_id,
            schema_id: aiondb_core::SchemaId::new(1),
            table_id,
            name: QualifiedName {
                schema: Some("public".to_string()),
                name: format!("idx_{}", index_id.get()),
            },
            unique,
            nulls_not_distinct: false,
            kind: IndexKind::BTree,
            key_columns,
            include_columns: Vec::new(),
            constraint_name: None,
            hnsw_params: None,
        }
    }

    // -----------------------------------------------------------------
    // resolve_column_ordinals tests
    // -----------------------------------------------------------------

    #[test]
    fn resolve_column_ordinals_basic() {
        let table = make_table_desc(&[
            ("id", ColumnId::new(1)),
            ("name", ColumnId::new(2)),
            ("age", ColumnId::new(3)),
        ]);
        let names = vec!["name".to_string(), "age".to_string()];
        assert_eq!(resolve_column_ordinals(&table, &names), vec![1, 2]);
    }

    #[test]
    fn resolve_column_ordinals_case_insensitive() {
        let table = make_table_desc(&[("Id", ColumnId::new(1)), ("Name", ColumnId::new(2))]);
        let names = vec!["id".to_string(), "NAME".to_string()];
        assert_eq!(resolve_column_ordinals(&table, &names), vec![0, 1]);
    }

    #[test]
    fn resolve_column_ordinals_missing_column() {
        let table = make_table_desc(&[("id", ColumnId::new(1))]);
        let names = vec!["id".to_string(), "missing".to_string()];
        assert_eq!(resolve_column_ordinals(&table, &names), vec![0]);
    }

    // -----------------------------------------------------------------
    // values_equal tests
    // -----------------------------------------------------------------

    #[test]
    fn values_equal_basic() {
        assert!(values_equal(&Value::Int(42), &Value::Int(42)));
        assert!(!values_equal(&Value::Int(1), &Value::Int(2)));
    }

    #[test]
    fn values_equal_null_handling() {
        assert!(!values_equal(&Value::Null, &Value::Int(1)));
        assert!(!values_equal(&Value::Int(1), &Value::Null));
        assert!(!values_equal(&Value::Null, &Value::Null));
    }

    // -----------------------------------------------------------------
    // fk_references_table tests
    // -----------------------------------------------------------------

    #[test]
    fn fk_references_table_matches_case_insensitive() {
        let fk = ForeignKeyConstraint {
            columns: vec!["user_id".to_string()],
            referenced_table: "Users".to_string(),
            referenced_columns: vec!["id".to_string()],
            on_delete: aiondb_core::FkAction::NoAction,
            on_update: aiondb_core::FkAction::NoAction,
            on_delete_set_columns: Vec::new(),
            on_update_set_columns: Vec::new(),
            match_type: aiondb_core::FkMatchType::Simple,
            name: None,
        };
        let table_name = QualifiedName {
            schema: Some("public".to_string()),
            name: "users".to_string(),
        };
        assert!(fk_references_table(&fk, &table_name));
    }

    #[test]
    fn fk_references_table_does_not_match_different_table() {
        let fk = ForeignKeyConstraint {
            columns: vec!["user_id".to_string()],
            referenced_table: "orders".to_string(),
            referenced_columns: vec!["id".to_string()],
            on_delete: aiondb_core::FkAction::NoAction,
            on_update: aiondb_core::FkAction::NoAction,
            on_delete_set_columns: Vec::new(),
            on_update_set_columns: Vec::new(),
            match_type: aiondb_core::FkMatchType::Simple,
            name: None,
        };
        let table_name = QualifiedName {
            schema: Some("public".to_string()),
            name: "users".to_string(),
        };
        assert!(!fk_references_table(&fk, &table_name));
    }

    // -----------------------------------------------------------------
    // select_covering_index tests
    // -----------------------------------------------------------------

    #[test]
    fn select_covering_index_returns_none_for_empty_indexes() {
        let table = make_table_desc(&[("id", ColumnId::new(1))]);
        let result = select_covering_index(&[], &["id".to_string()], &table);
        assert!(result.is_none());
    }

    #[test]
    fn select_covering_index_returns_none_for_unresolvable_columns() {
        let table = make_table_desc(&[("id", ColumnId::new(1))]);
        let idx = make_index(
            IndexId::new(1),
            RelationId::new(1),
            vec![make_index_key_col(ColumnId::new(1))],
            true,
        );
        let result = select_covering_index(&[idx], &["nonexistent".to_string()], &table);
        assert!(result.is_none());
    }

    #[test]
    fn select_covering_index_single_column_match() {
        let table = make_table_desc(&[("id", ColumnId::new(1)), ("name", ColumnId::new(2))]);
        let idx = make_index(
            IndexId::new(10),
            RelationId::new(1),
            vec![make_index_key_col(ColumnId::new(1))],
            true,
        );
        let result = select_covering_index(&[idx], &["id".to_string()], &table);
        assert_eq!(result, Some(IndexId::new(10)));
    }

    #[test]
    fn select_covering_index_prefers_unique() {
        let col_id = ColumnId::new(1);
        let table = make_table_desc(&[("id", col_id)]);
        let nonunique = make_index(
            IndexId::new(1),
            RelationId::new(1),
            vec![make_index_key_col(col_id)],
            false,
        );
        let unique = make_index(
            IndexId::new(2),
            RelationId::new(1),
            vec![make_index_key_col(col_id)],
            true,
        );
        let result = select_covering_index(&[nonunique, unique], &["id".to_string()], &table);
        assert_eq!(result, Some(IndexId::new(2)));
    }

    #[test]
    fn select_covering_index_rejects_wrong_leading_column() {
        let table = make_table_desc(&[("a", ColumnId::new(1)), ("b", ColumnId::new(2))]);
        // Index on column "b" - should NOT cover lookup on "a".
        let idx = make_index(
            IndexId::new(1),
            RelationId::new(1),
            vec![make_index_key_col(ColumnId::new(2))],
            true,
        );
        let result = select_covering_index(&[idx], &["a".to_string()], &table);
        assert!(result.is_none());
    }

    #[test]
    fn select_covering_index_composite_prefix_match() {
        let table = make_table_desc(&[
            ("a", ColumnId::new(1)),
            ("b", ColumnId::new(2)),
            ("c", ColumnId::new(3)),
        ]);
        // Index on (a, b, c) should cover a lookup on (a, b).
        let idx = make_index(
            IndexId::new(5),
            RelationId::new(1),
            vec![
                make_index_key_col(ColumnId::new(1)),
                make_index_key_col(ColumnId::new(2)),
                make_index_key_col(ColumnId::new(3)),
            ],
            true,
        );
        let result = select_covering_index(&[idx], &["a".to_string(), "b".to_string()], &table);
        assert_eq!(result, Some(IndexId::new(5)));
    }

    #[test]
    fn select_covering_index_rejects_too_few_index_columns() {
        let table = make_table_desc(&[("a", ColumnId::new(1)), ("b", ColumnId::new(2))]);
        // Index on (a) only - cannot cover a lookup on (a, b).
        let idx = make_index(
            IndexId::new(1),
            RelationId::new(1),
            vec![make_index_key_col(ColumnId::new(1))],
            true,
        );
        let result = select_covering_index(&[idx], &["a".to_string(), "b".to_string()], &table);
        assert!(result.is_none());
    }
}
