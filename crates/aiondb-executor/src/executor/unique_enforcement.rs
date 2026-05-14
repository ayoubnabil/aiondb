use std::collections::HashSet;

use aiondb_core::TupleId;
use aiondb_storage_api::KeyRange;

use super::*;
use aiondb_plan::UpdateAssignment as PlanUpdateAssignment;

/// Pre-resolved unique-index metadata held once per UPDATE statement.
/// `key_ordinals` mirrors `descriptor.key_columns` projected into the
/// table's row layout; resolving it once eliminates the per-row
/// `build_column_id_map` HashMap rebuild that the legacy
/// `resolve_key_column_ordinals(...)` call performed on every modified
/// tuple, scaled by the number of unique indexes on the relation.
/// `is_expression_index` is similarly cached so the per-row HOT-skip
/// check avoids the `expression_index_meta` Mutex round-trip on each
/// call, mirroring PostgreSQL's `IsExpressionalAttribute` lookup that
/// happens once at executor-start time.
#[derive(Clone)]
pub(crate) struct UniqueIndexForUpdate {
    pub descriptor: IndexDescriptor,
    pub key_ordinals: Vec<usize>,
    pub is_expression_index: bool,
}

pub(super) struct UniqueInsertState {
    table: TableDescriptor,
    indexes: Vec<PreparedUniqueIndex>,
    table_was_empty: bool,
    seen_keys: Vec<HashSet<Vec<ValueHashKey>>>,
}

impl UniqueInsertState {
    pub(super) fn table_was_empty(&self) -> bool {
        self.table_was_empty
    }
}

struct PreparedUniqueIndex {
    descriptor: IndexDescriptor,
    ordinals: Vec<usize>,
    column_names: Vec<String>,
}

impl Executor {
    pub(super) fn update_may_affect_unique_indexes(
        &self,
        table: &TableDescriptor,
        updated_ordinals: &std::collections::HashSet<usize>,
        context: &ExecutionContext,
    ) -> DbResult<bool> {
        if updated_ordinals.is_empty() {
            return Ok(false);
        }
        for index in self
            .catalog_reader
            .list_indexes(context.txn_id, table.table_id)?
            .into_iter()
            .filter(|idx| idx.unique)
        {
            if self.expression_index_meta(index.index_id).is_some() {
                return Ok(true);
            }
            let ordinals = resolve_key_column_ordinals(table, &index.key_columns);
            if ordinals
                .iter()
                .any(|ordinal| updated_ordinals.contains(ordinal))
            {
                return Ok(true);
            }
        }
        Ok(false)
    }

    /// Mirror of `update_may_affect_unique_indexes` over **every** index
    /// (unique or not). When this returns `false`, the storage layer
    /// can safely skip its own per-row diff walk
    /// (`indexed_column_update_plan::changed_row_ordinals` + index
    /// iteration) because no index entry can possibly need rewriting.
    /// Plumbed through the storage `update_with_no_index_rewrite_hint`
    /// path so the per-row hot loop pays neither the diff walk nor the
    /// index-list catalog read for OLTP UPDATEs that touch only
    /// non-indexed columns (the typical "bump a counter" shape).
    #[allow(dead_code)] // pre-wired for upcoming UPDATE pruning path
    pub(super) fn update_may_affect_any_indexes(
        &self,
        table: &TableDescriptor,
        updated_ordinals: &std::collections::HashSet<usize>,
        context: &ExecutionContext,
    ) -> DbResult<bool> {
        if updated_ordinals.is_empty() {
            return Ok(false);
        }
        for index in self
            .catalog_reader
            .list_indexes(context.txn_id, table.table_id)?
        {
            if self.expression_index_meta(index.index_id).is_some() {
                return Ok(true);
            }
            let key_ordinals = resolve_key_column_ordinals(table, &index.key_columns);
            if key_ordinals
                .iter()
                .any(|ordinal| updated_ordinals.contains(ordinal))
            {
                return Ok(true);
            }
            // INCLUDE columns also drag the index into the rewrite if
            // they move; mirror the storage diff logic exactly.
            for include_id in &index.include_columns {
                if let Some(ord) = table
                    .columns
                    .iter()
                    .position(|c| c.column_id == *include_id)
                {
                    if updated_ordinals.contains(&ord) {
                        return Ok(true);
                    }
                }
            }
        }
        Ok(false)
    }

    fn statement_already_touched_conflict_key(
        &self,
        table_id: RelationId,
        conflict_ordinals: &[usize],
        proposed_values: &[Value],
        context: &ExecutionContext,
    ) -> DbResult<bool> {
        let written_tuple_ids = context
            .statement_tuple_writes
            .lock()
            .map(|writes| {
                writes
                    .iter()
                    .filter_map(|(relation_id, tuple_id)| {
                        (*relation_id == table_id).then_some(*tuple_id)
                    })
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();

        if written_tuple_ids.is_empty() {
            return Ok(false);
        }

        let latest_snapshot =
            aiondb_tx::Snapshot::new(TxnId::default(), TxnId::default(), Vec::new());
        for tuple_id in written_tuple_ids {
            let Some(row) = self.storage_dml.fetch(
                context.txn_id,
                &latest_snapshot,
                table_id,
                tuple_id,
                None,
            )?
            else {
                continue;
            };

            let key_matches = conflict_ordinals.iter().all(|&ord| {
                let existing = row.values.get(ord).unwrap_or(&Value::Null);
                let proposed = proposed_values.get(ord).unwrap_or(&Value::Null);
                !existing.is_null() && !proposed.is_null() && existing == proposed
            });
            if key_matches {
                return Ok(true);
            }
        }

        Ok(false)
    }

    fn evaluate_on_conflict_expr_with_rows(
        &self,
        expr: &TypedExpr,
        existing_row: &Row,
        proposed_values: &[Value],
        context: &ExecutionContext,
    ) -> DbResult<Value> {
        self.evaluator
            .evaluate_with_row_and_resolver(expr, existing_row, &|sub_expr| match &sub_expr.kind {
                TypedExprKind::ColumnRef { name, ordinal }
                | TypedExprKind::OuterColumnRef { name, ordinal }
                    if on_conflict_refers_to_excluded(name) =>
                {
                    Some(Ok(proposed_values
                        .get(*ordinal)
                        .cloned()
                        .unwrap_or(Value::Null)))
                }
                _ => self.resolve_special_expr(sub_expr, Some(existing_row), context),
            })
    }

    /// Enforce UNIQUE constraints on INSERT.
    ///
    /// For each unique index on the table, verify that the values in the key
    /// columns of the new row do not duplicate any existing row. Per SQL
    /// semantics, NULL values are considered distinct, so a key containing
    /// any NULL never conflicts.
    pub(super) fn enforce_unique_on_insert(
        &self,
        table_id: RelationId,
        row_values: &[Value],
        context: &ExecutionContext,
    ) -> DbResult<()> {
        self.enforce_unique(table_id, row_values, None, context)
    }

    /// Enforce UNIQUE constraints on UPDATE.
    ///
    /// Same as INSERT enforcement, but the row currently being updated
    /// (identified by `exclude_tuple`) must be excluded from the duplicate
    /// List the unique indexes covering `table_id` once per UPDATE
    /// statement and pre-resolve their key column ordinals. PostgreSQL
    /// caches the same shape on the relation handle in
    /// `RelationGetIndexAttrBitmap`/`indexInfo`. Resolving the ordinal
    /// vector here once turns the per-row HOT-skip check into a slice
    /// scan instead of a HashMap rebuild.
    pub(super) fn list_unique_indexes_for_update(
        &self,
        table: &TableDescriptor,
        context: &ExecutionContext,
    ) -> DbResult<Vec<UniqueIndexForUpdate>> {
        let mut entries = Vec::new();
        for index in self
            .catalog_reader
            .list_indexes(context.txn_id, table.table_id)?
            .into_iter()
            .filter(|idx| idx.unique)
        {
            let key_ordinals = resolve_key_column_ordinals(table, &index.key_columns);
            let is_expression_index = self.expression_index_meta(index.index_id).is_some();
            entries.push(UniqueIndexForUpdate {
                descriptor: index,
                key_ordinals,
                is_expression_index,
            });
        }
        Ok(entries)
    }

    /// Per-row UNIQUE enforcement using a pre-listed index set and the
    /// already-scanned old row. Equivalent to
    /// `enforce_unique_on_update`, but skips the per-row catalog walk
    /// (`list_indexes`/`get_table_by_id`), the per-index
    /// `build_column_id_map` rebuild, and the redundant
    /// `storage_dml.fetch(exclude_tuple)` round-trip - the heap scan
    /// that drives the UPDATE already produced the old tuple.
    pub(super) fn enforce_unique_on_update_with_old_row(
        &self,
        table: &TableDescriptor,
        unique_indexes: &[UniqueIndexForUpdate],
        old_row: &Row,
        new_values: &[Value],
        exclude_tuple: TupleId,
        context: &ExecutionContext,
    ) -> DbResult<()> {
        if unique_indexes.is_empty() {
            return Ok(());
        }
        for entry in unique_indexes {
            // HOT-style fast path: for non-expression UNIQUE indexes,
            // if key values are unchanged there is no new conflict to
            // detect.
            if !entry.is_expression_index
                && entry.key_ordinals.len() == entry.descriptor.key_columns.len()
            {
                let unchanged = entry.key_ordinals.iter().all(|&ord| {
                    let old = old_row.values.get(ord).unwrap_or(&Value::Null);
                    let new = new_values.get(ord).unwrap_or(&Value::Null);
                    if entry.descriptor.nulls_not_distinct {
                        old == new
                    } else {
                        unique_values_equal(old, new)
                    }
                });
                if unchanged {
                    continue;
                }
            }
            self.check_unique_index(
                table,
                &entry.descriptor,
                new_values,
                Some(exclude_tuple),
                context,
            )?;
        }
        Ok(())
    }

    /// check so that updating a row to its own values does not trigger a
    /// violation.
    pub(super) fn enforce_unique_on_update(
        &self,
        table_id: RelationId,
        new_values: &[Value],
        exclude_tuple: TupleId,
        context: &ExecutionContext,
    ) -> DbResult<()> {
        let unique_indexes = self
            .catalog_reader
            .list_indexes(context.txn_id, table_id)?
            .into_iter()
            .filter(|idx| idx.unique)
            .collect::<Vec<_>>();

        if unique_indexes.is_empty() {
            return Ok(());
        }

        let table = self
            .catalog_reader
            .get_table_by_id(context.txn_id, table_id)?
            .ok_or_else(|| {
                DbError::internal(format!("table {table_id:?} not found for UNIQUE check"))
            })?;

        let old_row = self.storage_dml.fetch(
            context.txn_id,
            &context.snapshot,
            table_id,
            exclude_tuple,
            None,
        )?;

        for index in &unique_indexes {
            if let Some(old_row) = old_row.as_ref() {
                // HOT-style fast path: for non-expression UNIQUE indexes, if
                // key values are unchanged there is no new conflict to detect.
                if self.expression_index_meta(index.index_id).is_none() {
                    let ordinals = resolve_key_column_ordinals(&table, &index.key_columns);
                    if ordinals.len() == index.key_columns.len() {
                        let unchanged = ordinals.iter().all(|&ord| {
                            let old = old_row.values.get(ord).unwrap_or(&Value::Null);
                            let new = new_values.get(ord).unwrap_or(&Value::Null);
                            if index.nulls_not_distinct {
                                old == new
                            } else {
                                unique_values_equal(old, new)
                            }
                        });
                        if unchanged {
                            continue;
                        }
                    }
                }
            }
            self.check_unique_index(&table, index, new_values, Some(exclude_tuple), context)?;
        }
        Ok(())
    }

    /// Core UNIQUE constraint enforcement.
    fn enforce_unique(
        &self,
        table_id: RelationId,
        row_values: &[Value],
        exclude_tuple: Option<TupleId>,
        context: &ExecutionContext,
    ) -> DbResult<()> {
        let unique_indexes = self
            .catalog_reader
            .list_indexes(context.txn_id, table_id)?
            .into_iter()
            .filter(|idx| idx.unique)
            .collect::<Vec<_>>();

        if unique_indexes.is_empty() {
            return Ok(());
        }

        let table = self
            .catalog_reader
            .get_table_by_id(context.txn_id, table_id)?
            .ok_or_else(|| {
                DbError::internal(format!("table {table_id:?} not found for UNIQUE check"))
            })?;

        for index in &unique_indexes {
            self.check_unique_index(&table, index, row_values, exclude_tuple, context)?;
        }

        Ok(())
    }

    pub(super) fn prepare_unique_insert_state(
        &self,
        table_id: RelationId,
        probe_table_empty: bool,
        context: &ExecutionContext,
    ) -> DbResult<UniqueInsertState> {
        let table = self
            .catalog_reader
            .get_table_by_id(context.txn_id, table_id)?
            .ok_or_else(|| {
                DbError::internal(format!("table {table_id:?} not found for UNIQUE check"))
            })?;

        let mut indexes = Vec::new();
        for index in self
            .catalog_reader
            .list_indexes(context.txn_id, table_id)?
            .into_iter()
            .filter(|idx| idx.unique)
        {
            let ordinals = resolve_key_column_ordinals(&table, &index.key_columns);
            if ordinals.len() != index.key_columns.len() {
                return Err(DbError::internal(
                    "UNIQUE index column ordinals could not be resolved",
                ));
            }
            let column_names = resolve_key_column_names(&table, &index.key_columns);
            indexes.push(PreparedUniqueIndex {
                descriptor: index,
                ordinals,
                column_names,
            });
        }

        let table_was_empty = if !probe_table_empty || indexes.is_empty() {
            false
        } else {
            let mut stream = self.scan_table_locked(context, table_id, None)?;
            stream.next()?.is_none()
        };
        let seen_keys = (0..indexes.len()).map(|_| HashSet::new()).collect();

        Ok(UniqueInsertState {
            table,
            indexes,
            table_was_empty,
            seen_keys,
        })
    }

    pub(super) fn enforce_unique_with_state(
        &self,
        state: &mut UniqueInsertState,
        row_values: &[Value],
        exclude_tuple: Option<TupleId>,
        context: &ExecutionContext,
    ) -> DbResult<()> {
        for (index_pos, index) in state.indexes.iter().enumerate() {
            let new_key_values: Vec<Value> = index
                .ordinals
                .iter()
                .map(|&ord| row_values[ord].clone())
                .collect();
            let new_key_refs: Vec<&Value> = new_key_values.iter().collect();

            if !index.descriptor.nulls_not_distinct && new_key_refs.iter().any(|v| v.is_null()) {
                continue;
            }

            if state.table_was_empty {
                let hash_key: Vec<ValueHashKey> = new_key_values
                    .iter()
                    .map(build_hash_key)
                    .collect::<DbResult<_>>()?;
                if !state.seen_keys[index_pos].insert(hash_key) {
                    return Err(unique_violation_error_with_detail(
                        &state.table,
                        &index.descriptor,
                        &index.column_names,
                        &new_key_refs,
                    ));
                }
                continue;
            }

            let has_nullable_nd_key =
                index.descriptor.nulls_not_distinct && new_key_refs.iter().any(|v| v.is_null());
            let has_conflict = if let Some(has_conflict) = self
                .fast_single_column_unique_count_has_conflict(
                    state.table.table_id,
                    &index.descriptor,
                    &new_key_values,
                    exclude_tuple,
                    context,
                )? {
                has_conflict
            } else if index.descriptor.kind == IndexKind::BTree && !has_nullable_nd_key {
                self.fast_exact_unique_index_has_conflict(
                    state.table.table_id,
                    index.descriptor.index_id,
                    &new_key_values,
                    exclude_tuple,
                    context,
                )?
            } else {
                let mut stream = self.scan_table_locked(context, state.table.table_id, None)?;
                self.stream_has_unique_conflict(
                    &mut stream,
                    &index.ordinals,
                    &new_key_refs,
                    index.descriptor.nulls_not_distinct,
                    exclude_tuple,
                    context,
                )?
            };
            if has_conflict {
                return Err(unique_violation_error_with_detail(
                    &state.table,
                    &index.descriptor,
                    &index.column_names,
                    &new_key_refs,
                ));
            }
        }

        Ok(())
    }

    /// Scan a stream of records for a matching key, returning `true` if a
    /// conflicting row was found (skipping `exclude_tuple` if set).
    fn stream_has_unique_conflict(
        &self,
        stream: &mut Box<dyn TupleStream>,
        ordinals: &[usize],
        key_refs: &[&Value],
        nulls_not_distinct: bool,
        exclude_tuple: Option<TupleId>,
        context: &ExecutionContext,
    ) -> DbResult<bool> {
        let has_interrupts = context.has_execution_interrupts();
        let mut row_counter: u32 = 0;
        while let Some(record) = stream.next()? {
            if has_interrupts {
                row_counter = row_counter.wrapping_add(1);
                if row_counter.trailing_zeros() >= 10 {
                    context.check_deadline()?;
                }
            }
            if let Some(exclude) = exclude_tuple {
                if record.tuple_id == exclude {
                    continue;
                }
            }
            if ordinals.iter().zip(key_refs.iter()).all(|(&ord, new_val)| {
                if nulls_not_distinct {
                    &record.row.values[ord] == *new_val
                } else {
                    unique_values_equal(&record.row.values[ord], new_val)
                }
            }) {
                return Ok(true);
            }
        }
        Ok(false)
    }

    fn fast_exact_unique_index_has_conflict(
        &self,
        table_id: RelationId,
        index_id: IndexId,
        key_values: &[Value],
        exclude_tuple: Option<TupleId>,
        context: &ExecutionContext,
    ) -> DbResult<bool> {
        let mut stream = self.scan_index_locked(
            context,
            table_id,
            index_id,
            KeyRange::point(key_values.to_vec()),
            Some(Vec::new()),
        )?;
        // Index-point lookup: typically zero or one matching row, so the
        // per-row deadline check is cold. Keep it but only call once after
        // the loop finishes if we're going to fall through to "no conflict".
        let has_interrupts = context.has_execution_interrupts();
        while let Some(record) = stream.next()? {
            if exclude_tuple.is_some_and(|exclude| record.tuple_id == exclude) {
                continue;
            }
            return Ok(true);
        }
        if has_interrupts {
            context.check_deadline()?;
        }
        Ok(false)
    }

    fn fast_single_column_unique_count_has_conflict(
        &self,
        table_id: RelationId,
        index: &IndexDescriptor,
        key_values: &[Value],
        exclude_tuple: Option<TupleId>,
        context: &ExecutionContext,
    ) -> DbResult<Option<bool>> {
        if exclude_tuple.is_some()
            || context.txn_id != TxnId::default()
            || index.key_columns.len() != 1
            || key_values.len() != 1
            || key_values[0].is_null()
            || self.expression_index_meta(index.index_id).is_some()
        {
            return Ok(None);
        }

        let latest_snapshot =
            aiondb_tx::Snapshot::new(TxnId::default(), TxnId::default(), Vec::new());
        match self.storage_dml.visible_eq_row_count(
            TxnId::default(),
            &latest_snapshot,
            table_id,
            index.key_columns[0].column_id,
            &key_values[0],
        ) {
            Ok(count) => Ok(Some(count > 0)),
            Err(error) if error.sqlstate() == SqlState::FeatureNotSupported => Ok(None),
            Err(error) => Err(error),
        }
    }

    /// Check that the new row's key values do not conflict with any existing
    /// row for a single unique index.
    fn check_unique_index(
        &self,
        table: &TableDescriptor,
        index: &IndexDescriptor,
        row_values: &[Value],
        exclude_tuple: Option<TupleId>,
        context: &ExecutionContext,
    ) -> DbResult<()> {
        if let Some(expr_meta) = self.expression_index_meta(index.index_id) {
            let base_ordinals = if expr_meta.expression_only {
                Vec::new()
            } else {
                let resolved = resolve_key_column_ordinals(table, &index.key_columns);
                if resolved.len() != index.key_columns.len() {
                    return Err(DbError::internal(
                        "UNIQUE index column ordinals could not be resolved",
                    ));
                }
                resolved
            };
            let input_row = Row::new(row_values.to_vec());
            let mut new_key_values: Vec<Value> = base_ordinals
                .iter()
                .map(|&ord| row_values.get(ord).cloned().unwrap_or(Value::Null))
                .collect();
            for expression in &expr_meta.typed_expressions {
                new_key_values.push(self.evaluate_expr_with_row(expression, &input_row, context)?);
            }
            let new_key_refs: Vec<&Value> = new_key_values.iter().collect();

            if !index.nulls_not_distinct && new_key_refs.iter().any(|v| v.is_null()) {
                return Ok(());
            }

            let mut stream = self.scan_table_locked(context, table.table_id, None)?;
            while let Some(record) = stream.next()? {
                context.check_deadline()?;
                if let Some(exclude) = exclude_tuple {
                    if record.tuple_id == exclude {
                        continue;
                    }
                }

                let mut existing_key_values: Vec<Value> = base_ordinals
                    .iter()
                    .map(|&ord| record.row.values.get(ord).cloned().unwrap_or(Value::Null))
                    .collect();
                for expression in &expr_meta.typed_expressions {
                    existing_key_values.push(self.evaluate_expr_with_row(
                        expression,
                        &record.row,
                        context,
                    )?);
                }

                let matches = existing_key_values.iter().zip(new_key_refs.iter()).all(
                    |(existing, incoming)| {
                        if index.nulls_not_distinct {
                            existing == *incoming
                        } else {
                            unique_values_equal(existing, incoming)
                        }
                    },
                );
                if matches {
                    let mut col_names = if expr_meta.expression_only {
                        Vec::new()
                    } else {
                        resolve_key_column_names(table, &index.key_columns)
                    };
                    col_names.extend(expr_meta.display_expressions.iter().cloned());
                    return Err(unique_violation_error_with_detail(
                        table,
                        index,
                        &col_names,
                        &new_key_refs,
                    ));
                }
            }
            return Ok(());
        }

        // Map index key column IDs to ordinal positions in the table.
        let ordinals = resolve_key_column_ordinals(table, &index.key_columns);
        if ordinals.len() != index.key_columns.len() {
            return Err(DbError::internal(
                "UNIQUE index column ordinals could not be resolved",
            ));
        }

        // Extract the new row's key values.
        let new_key_values: Vec<Value> = ordinals
            .iter()
            .map(|&ord| row_values[ord].clone())
            .collect();
        let new_key_refs: Vec<&Value> = new_key_values.iter().collect();

        // SQL semantics: for regular UNIQUE indexes, a key containing NULL is
        // distinct. For NULLS NOT DISTINCT indexes, NULL participates in
        // duplicate detection.
        if !index.nulls_not_distinct && new_key_refs.iter().any(|v| v.is_null()) {
            return Ok(());
        }

        // Scan via index (if B-tree) or full table scan.
        let has_nullable_nd_key =
            index.nulls_not_distinct && new_key_refs.iter().any(|v| v.is_null());
        let has_conflict = if let Some(has_conflict) = self
            .fast_single_column_unique_count_has_conflict(
                table.table_id,
                index,
                &new_key_values,
                exclude_tuple,
                context,
            )? {
            has_conflict
        } else if index.kind == IndexKind::BTree && !has_nullable_nd_key {
            self.fast_exact_unique_index_has_conflict(
                table.table_id,
                index.index_id,
                &new_key_values,
                exclude_tuple,
                context,
            )?
        } else {
            let mut stream = self.scan_table_locked(context, table.table_id, None)?;
            self.stream_has_unique_conflict(
                &mut stream,
                &ordinals,
                &new_key_refs,
                index.nulls_not_distinct,
                exclude_tuple,
                context,
            )?
        };
        if has_conflict {
            let col_names = resolve_key_column_names(table, &index.key_columns);
            return Err(unique_violation_error_with_detail(
                table,
                index,
                &col_names,
                &new_key_refs,
            ));
        }
        Ok(())
    }

    /// Attempt to insert a row, handling ON CONFLICT if present.
    ///
    /// Returns `true` if a row was inserted (or updated via DO UPDATE),
    /// `false` if the row was skipped (DO NOTHING).
    pub(super) fn try_insert_with_on_conflict(
        &self,
        table_id: RelationId,
        values: Vec<Value>,
        on_conflict: Option<&InsertOnConflict>,
        context: &ExecutionContext,
    ) -> DbResult<Option<Row>> {
        if self
            .catalog_reader
            .list_indexes(context.txn_id, table_id)?
            .into_iter()
            .any(|index| index.unique)
        {
            self.lock_table(context, table_id, LockMode::Update)?;
        }
        match on_conflict {
            None => {
                // No ON CONFLICT: normal insert path
                self.enforce_unique_on_insert(table_id, &values, context)?;
                let row = Row::new(values);
                self.insert_locked(context, table_id, row.clone())?;
                self.fire_after_insert_triggers(table_id, &row.values, context)?;
                Ok(Some(row))
            }
            Some(oc) => {
                // Try the unique enforcement; catch UniqueViolation
                match self.enforce_unique_on_insert(table_id, &values, context) {
                    Ok(()) => {
                        // No conflict - normal insert
                        let row = Row::new(values);
                        self.insert_locked(context, table_id, row.clone())?;
                        self.fire_after_insert_triggers(table_id, &row.values, context)?;
                        Ok(Some(row))
                    }
                    Err(err) if err.sqlstate() == SqlState::UniqueViolation => match &oc.action {
                        OnConflictActionPlan::DoNothing => Ok(None),
                        OnConflictActionPlan::DoUpdate {
                            assignments,
                            where_clause,
                        } => {
                            let updated = self.handle_do_update(
                                table_id,
                                &values,
                                &oc.columns,
                                assignments,
                                where_clause.as_ref(),
                                context,
                            )?;
                            Ok(updated)
                        }
                    },
                    Err(err) => Err(err),
                }
            }
        }
    }

    /// Handle DO UPDATE: find the conflicting row and update it.
    ///
    /// If `where_clause` is `Some`, the update is only applied when the
    /// condition evaluates to true for the conflicting row.  Otherwise the
    /// as DO NOTHING for that row).
    fn handle_do_update(
        &self,
        table_id: RelationId,
        proposed_values: &[Value],
        conflict_columns: &[String],
        assignments: &[PlanUpdateAssignment],
        where_clause: Option<&TypedExpr>,
        context: &ExecutionContext,
    ) -> DbResult<Option<Row>> {
        let table = self
            .catalog_reader
            .get_table_by_id(context.txn_id, table_id)?
            .ok_or_else(|| {
                DbError::internal(format!("table {table_id:?} not found for ON CONFLICT"))
            })?;

        // Resolve conflict column names to ordinal positions.
        let col_name_map: std::collections::HashMap<String, usize> = table
            .columns
            .iter()
            .enumerate()
            .map(|(i, col)| (col.name.to_ascii_lowercase(), i))
            .collect();
        let conflict_ordinals: Vec<usize> = conflict_columns
            .iter()
            .map(|name| {
                col_name_map
                    .get(&name.to_ascii_lowercase())
                    .copied()
                    .ok_or_else(|| {
                        DbError::internal(format!(
                            "ON CONFLICT column \"{name}\" not found in table"
                        ))
                    })
            })
            .collect::<DbResult<Vec<_>>>()?;

        // Find the conflicting row by scanning for matching conflict column values.
        let mut stream = self.scan_table_locked(context, table_id, None)?;

        while let Some(record) = stream.next()? {
            context.check_deadline()?;
            let all_match = conflict_ordinals.iter().all(|&ord| {
                let existing = &record.row.values[ord];
                let proposed = &proposed_values[ord];
                !existing.is_null() && !proposed.is_null() && existing == proposed
            });

            if !all_match {
                continue;
            }

            // Found the conflicting row.

            // Evaluate the optional WHERE clause against the existing row.
            // If it evaluates to false/null the update is skipped (row is
            // left untouched, equivalent to DO NOTHING for this row).
            if let Some(where_expr) = where_clause {
                let cond = self.evaluate_on_conflict_expr_with_rows(
                    where_expr,
                    &record.row,
                    proposed_values,
                    context,
                )?;
                if cond != Value::Boolean(true) {
                    return Ok(None);
                }
            }

            let tuple_id = record.tuple_id;
            if context.tuple_written_in_statement(table_id, tuple_id) {
                // PostgreSQL does not apply ON CONFLICT DO UPDATE to rows that
                // were already written earlier in the same top-level statement
                // (e.g. by a prior data-modifying CTE step).
                return Ok(None);
            }
            if self.statement_already_touched_conflict_key(
                table_id,
                &conflict_ordinals,
                proposed_values,
                context,
            )? {
                return Ok(None);
            }
            let existing_row = record.row;

            let mut new_values = existing_row.values.clone();
            for assignment in assignments {
                let value = self.evaluate_on_conflict_expr_with_rows(
                    &assignment.expr,
                    &existing_row,
                    proposed_values,
                    context,
                )?;
                let text_type_modifier = table
                    .columns
                    .get(assignment.column_ordinal)
                    .and_then(|column| column.text_type_modifier);
                let value = coerce_assigned_value(
                    value,
                    &assignment.data_type,
                    assignment.nullable,
                    text_type_modifier,
                )?;
                new_values[assignment.column_ordinal] = value;
            }

            let nullified_conflict_key = conflict_ordinals.iter().any(|&ord| {
                let existing = existing_row.values.get(ord).unwrap_or(&Value::Null);
                let proposed = proposed_values.get(ord).unwrap_or(&Value::Null);
                let updated = new_values.get(ord).unwrap_or(&Value::Null);
                !existing.is_null() && !proposed.is_null() && updated.is_null()
            });
            if nullified_conflict_key {
                // PostgreSQL semantics for ON CONFLICT avoid applying a second
                // conflict update when the assignment source row is absent in
                // this statement context. Treat this row as not updated.
                return Ok(None);
            }

            // Fire BEFORE UPDATE triggers; skip if vetoed.
            if !self.fire_before_update_triggers(
                table_id,
                &mut new_values,
                &existing_row.values,
                context,
            )? {
                return Ok(None);
            }
            self.enforce_fk_on_update(table_id, &new_values, context)?;
            self.enforce_check_on_update(table_id, &new_values, context)?;
            self.enforce_unique_on_update(table_id, &new_values, tuple_id, context)?;
            let updated_row = Row::new(new_values);
            self.update_locked(
                context,
                table_id,
                tuple_id,
                Some(&existing_row),
                updated_row.clone(),
            )?;
            self.fire_after_update_triggers(
                table_id,
                &updated_row.values,
                &existing_row.values,
                context,
            )?;
            return Ok(Some(updated_row));
        }

        // If no conflicting row found (shouldn't happen since enforce_unique
        // raised UniqueViolation), fall back to a plain insert.
        let inserted_row = Row::new(proposed_values.to_vec());
        self.insert_locked(context, table_id, inserted_row.clone())?;
        Ok(Some(inserted_row))
    }
}

fn on_conflict_refers_to_excluded(name: &str) -> bool {
    name.split_once('\0')
        .is_some_and(|(qualifier, _)| qualifier.eq_ignore_ascii_case("excluded"))
}

/// Build a `column_id` → ordinal lookup map for a table.
fn build_column_id_map(table: &TableDescriptor) -> std::collections::HashMap<ColumnId, usize> {
    table
        .columns
        .iter()
        .enumerate()
        .map(|(i, col)| (col.column_id, i))
        .collect()
}

/// Map `IndexKeyColumn` entries to zero-based ordinal positions in the table
/// by matching on `column_id`.
fn resolve_key_column_ordinals(
    table: &TableDescriptor,
    key_columns: &[IndexKeyColumn],
) -> Vec<usize> {
    let col_map = build_column_id_map(table);
    key_columns
        .iter()
        .filter_map(|kc| col_map.get(&kc.column_id).copied())
        .collect()
}

/// Resolve `IndexKeyColumn` entries to column names for error messages.
fn resolve_key_column_names(
    table: &TableDescriptor,
    key_columns: &[IndexKeyColumn],
) -> Vec<String> {
    key_columns
        .iter()
        .filter_map(|kc| {
            table
                .columns
                .iter()
                .find(|col| col.column_id == kc.column_id)
                .map(|col| col.name.clone())
        })
        .collect()
}

use super::fk_enforcement::values_equal as unique_values_equal;

impl Executor {
    /// Create backing unique indexes for PRIMARY KEY and UNIQUE constraints on
    /// a newly created table.
    ///
    /// Called from the `CreateTable` handler in `command_plans.rs` after the
    /// table has been written to the catalog and storage.
    pub(super) fn create_constraint_backing_indexes(
        &self,
        table: &TableDescriptor,
        primary_key_columns: &[String],
        unique_constraints: &[aiondb_plan::UniqueConstraintPlan],
        context: &ExecutionContext,
    ) -> DbResult<()> {
        if !primary_key_columns.is_empty() {
            self.create_constraint_backing_index(table, primary_key_columns, None, true, context)?;
        }

        for constraint in unique_constraints {
            self.create_constraint_backing_index(
                table,
                &constraint.columns,
                constraint.name.as_deref(),
                false,
                context,
            )?;
        }

        Ok(())
    }

    /// Validate existing rows for a PRIMARY KEY or UNIQUE constraint before it
    /// is added to a non-empty table.
    pub(super) fn validate_constraint_backing_index(
        &self,
        table: &TableDescriptor,
        columns: &[String],
        constraint_name: Option<&str>,
        primary_key: bool,
        context: &ExecutionContext,
    ) -> DbResult<()> {
        if primary_key && table.primary_key.is_some() {
            return Err(DbError::constraint_error(
                SqlState::InvalidTableDefinition,
                format!(
                    "multiple primary keys for table \"{}\" are not allowed",
                    table.name.object_name()
                ),
            ));
        }

        let ordinals = resolve_constraint_column_ordinals(table, columns)?;
        let mut seen_keys: HashSet<Vec<ValueHashKey>> = HashSet::new();

        let mut stream = self.scan_table_locked(context, table.table_id, None)?;
        while let Some(record) = stream.next()? {
            context.check_deadline()?;
            let key: Vec<Value> = ordinals
                .iter()
                .map(|&ordinal| {
                    record
                        .row
                        .values
                        .get(ordinal)
                        .cloned()
                        .ok_or_else(|| DbError::internal("row is missing constrained value"))
                })
                .collect::<DbResult<_>>()?;

            if primary_key {
                if let Some(null_index) = key.iter().position(Value::is_null) {
                    return Err(DbError::constraint_error(
                        SqlState::CheckViolation,
                        format!(
                            "primary key column \"{}\" contains NULL values",
                            columns[null_index]
                        ),
                    ));
                }
            }

            if key.iter().any(Value::is_null) {
                continue;
            }

            let hash_key: Vec<ValueHashKey> =
                key.iter().map(build_hash_key).collect::<DbResult<_>>()?;
            if !seen_keys.insert(hash_key) {
                return Err(unique_constraint_violation(
                    table,
                    columns,
                    constraint_name,
                    primary_key,
                ));
            }
        }

        Ok(())
    }

    /// Create the unique index that backs a PRIMARY KEY or UNIQUE constraint.
    pub(super) fn create_constraint_backing_index(
        &self,
        table: &TableDescriptor,
        columns: &[String],
        constraint_name: Option<&str>,
        primary_key: bool,
        context: &ExecutionContext,
    ) -> DbResult<()> {
        let key_columns = resolve_constraint_key_columns(table, columns)?;
        let index_name =
            constraint_backing_index_name(table, columns, constraint_name, primary_key);
        let index_descriptor = IndexDescriptor {
            index_id: IndexId::default(),
            schema_id: table.schema_id,
            table_id: table.table_id,
            name: parse_qualified_name(&index_name),
            unique: true,
            nulls_not_distinct: false,
            kind: IndexKind::BTree,
            key_columns,
            include_columns: Vec::new(),
            constraint_name: (!primary_key).then(|| index_name.clone()),
            hnsw_params: None,
        };
        let index_id = self
            .catalog_writer
            .create_index(context.txn_id, index_descriptor)?;
        let index = self
            .catalog_reader
            .get_index(context.txn_id, index_id)?
            .ok_or_else(|| {
                DbError::internal(format!(
                    "created constraint index \"{index_name}\" is missing from catalog"
                ))
            })?;
        let idx_storage = to_index_storage_descriptor(&index)?;
        self.storage_ddl
            .create_index_storage(context.txn_id, &idx_storage)?;
        Ok(())
    }
}

fn resolve_constraint_column_ordinals(
    table: &TableDescriptor,
    columns: &[String],
) -> DbResult<Vec<usize>> {
    columns
        .iter()
        .map(|name| {
            table
                .columns
                .iter()
                .position(|column| column.name.eq_ignore_ascii_case(name))
                .ok_or_else(|| {
                    DbError::bind_error(
                        SqlState::UndefinedColumn,
                        format!(
                            "column \"{name}\" of relation \"{}\" does not exist",
                            table.name.object_name()
                        ),
                    )
                })
        })
        .collect()
}

fn resolve_constraint_key_columns(
    table: &TableDescriptor,
    columns: &[String],
) -> DbResult<Vec<IndexKeyColumn>> {
    columns
        .iter()
        .map(|name| {
            let column = table
                .columns
                .iter()
                .find(|column| column.name.eq_ignore_ascii_case(name))
                .ok_or_else(|| {
                    DbError::bind_error(
                        SqlState::UndefinedColumn,
                        format!(
                            "column \"{name}\" of relation \"{}\" does not exist",
                            table.name.object_name()
                        ),
                    )
                })?;
            Ok(IndexKeyColumn {
                column_id: column.column_id,
                sort_order: SortOrder::Ascending,
                nulls_first: false,
            })
        })
        .collect()
}

fn constraint_backing_index_name(
    table: &TableDescriptor,
    columns: &[String],
    constraint_name: Option<&str>,
    primary_key: bool,
) -> String {
    if let Some(name) = constraint_name {
        return name.to_owned();
    }

    if primary_key {
        format!("{}_pkey", table.name.object_name())
    } else {
        format!("{}_{}_unique", table.name.object_name(), columns.join("_"))
    }
}

fn default_unique_constraint_name(table: &TableDescriptor, columns: &[String]) -> String {
    format!("{}_{}_key", table.name.object_name(), columns.join("_"))
}

fn unique_violation_name(
    table: &TableDescriptor,
    index: &IndexDescriptor,
    columns: &[String],
) -> String {
    if index.name.object_name() == constraint_backing_index_name(table, columns, None, false) {
        default_unique_constraint_name(table, columns)
    } else {
        index.name.object_name().to_owned()
    }
}

fn format_unique_violation_detail(columns: &[String], values: &[&Value]) -> String {
    let rendered_values = values
        .iter()
        .map(|value| value.to_string())
        .collect::<Vec<_>>()
        .join(", ");
    format!(
        "Key ({})=({rendered_values}) already exists.",
        columns.join(", ")
    )
}

/// Build a unique violation error with detail for a given index and key values.
fn unique_violation_error_with_detail(
    table: &TableDescriptor,
    index: &IndexDescriptor,
    column_names: &[String],
    key_refs: &[&Value],
) -> DbError {
    DbError::constraint_error(
        SqlState::UniqueViolation,
        format!(
            "duplicate key value violates unique constraint \"{}\"",
            unique_violation_name(table, index, column_names),
        ),
    )
    .with_client_detail(format_unique_violation_detail(column_names, key_refs))
}

fn unique_constraint_violation(
    table: &TableDescriptor,
    columns: &[String],
    constraint_name: Option<&str>,
    primary_key: bool,
) -> DbError {
    let constraint_name = if let Some(name) = constraint_name {
        name.to_owned()
    } else if primary_key {
        constraint_backing_index_name(table, columns, None, true)
    } else {
        default_unique_constraint_name(table, columns)
    };
    DbError::constraint_error(
        SqlState::UniqueViolation,
        format!("duplicate key value violates unique constraint \"{constraint_name}\""),
    )
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use aiondb_core::Value;
    use aiondb_eval::{build_hash_key, ValueHashKey};

    /// Verify that the hash-based duplicate detection correctly identifies
    /// identical key vectors while accepting distinct ones.
    #[test]
    fn hash_based_duplicate_detection_is_correct() {
        let mut seen: HashSet<Vec<ValueHashKey>> = HashSet::new();

        let key_a = [Value::Int(1), Value::Text("hello".into())];
        let key_b = [Value::Int(1), Value::Text("hello".into())];
        let key_c = [Value::Int(2), Value::Text("world".into())];

        let hash_a: Vec<ValueHashKey> = key_a.iter().map(|v| build_hash_key(v).unwrap()).collect();
        let hash_b: Vec<ValueHashKey> = key_b.iter().map(|v| build_hash_key(v).unwrap()).collect();
        let hash_c: Vec<ValueHashKey> = key_c.iter().map(|v| build_hash_key(v).unwrap()).collect();

        // First insert succeeds.
        assert!(seen.insert(hash_a), "first insert of key_a should succeed");

        // Duplicate insert returns false (already present).
        assert!(
            !seen.insert(hash_b),
            "duplicate key_b should be detected as already present"
        );

        // Distinct key insert succeeds.
        assert!(seen.insert(hash_c), "distinct key_c should succeed");

        assert_eq!(seen.len(), 2);
    }

    /// Verify that NULL keys are never considered duplicates of each other,
    /// matching SQL NULL-as-distinct semantics. The caller must skip NULL keys
    /// before inserting into the set; this test confirms that if NULLs were
    /// inserted they would hash equally (the caller is responsible for the
    /// skip, not the data structure).
    #[test]
    fn null_keys_hash_equally_so_caller_must_skip() {
        let mut seen: HashSet<Vec<ValueHashKey>> = HashSet::new();

        let null_key = [Value::Null, Value::Int(1)];
        let hash: Vec<ValueHashKey> = null_key
            .iter()
            .map(|v| build_hash_key(v).unwrap())
            .collect();
        let hash2: Vec<ValueHashKey> = null_key
            .iter()
            .map(|v| build_hash_key(v).unwrap())
            .collect();

        assert!(seen.insert(hash));
        // Second insert of identical null-containing key is detected.
        // This confirms the caller MUST skip null keys to preserve SQL semantics.
        assert!(!seen.insert(hash2));
    }
}
