use super::*;
use aiondb_plan::UpdateAssignment as PlanUpdateAssignment;

use crate::executor::check_enforcement::CompiledCheckConstraint;
use crate::executor::fk_enforcement::{CompiledChildFkCheck, ReferencingUpdateFkEntry};
use crate::executor::unique_enforcement::UniqueIndexForUpdate;

#[path = "dml_plans_filters.rs"]
mod dml_plans_filters;

#[path = "dml_plans_rls.rs"]
mod dml_plans_rls;
pub(super) use dml_plans_filters::{
    best_composite_eq_lookup_index, best_eq_lookup_index, build_dml_literal_key_set,
    enforce_not_null_constraints, enforce_not_null_constraints_for_table,
    enforce_not_null_constraints_on_updated_columns, extract_dml_case_lookup_assignment,
    extract_dml_composite_eq_literal_filter, extract_dml_in_literal_filter,
    extract_dml_or_eq_literal_filter, extract_dml_range_literal_filter,
    extract_dml_simple_eq_literal_filter, extract_update_from_hash_join_plan,
    row_matches_dml_literal_key_set, row_matches_dml_range_bound,
    row_matches_dml_simple_eq_literal_filter, updated_not_null_check_ordinals,
    value_matches_column_type_exactly, DmlCaseLookupAssignment, UpdateFromHashJoinPlan,
};

thread_local! {
    /// Per-thread direct INSERT eligibility cache. The positive case is
    /// the common OLTP shape: INSERT VALUES into a simple table with no
    /// RLS, FK/check constraints, INSERT triggers, or non-PK unique
    /// indexes. Keying by catalog revision keeps DDL invalidation simple.
    static DIRECT_INSERT_PATH_CACHE: RefCell<HashMap<(u64, RelationId), Option<TableDescriptor>>> =
        RefCell::new(HashMap::new());

    /// Per-thread UPDATE metadata fast-path cache. The updated-column
    /// vector is part of the key because unique-index checks only matter
    /// when the UPDATE touches a column covered by such an index.
    static SIMPLE_UPDATE_PATH_CACHE: RefCell<HashMap<(u64, RelationId, Vec<usize>), Option<TableDescriptor>>> =
        RefCell::new(HashMap::new());
}

fn update_assignment_ordinals_key(assignments: &[aiondb_plan::UpdateAssignment]) -> Vec<usize> {
    let mut ordinals: Vec<usize> = assignments
        .iter()
        .map(|assignment| assignment.column_ordinal)
        .collect();
    ordinals.sort_unstable();
    ordinals.dedup();
    ordinals
}

/// Split a `key=value, key=value, …` joined options string while respecting
/// matching parentheses and single-quoted SQL string literals. Top-level
/// commas inside a value (e.g. `using=cid IN (11, 22, 33)`) are not treated
/// as option separators.
pub(super) fn split_compat_options_paren_aware(options_joined: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let bytes = options_joined.as_bytes();
    let mut start = 0usize;
    let mut depth: i32 = 0;
    let mut in_string = false;
    let mut idx = 0usize;
    while idx < bytes.len() {
        let b = bytes[idx];
        if in_string {
            if b == b'\'' {
                if idx + 1 < bytes.len() && bytes[idx + 1] == b'\'' {
                    idx += 2;
                    continue;
                }
                in_string = false;
            }
            idx += 1;
            continue;
        }
        match b {
            b'\'' => in_string = true,
            b'(' | b'[' => depth += 1,
            b')' | b']' => {
                if depth > 0 {
                    depth -= 1;
                }
            }
            b',' if depth == 0 => {
                let pair = options_joined[start..idx].trim().to_owned();
                if !pair.is_empty() {
                    out.push(pair);
                }
                start = idx + 1;
            }
            _ => {}
        }
        idx += 1;
    }
    let tail = options_joined[start..].trim().to_owned();
    if !tail.is_empty() {
        out.push(tail);
    }
    out
}

#[derive(Clone)]
pub(super) struct CompatRlsPolicy {
    policy_name: Option<String>,
    permissive: bool,
    using_expr: Option<TypedExpr>,
    with_check_expr: Option<TypedExpr>,
    /// Cached `expr_requires_special_resolution(using_expr)` so the
    /// per-row scan loop doesn't re-walk the predicate tree per row.
    using_requires_special_resolution: bool,
    with_check_requires_special_resolution: bool,
}

#[derive(Clone, Copy)]
pub(super) enum CompatRlsAction {
    Select,
    Insert,
    Update,
    Delete,
}

impl CompatRlsAction {
    const fn policy_keyword(self) -> &'static str {
        match self {
            Self::Select => "select",
            Self::Insert => "insert",
            Self::Update => "update",
            Self::Delete => "delete",
        }
    }
}

impl Executor {
    pub(super) fn execute_dml_plan(
        &self,
        plan: &PhysicalPlan,
        context: &ExecutionContext,
    ) -> DbResult<ExecutionResult> {
        match plan {
            PhysicalPlan::InsertValues {
                table_id,
                columns,
                rows,
                on_conflict,
                returning,
            } => {
                if context.has_execution_interrupts() {
                    context.check_deadline()?;
                }
                let mut inserted = 0u64;
                let has_returning = !returning.is_empty();
                let returning_direct_column_ordinals = has_returning
                    .then(|| Self::projection_column_ordinals(returning))
                    .flatten();
                let needs_compat_returning_row = has_returning
                    && returning
                        .iter()
                        .any(|output| dml_expr_references_compat_system_column(&output.expr));
                let include_oid_system_column = if needs_compat_returning_row {
                    self.compat_include_oid_system_column_for_table_id(context, *table_id)?
                } else {
                    false
                };
                let direct_insert_table = self.try_direct_insert_path(
                    *table_id,
                    on_conflict.as_ref(),
                    has_returning,
                    rows.len(),
                    context,
                )?;
                let use_direct_insert = direct_insert_table.is_some();
                let probe_table_empty_for_unique = rows.len() > 1;
                let has_not_null_columns = columns.iter().any(|column| !column.nullable);
                let mut unique_state = if !use_direct_insert && on_conflict.is_none() {
                    Some(self.prepare_unique_insert_state(
                        *table_id,
                        probe_table_empty_for_unique,
                        context,
                    )?)
                } else {
                    None
                };
                let use_prelocked_insert = use_direct_insert
                    || unique_state
                        .as_ref()
                        .is_some_and(|state| state.table_was_empty());
                let mut returning_rows = if has_returning {
                    Vec::with_capacity(rows.len())
                } else {
                    Vec::new()
                };
                let mut returning_result_bytes = 0u64;
                if use_prelocked_insert {
                    context.record_relation_write(*table_id)?;
                    self.lock_table(context, *table_id, LockMode::RowExclusive)?;
                }
                // Reuse the descriptor `try_direct_insert_path` already
                // resolved when the fast path applies; only re-fetch when
                // we're on the slow path (where unique_state may have
                // observed a different state of the table anyway).
                let insert_table = if let Some(table) = direct_insert_table {
                    table
                } else {
                    self.catalog_reader
                        .get_table_by_id(context.txn_id, *table_id)?
                        .ok_or_else(|| {
                            DbError::internal(format!("table {table_id:?} not found for INSERT"))
                        })?
                };
                // `insert_table` is owned and not mutated for the rest of
                // the function, so a borrow is enough --- no need for the
                // historical `to_owned()` alloc per INSERT.
                let table_name_for_null_check: &str = insert_table.name.object_name();
                let has_fk_constraints = !insert_table.foreign_keys.is_empty();
                let has_check_constraints = !insert_table.check_constraints.is_empty();
                // `try_direct_insert_path` already proved RLS policies are
                // absent on the direct path (it returns None otherwise),
                // so we can skip the second `compile_compat_rls_policies`
                // round-trip there.
                let insert_policies = if use_direct_insert {
                    None
                } else {
                    self.compile_compat_rls_policies(
                        &insert_table,
                        CompatRlsAction::Insert,
                        context,
                    )?
                };
                let (
                    has_before_insert_row_triggers,
                    has_after_insert_row_triggers,
                    has_before_insert_statement_triggers,
                    has_after_insert_statement_triggers,
                ) = if use_direct_insert {
                    (false, false, false, false)
                } else {
                    let triggers = self.list_triggers_cached(
                        *table_id,
                        &insert_table.name.to_string(),
                        context,
                    )?;
                    let has_before_row = triggers.iter().any(|trigger| {
                        trigger.event == TriggerEventDescriptor::Insert
                            && trigger.timing == TriggerTimingDescriptor::Before
                            && trigger.for_each_row
                    });
                    let has_after_row = triggers.iter().any(|trigger| {
                        trigger.event == TriggerEventDescriptor::Insert
                            && trigger.timing == TriggerTimingDescriptor::After
                            && trigger.for_each_row
                    });
                    let has_before_statement = triggers.iter().any(|trigger| {
                        trigger.event == TriggerEventDescriptor::Insert
                            && trigger.timing == TriggerTimingDescriptor::Before
                            && !trigger.for_each_row
                    });
                    let has_after_statement = triggers.iter().any(|trigger| {
                        trigger.event == TriggerEventDescriptor::Insert
                            && trigger.timing == TriggerTimingDescriptor::After
                            && !trigger.for_each_row
                    });
                    (
                        has_before_row,
                        has_after_row,
                        has_before_statement,
                        has_after_statement,
                    )
                };
                if has_before_insert_statement_triggers {
                    self.fire_statement_triggers(
                        *table_id,
                        TriggerEventDescriptor::Insert,
                        TriggerTimingDescriptor::Before,
                        context,
                    )?;
                }
                let direct_insert_txn_id = if use_direct_insert
                    && context.implicit_transaction
                    && context.storage_autocommit_fast_path
                    && !has_returning
                {
                    TxnId::default()
                } else {
                    context.txn_id
                };

                if use_direct_insert {
                    let prefetched_next_values_by_column =
                        self.prefetch_insert_values_next_values(rows, columns.len(), context)?;

                    let mut direct_rows = Vec::with_capacity(rows.len());
                    let has_interrupts = context.has_execution_interrupts();
                    for (row_idx, row) in rows.iter().enumerate() {
                        if has_interrupts {
                            context.check_deadline()?;
                        }
                        let mut values = Vec::with_capacity(columns.len());
                        for (column_idx, (expr, column)) in
                            row.iter().zip(columns.iter()).enumerate()
                        {
                            let prefetched_next_value = prefetched_next_values_by_column
                                .get(column_idx)
                                .and_then(|values| values.as_ref())
                                .map(|values| values[row_idx]);
                            let value = if let Some(prefetched) = prefetched_next_value {
                                Value::BigInt(prefetched)
                            } else if let TypedExprKind::Literal(literal) = &expr.kind {
                                // Fast path: literal column values
                                // (the bench-typical
                                // \`INSERT VALUES (1,'a'),(2,'b'),...\`
                                // shape) bypass the evaluator's
                                // resolver-closure dispatch and clone
                                // the literal directly.
                                literal.clone()
                            } else {
                                self.evaluate_expr(expr, context)?
                            };
                            // Fast path: when the literal already matches
                            // the column's storage type and there's no
                            // text-length modifier, the full
                            // `coerce_assigned_value` chain (cast +
                            // text-modifier check + range/multirange
                            // canonicalisation) is a series of no-ops on
                            // the dominant `INSERT VALUES (1, 'short')`
                            // shape. Skip the three function calls per
                            // value when we can prove they're irrelevant.
                            // PG does the equivalent of this at plan time
                            // via `coerce_to_target_type`.
                            let needs_coerce = column.text_type_modifier.is_some()
                                || matches!(column.data_type, aiondb_core::DataType::Text)
                                || !value_matches_column_type_exactly(&value, &column.data_type);
                            let coerced = if needs_coerce {
                                coerce_assigned_value(
                                    value,
                                    &column.data_type,
                                    column.nullable,
                                    column.text_type_modifier,
                                )?
                            } else {
                                value
                            };
                            values.push(coerced);
                        }
                        if has_not_null_columns {
                            enforce_not_null_constraints(
                                &values,
                                columns,
                                table_name_for_null_check,
                            )?;
                        }
                        // Build the row once, borrow it for the RLS check
                        // (when policies exist), then move it into the
                        // direct-insert batch - no Vec<Value> clone even
                        // when policies are present.
                        let row = Row::new(values);
                        if insert_policies.is_some() {
                            self.compat_rls_enforce_new_row(
                                &insert_table,
                                insert_policies.as_deref(),
                                &row,
                                context,
                            )?;
                        }
                        direct_rows.push(row);
                    }

                    if direct_rows.len() == 1 {
                        let row = direct_rows
                            .pop()
                            .ok_or_else(|| DbError::internal("direct insert row missing"))?;
                        let tuple_id =
                            self.storage_dml
                                .insert(direct_insert_txn_id, *table_id, row)?;
                        context.record_tuple_write(*table_id, tuple_id)?;
                        inserted = 1;
                    } else {
                        let tuple_ids = self.storage_dml.insert_batch(
                            direct_insert_txn_id,
                            *table_id,
                            direct_rows,
                        )?;
                        inserted = tuple_ids.len() as u64;
                        context.record_tuple_writes(*table_id, &tuple_ids)?;
                    }
                } else {
                    for row in rows {
                        context.check_deadline()?;
                        let mut values = row
                            .iter()
                            .zip(columns.iter())
                            .map(|(expr, column)| {
                                let value = self.evaluate_expr(expr, context)?;
                                let needs_coerce = column.text_type_modifier.is_some()
                                    || matches!(column.data_type, aiondb_core::DataType::Text)
                                    || !value_matches_column_type_exactly(
                                        &value,
                                        &column.data_type,
                                    );
                                if needs_coerce {
                                    coerce_assigned_value(
                                        value,
                                        &column.data_type,
                                        column.nullable,
                                        column.text_type_modifier,
                                    )
                                } else {
                                    Ok(value)
                                }
                            })
                            .collect::<DbResult<Vec<_>>>()?;
                        if has_not_null_columns {
                            enforce_not_null_constraints(
                                &values,
                                columns,
                                table_name_for_null_check,
                            )?;
                        }
                        // Move `values` into a transient Row for the RLS
                        // check via `mem::take`, then restore - avoids
                        // the per-row Vec<Value> clone of the previous
                        // `&Row::new(values.clone())` pattern. `values`
                        // is still mutably used below by triggers/FK,
                        // so we cannot keep the Row owned.
                        if insert_policies.is_some() {
                            let row = Row::new(std::mem::take(&mut values));
                            self.compat_rls_enforce_new_row(
                                &insert_table,
                                insert_policies.as_deref(),
                                &row,
                                context,
                            )?;
                            values = row.values;
                        }
                        if has_before_insert_row_triggers
                            && !self.fire_before_insert_triggers(*table_id, &mut values, context)?
                        {
                            continue;
                        }
                        if has_fk_constraints {
                            self.enforce_fk_on_insert(*table_id, &values, context)?;
                        }
                        if has_check_constraints {
                            self.enforce_check_on_insert(*table_id, &values, context)?;
                        }
                        if let Some(unique_state) = unique_state.as_mut() {
                            self.enforce_unique_with_state(unique_state, &values, None, context)?;
                            let returned_row = Row::new(values);
                            let tuple_id = self.storage_dml.insert(
                                context.txn_id,
                                *table_id,
                                returned_row.clone(),
                            )?;
                            context.record_tuple_write(*table_id, tuple_id)?;
                            if has_after_insert_row_triggers {
                                self.fire_after_insert_triggers(
                                    *table_id,
                                    &returned_row.values,
                                    context,
                                )?;
                            }
                            if has_returning {
                                let returning_eval_row = if needs_compat_returning_row {
                                    // The record exists only to feed compat_scan_row;
                                    // consume it so we skip the inner values clone.
                                    let record = TupleRecord {
                                        tuple_id,
                                        heap_position: tuple_id.get(),
                                        row: returned_row.clone(),
                                    };
                                    self.compat_scan_row_consume(
                                        record,
                                        include_oid_system_column,
                                        Some(*table_id),
                                    )
                                } else {
                                    returned_row.clone()
                                };
                                let returning_row = self
                                    .project_outputs_with_precomputed_ordinals(
                                        returning,
                                        returning_direct_column_ordinals.as_deref(),
                                        &returning_eval_row,
                                        context,
                                    )?;
                                self.push_returning_row_with_limits(
                                    &mut returning_rows,
                                    returning_row,
                                    context,
                                    &mut returning_result_bytes,
                                )?;
                            }
                            inserted += 1;
                            continue;
                        }
                        let Some(returned_row) = self.try_insert_with_on_conflict(
                            *table_id,
                            values,
                            on_conflict.as_ref(),
                            context,
                        )?
                        else {
                            continue;
                        };
                        if has_returning {
                            let returning_row = self.project_outputs_with_precomputed_ordinals(
                                returning,
                                returning_direct_column_ordinals.as_deref(),
                                &returned_row,
                                context,
                            )?;
                            self.push_returning_row_with_limits(
                                &mut returning_rows,
                                returning_row,
                                context,
                                &mut returning_result_bytes,
                            )?;
                        }
                        inserted += 1;
                    }
                }
                if has_after_insert_statement_triggers {
                    self.fire_statement_triggers(
                        *table_id,
                        TriggerEventDescriptor::Insert,
                        TriggerTimingDescriptor::After,
                        context,
                    )?;
                }

                if has_returning {
                    Ok(ExecutionResult::Query {
                        columns: returning.iter().map(|r| r.field.clone()).collect(),
                        rows: returning_rows,
                    })
                } else {
                    Ok(ExecutionResult::Command {
                        tag: "INSERT".to_owned(),
                        rows_affected: inserted,
                    })
                }
            }
            PhysicalPlan::InsertSelect {
                table_id,
                columns,
                assignments,
                source,
                on_conflict,
                returning,
            } => {
                context.check_deadline()?;
                let mut source_context = context.clone();
                source_context.max_result_rows = source_context
                    .max_result_rows
                    .min(internal_materialize_row_cap(context));
                source_context.collect_row_limit = None;
                source_context.collect_row_offset = 0;
                source_context.max_result_bytes =
                    source_context.max_result_bytes.max(context.max_temp_bytes);

                let source_result = self.execute(source, &source_context)?;
                let ExecutionResult::Query { rows, .. } = source_result else {
                    return Err(DbError::internal(
                        "INSERT ... SELECT source did not produce query rows",
                    ));
                };

                let has_returning = !returning.is_empty();
                let returning_direct_column_ordinals = has_returning
                    .then(|| Self::projection_column_ordinals(returning))
                    .flatten();
                let needs_compat_returning_row = has_returning
                    && returning
                        .iter()
                        .any(|output| dml_expr_references_compat_system_column(&output.expr));
                let include_oid_system_column = if needs_compat_returning_row {
                    self.compat_include_oid_system_column_for_table_id(context, *table_id)?
                } else {
                    false
                };
                let direct_insert_table = self.try_direct_insert_path(
                    *table_id,
                    on_conflict.as_ref(),
                    has_returning,
                    rows.len(),
                    context,
                )?;
                let use_direct_insert = direct_insert_table.is_some();
                let probe_table_empty_for_unique = rows.len() > 1;
                let assignment_requires_special_resolution: Vec<bool> = assignments
                    .iter()
                    .map(super::projection_plans::expr_requires_special_resolution)
                    .collect();
                let has_not_null_columns = columns.iter().any(|column| !column.nullable);
                let mut unique_state = if !use_direct_insert && on_conflict.is_none() {
                    Some(self.prepare_unique_insert_state(
                        *table_id,
                        probe_table_empty_for_unique,
                        context,
                    )?)
                } else {
                    None
                };
                let use_prelocked_insert = use_direct_insert
                    || unique_state
                        .as_ref()
                        .is_some_and(|state| state.table_was_empty());
                let mut returning_rows = if has_returning {
                    Vec::with_capacity(rows.len())
                } else {
                    Vec::new()
                };
                let mut returning_result_bytes = 0u64;
                let mut inserted = 0u64;
                if use_prelocked_insert {
                    context.record_relation_write(*table_id)?;
                    self.lock_table(context, *table_id, LockMode::RowExclusive)?;
                }
                // Reuse the descriptor `try_direct_insert_path` resolved
                // when the fast path applies; only re-fetch on the slow
                // path. This also folds the previous separate
                // `table_name_for_null_check` lookup into the same
                // `insert_table` we pull below.
                let insert_table = if let Some(table) = direct_insert_table {
                    table
                } else {
                    self.catalog_reader
                        .get_table_by_id(context.txn_id, *table_id)?
                        .ok_or_else(|| {
                            DbError::internal(format!("table {table_id:?} not found for INSERT"))
                        })?
                };
                let table_name_for_null_check: &str = insert_table.name.object_name();
                let has_fk_constraints = !insert_table.foreign_keys.is_empty();
                let has_check_constraints = !insert_table.check_constraints.is_empty();
                // See the matching note in the upper InsertValues arm:
                // `try_direct_insert_path` already proved RLS policies
                // are absent before returning Some, so we can skip the
                // second call on the direct path.
                let insert_policies = if use_direct_insert {
                    None
                } else {
                    self.compile_compat_rls_policies(
                        &insert_table,
                        CompatRlsAction::Insert,
                        context,
                    )?
                };
                let (
                    has_before_insert_row_triggers,
                    has_after_insert_row_triggers,
                    has_before_insert_statement_triggers,
                    has_after_insert_statement_triggers,
                ) = if use_direct_insert {
                    (false, false, false, false)
                } else {
                    let triggers = self.list_triggers_cached(
                        *table_id,
                        &insert_table.name.to_string(),
                        context,
                    )?;
                    let has_before_row = triggers.iter().any(|trigger| {
                        trigger.event == TriggerEventDescriptor::Insert
                            && trigger.timing == TriggerTimingDescriptor::Before
                            && trigger.for_each_row
                    });
                    let has_after_row = triggers.iter().any(|trigger| {
                        trigger.event == TriggerEventDescriptor::Insert
                            && trigger.timing == TriggerTimingDescriptor::After
                            && trigger.for_each_row
                    });
                    let has_before_statement = triggers.iter().any(|trigger| {
                        trigger.event == TriggerEventDescriptor::Insert
                            && trigger.timing == TriggerTimingDescriptor::Before
                            && !trigger.for_each_row
                    });
                    let has_after_statement = triggers.iter().any(|trigger| {
                        trigger.event == TriggerEventDescriptor::Insert
                            && trigger.timing == TriggerTimingDescriptor::After
                            && !trigger.for_each_row
                    });
                    (
                        has_before_row,
                        has_after_row,
                        has_before_statement,
                        has_after_statement,
                    )
                };
                if has_before_insert_statement_triggers {
                    self.fire_statement_triggers(
                        *table_id,
                        TriggerEventDescriptor::Insert,
                        TriggerTimingDescriptor::Before,
                        context,
                    )?;
                }
                if use_direct_insert {
                    let mut prefetched_next_values: Vec<Option<Vec<i64>>> =
                        vec![None; assignments.len()];
                    for (assignment_idx, expr) in assignments.iter().enumerate() {
                        if let TypedExprKind::NextValue { sequence_name } = &expr.kind {
                            prefetched_next_values[assignment_idx] =
                                Some(self.prefetch_sequence_next_values(
                                    sequence_name,
                                    rows.len(),
                                    context,
                                )?);
                        }
                    }
                    let mut direct_rows = Vec::with_capacity(rows.len());
                    for (row_idx, row) in rows.into_iter().enumerate() {
                        context.check_deadline()?;
                        let mut values = Vec::with_capacity(assignments.len());
                        for ((expr, column), requires_special_resolution) in assignments
                            .iter()
                            .zip(columns.iter())
                            .zip(assignment_requires_special_resolution.iter().copied())
                        {
                            let assignment_idx = values.len();
                            let value = if let Some(prefetched) =
                                prefetched_next_values[assignment_idx].as_ref()
                            {
                                Value::BigInt(prefetched[row_idx])
                            } else {
                                self.evaluate_expr_with_row_prechecked(
                                    expr,
                                    &row,
                                    context,
                                    requires_special_resolution,
                                )?
                            };
                            // Same fast path as the InsertValues arm: skip
                            // the no-op coercion chain when the value's
                            // type already matches the column's storage
                            // type and there's no text-length modifier.
                            let needs_coerce = column.text_type_modifier.is_some()
                                || matches!(column.data_type, aiondb_core::DataType::Text)
                                || !value_matches_column_type_exactly(&value, &column.data_type);
                            let coerced = if needs_coerce {
                                coerce_assigned_value(
                                    value,
                                    &column.data_type,
                                    column.nullable,
                                    column.text_type_modifier,
                                )?
                            } else {
                                value
                            };
                            values.push(coerced);
                        }
                        if has_not_null_columns {
                            enforce_not_null_constraints(
                                &values,
                                columns,
                                table_name_for_null_check,
                            )?;
                        }
                        // Build the row once, borrow it for the RLS check
                        // (when policies exist), then move it into the
                        // direct-insert batch - no Vec<Value> clone even
                        // when policies are present.
                        let row = Row::new(values);
                        if insert_policies.is_some() {
                            self.compat_rls_enforce_new_row(
                                &insert_table,
                                insert_policies.as_deref(),
                                &row,
                                context,
                            )?;
                        }
                        direct_rows.push(row);
                    }
                    let tuple_ids =
                        self.storage_dml
                            .insert_batch(context.txn_id, *table_id, direct_rows)?;
                    inserted = tuple_ids.len() as u64;
                    context.record_tuple_writes(*table_id, &tuple_ids)?;
                } else {
                    for row in rows {
                        context.check_deadline()?;
                        let mut values = assignments
                            .iter()
                            .zip(columns.iter())
                            .zip(assignment_requires_special_resolution.iter().copied())
                            .map(|((expr, column), requires_special_resolution)| {
                                let value = self.evaluate_expr_with_row_prechecked(
                                    expr,
                                    &row,
                                    context,
                                    requires_special_resolution,
                                )?;
                                let needs_coerce = column.text_type_modifier.is_some()
                                    || matches!(column.data_type, aiondb_core::DataType::Text)
                                    || !value_matches_column_type_exactly(
                                        &value,
                                        &column.data_type,
                                    );
                                if needs_coerce {
                                    coerce_assigned_value(
                                        value,
                                        &column.data_type,
                                        column.nullable,
                                        column.text_type_modifier,
                                    )
                                } else {
                                    Ok(value)
                                }
                            })
                            .collect::<DbResult<Vec<_>>>()?;
                        if has_not_null_columns {
                            enforce_not_null_constraints(
                                &values,
                                columns,
                                table_name_for_null_check,
                            )?;
                        }
                        // Move `values` into a transient Row for the RLS
                        // check via `mem::take`, then restore - avoids
                        // the per-row Vec<Value> clone of the legacy
                        // `&Row::new(values.clone())` pattern. `values`
                        // is still mutably used below by triggers/FK,
                        // so we cannot keep the Row owned.
                        if insert_policies.is_some() {
                            let row = Row::new(std::mem::take(&mut values));
                            self.compat_rls_enforce_new_row(
                                &insert_table,
                                insert_policies.as_deref(),
                                &row,
                                context,
                            )?;
                            values = row.values;
                        }
                        if has_before_insert_row_triggers
                            && !self.fire_before_insert_triggers(*table_id, &mut values, context)?
                        {
                            continue;
                        }
                        if has_fk_constraints {
                            self.enforce_fk_on_insert(*table_id, &values, context)?;
                        }
                        if has_check_constraints {
                            self.enforce_check_on_insert(*table_id, &values, context)?;
                        }
                        if let Some(unique_state) = unique_state.as_mut() {
                            self.enforce_unique_with_state(unique_state, &values, None, context)?;
                            let returned_row = Row::new(values);
                            let tuple_id = self.storage_dml.insert(
                                context.txn_id,
                                *table_id,
                                returned_row.clone(),
                            )?;
                            context.record_tuple_write(*table_id, tuple_id)?;
                            if has_after_insert_row_triggers {
                                self.fire_after_insert_triggers(
                                    *table_id,
                                    &returned_row.values,
                                    context,
                                )?;
                            }
                            if has_returning {
                                let returning_eval_row = if needs_compat_returning_row {
                                    // The record exists only to feed compat_scan_row;
                                    // consume it so we skip the inner values clone.
                                    let record = TupleRecord {
                                        tuple_id,
                                        heap_position: tuple_id.get(),
                                        row: returned_row.clone(),
                                    };
                                    self.compat_scan_row_consume(
                                        record,
                                        include_oid_system_column,
                                        Some(*table_id),
                                    )
                                } else {
                                    returned_row.clone()
                                };
                                let returning_row = self
                                    .project_outputs_with_precomputed_ordinals(
                                        returning,
                                        returning_direct_column_ordinals.as_deref(),
                                        &returning_eval_row,
                                        context,
                                    )?;
                                self.push_returning_row_with_limits(
                                    &mut returning_rows,
                                    returning_row,
                                    context,
                                    &mut returning_result_bytes,
                                )?;
                            }
                            inserted += 1;
                            continue;
                        }
                        let Some(returned_row) = self.try_insert_with_on_conflict(
                            *table_id,
                            values,
                            on_conflict.as_ref(),
                            context,
                        )?
                        else {
                            continue;
                        };
                        if has_returning {
                            let returning_row = self.project_outputs_with_precomputed_ordinals(
                                returning,
                                returning_direct_column_ordinals.as_deref(),
                                &returned_row,
                                context,
                            )?;
                            self.push_returning_row_with_limits(
                                &mut returning_rows,
                                returning_row,
                                context,
                                &mut returning_result_bytes,
                            )?;
                        }
                        inserted += 1;
                    }
                }
                if has_after_insert_statement_triggers {
                    self.fire_statement_triggers(
                        *table_id,
                        TriggerEventDescriptor::Insert,
                        TriggerTimingDescriptor::After,
                        context,
                    )?;
                }
                if has_returning {
                    Ok(ExecutionResult::Query {
                        columns: returning.iter().map(|r| r.field.clone()).collect(),
                        rows: returning_rows,
                    })
                } else {
                    Ok(ExecutionResult::Command {
                        tag: "INSERT".to_owned(),
                        rows_affected: inserted,
                    })
                }
            }
            PhysicalPlan::DeleteFromTable {
                table_id,
                filter,
                returning,
                using_table_ids,
            } => {
                let has_returning = !returning.is_empty();
                let returning_direct_column_ordinals = has_returning
                    .then(|| Self::projection_column_ordinals(returning))
                    .flatten();
                let mut returning_rows = Vec::new();
                let mut returning_result_bytes = 0u64;
                let mut deleted = 0u64;

                // Pre-materialize USING table rows when DELETE ... USING is used.
                let materialize_row_cap = internal_materialize_row_cap(context);
                let using_rows: Vec<Vec<Row>> = using_table_ids
                    .iter()
                    .map(|using_id| {
                        self.materialize_table_rows_with_limits(
                            context,
                            *using_id,
                            materialize_row_cap,
                            "DELETE ... USING",
                        )
                    })
                    .collect::<DbResult<Vec<_>>>()?;
                let filter_requires_special_resolution = filter
                    .as_ref()
                    .is_some_and(dml_expr_requires_special_resolution);
                let needs_compat_row = !using_table_ids.is_empty()
                    || filter
                        .as_ref()
                        .is_some_and(dml_expr_references_compat_system_column)
                    || returning
                        .iter()
                        .any(|output| dml_expr_references_compat_system_column(&output.expr));
                let include_oid_system_column = if needs_compat_row {
                    self.compat_include_oid_system_column_for_table_id(context, *table_id)?
                } else {
                    false
                };
                let delete_table = self
                    .catalog_reader
                    .get_table_by_id(context.txn_id, *table_id)?
                    .ok_or_else(|| {
                        DbError::internal(format!("table {table_id:?} not found for DELETE"))
                    })?;
                let delete_triggers =
                    self.list_triggers_cached(*table_id, &delete_table.name.to_string(), context)?;
                let has_before_delete_statement_triggers = delete_triggers.iter().any(|trigger| {
                    trigger.event == TriggerEventDescriptor::Delete
                        && trigger.timing == TriggerTimingDescriptor::Before
                        && !trigger.for_each_row
                });
                let has_after_delete_statement_triggers = delete_triggers.iter().any(|trigger| {
                    trigger.event == TriggerEventDescriptor::Delete
                        && trigger.timing == TriggerTimingDescriptor::After
                        && !trigger.for_each_row
                });
                if has_before_delete_statement_triggers {
                    self.fire_statement_triggers(
                        *table_id,
                        TriggerEventDescriptor::Delete,
                        TriggerTimingDescriptor::Before,
                        context,
                    )?;
                }
                let delete_policies = self.compile_compat_rls_policies(
                    &delete_table,
                    CompatRlsAction::Delete,
                    context,
                )?;
                if !using_table_ids.is_empty() && using_rows.iter().any(Vec::is_empty) {
                    if has_after_delete_statement_triggers {
                        self.fire_statement_triggers(
                            *table_id,
                            TriggerEventDescriptor::Delete,
                            TriggerTimingDescriptor::After,
                            context,
                        )?;
                    }
                    return if has_returning {
                        Ok(ExecutionResult::Query {
                            columns: returning.iter().map(|r| r.field.clone()).collect(),
                            rows: returning_rows,
                        })
                    } else {
                        Ok(ExecutionResult::Command {
                            tag: "DELETE".to_owned(),
                            rows_affected: 0,
                        })
                    };
                }

                // Fast path: DELETE WHERE col IN (lit1, lit2, ..., litN).
                // Each list element gets one index_eq scan instead of a
                // full SeqScan + per-row predicate. Mirrors the
                // SELECT-side BitmapOr access path, but inlined here
                // because the DELETE arm has its own access-path
                // selection (it doesn't go through `choose_access_path`).
                if using_table_ids.is_empty()
                    && !needs_compat_row
                    && !filter_requires_special_resolution
                    && delete_policies.is_none()
                {
                    // Detect IN-list directly, OR fall back to OR-of-eq
                    // ↔ IN runtime rewrite. Mirrors the UPDATE-side
                    // `extract_dml_or_eq_literal_filter` so DELETE
                    // benefits from the same access-path expansion
                    // when the planner did not collapse the OR chain
                    // into an IN list.
                    let in_filter_match = filter
                        .as_ref()
                        .and_then(extract_dml_in_literal_filter)
                        .or_else(|| filter.as_ref().and_then(extract_dml_or_eq_literal_filter));
                    if let Some((column_ordinal, in_literals)) = in_filter_match {
                        if let Some(filter_projection_column_ids) = self
                            .table_column_ids_for_ordinals(context, *table_id, &[column_ordinal])?
                        {
                            if let Some(filter_column_id) =
                                filter_projection_column_ids.first().copied()
                            {
                                let filter_indexes = self
                                    .catalog_reader
                                    .list_indexes(context.txn_id, *table_id)?;
                                if let Some(index_id) =
                                    best_eq_lookup_index(&filter_indexes, filter_column_id)
                                {
                                    let mode = if context.isolation
                                        == aiondb_tx::IsolationLevel::Serializable
                                    {
                                        LockMode::PredicateRead
                                    } else {
                                        LockMode::AccessShare
                                    };
                                    self.lock_table(context, *table_id, mode)?;
                                    context.record_relation_read(*table_id)?;
                                    let mut matching_tuple_ids: Vec<aiondb_core::TupleId> =
                                        Vec::new();
                                    let mut used_index_access = false;
                                    if in_literals.len() > 1 {
                                        used_index_access = true;
                                        for literal in &in_literals {
                                            match self.storage_dml.index_candidate_tuple_ids(
                                                context.txn_id,
                                                &context.snapshot,
                                                index_id,
                                                exact_lookup_key_range(literal),
                                            ) {
                                                Ok(tuple_ids) => {
                                                    matching_tuple_ids.extend(tuple_ids);
                                                }
                                                Err(error)
                                                    if error.report().sqlstate
                                                        == SqlState::FeatureNotSupported =>
                                                {
                                                    used_index_access = false;
                                                    matching_tuple_ids.clear();
                                                    break;
                                                }
                                                Err(error) => return Err(error),
                                            }
                                        }
                                    }
                                    if !used_index_access {
                                        used_index_access = true;
                                        for literal in &in_literals {
                                            match self.storage_dml.scan_index(
                                                context.txn_id,
                                                &context.snapshot,
                                                index_id,
                                                exact_lookup_key_range(literal),
                                                Some(vec![filter_column_id]),
                                            ) {
                                                Ok(mut filter_stream) => {
                                                    while let Some(record) = filter_stream.next()? {
                                                        context.check_deadline()?;
                                                        if row_matches_dml_simple_eq_literal_filter(
                                                            &record.row,
                                                            0,
                                                            literal,
                                                        )? {
                                                            matching_tuple_ids
                                                                .push(record.tuple_id);
                                                        }
                                                    }
                                                }
                                                Err(error)
                                                    if error.report().sqlstate
                                                        == SqlState::FeatureNotSupported =>
                                                {
                                                    used_index_access = false;
                                                    matching_tuple_ids.clear();
                                                    break;
                                                }
                                                Err(error) => return Err(error),
                                            }
                                        }
                                    }
                                    if !used_index_access {
                                        let in_literal_keys =
                                            build_dml_literal_key_set(&in_literals);
                                        let mut filter_stream = self.scan_table_locked(
                                            context,
                                            *table_id,
                                            Some(filter_projection_column_ids.clone()),
                                        )?;
                                        while let Some(record) = filter_stream.next()? {
                                            context.check_deadline()?;
                                            let row_matches = match &in_literal_keys {
                                                Some(keys) => row_matches_dml_literal_key_set(
                                                    &record.row,
                                                    0,
                                                    keys,
                                                ),
                                                None => in_literals.iter().try_fold(
                                                    false,
                                                    |acc, lit| {
                                                        if acc {
                                                            return Ok::<bool, DbError>(true);
                                                        }
                                                        row_matches_dml_simple_eq_literal_filter(
                                                            &record.row,
                                                            0,
                                                            lit,
                                                        )
                                                    },
                                                )?,
                                            };
                                            if row_matches {
                                                matching_tuple_ids.push(record.tuple_id);
                                            }
                                        }
                                    }
                                    // De-dup: a single tuple shouldn't be
                                    // deleted twice if the IN-list contains
                                    // duplicate values.
                                    matching_tuple_ids.sort_unstable_by_key(|tid| tid.get());
                                    matching_tuple_ids.dedup();
                                    let has_row_delete_triggers =
                                        delete_triggers.iter().any(|trigger| {
                                            trigger.event == TriggerEventDescriptor::Delete
                                                && trigger.for_each_row
                                        });
                                    let can_delete_by_tuple_id_only = !has_returning
                                        && !has_row_delete_triggers
                                        && !self.table_has_referencing_delete_foreign_keys(
                                            *table_id, context,
                                        )?;
                                    if can_delete_by_tuple_id_only {
                                        for tuple_id in matching_tuple_ids {
                                            context.check_deadline()?;
                                            self.delete_locked(context, *table_id, tuple_id, None)?;
                                            deleted += 1;
                                        }
                                        if has_after_delete_statement_triggers {
                                            self.fire_statement_triggers(
                                                *table_id,
                                                TriggerEventDescriptor::Delete,
                                                TriggerTimingDescriptor::After,
                                                context,
                                            )?;
                                        }
                                        return Ok(ExecutionResult::Command {
                                            tag: "DELETE".to_owned(),
                                            rows_affected: deleted,
                                        });
                                    }
                                    let in_literal_keys = build_dml_literal_key_set(&in_literals);
                                    for tuple_id in matching_tuple_ids {
                                        context.check_deadline()?;
                                        let Some(base_row) = self.storage_dml.fetch(
                                            context.txn_id,
                                            &context.snapshot,
                                            *table_id,
                                            tuple_id,
                                            None,
                                        )?
                                        else {
                                            continue;
                                        };
                                        let row_matches = match &in_literal_keys {
                                            Some(keys) => row_matches_dml_literal_key_set(
                                                &base_row,
                                                column_ordinal,
                                                keys,
                                            ),
                                            None => {
                                                in_literals.iter().try_fold(false, |acc, lit| {
                                                    if acc {
                                                        return Ok::<bool, DbError>(true);
                                                    }
                                                    row_matches_dml_simple_eq_literal_filter(
                                                        &base_row,
                                                        column_ordinal,
                                                        lit,
                                                    )
                                                })?
                                            }
                                        };
                                        if !row_matches {
                                            continue;
                                        }
                                        if !self.fire_before_delete_triggers(
                                            *table_id,
                                            &base_row.values,
                                            context,
                                        )? {
                                            continue;
                                        }
                                        self.enforce_fk_on_delete(
                                            *table_id,
                                            &base_row.values,
                                            context,
                                        )?;
                                        if has_returning {
                                            let returning_row = self
                                                .project_outputs_with_precomputed_ordinals(
                                                    returning,
                                                    returning_direct_column_ordinals.as_deref(),
                                                    &base_row,
                                                    context,
                                                )?;
                                            self.push_returning_row_with_limits(
                                                &mut returning_rows,
                                                returning_row,
                                                context,
                                                &mut returning_result_bytes,
                                            )?;
                                        }
                                        self.delete_locked(
                                            context,
                                            *table_id,
                                            tuple_id,
                                            Some(&base_row),
                                        )?;
                                        self.fire_after_delete_triggers(
                                            *table_id,
                                            &base_row.values,
                                            context,
                                        )?;
                                        deleted += 1;
                                    }
                                    if has_after_delete_statement_triggers {
                                        self.fire_statement_triggers(
                                            *table_id,
                                            TriggerEventDescriptor::Delete,
                                            TriggerTimingDescriptor::After,
                                            context,
                                        )?;
                                    }
                                    return if has_returning {
                                        Ok(ExecutionResult::Query {
                                            columns: returning
                                                .iter()
                                                .map(|r| r.field.clone())
                                                .collect(),
                                            rows: returning_rows,
                                        })
                                    } else {
                                        Ok(ExecutionResult::Command {
                                            tag: "DELETE".to_owned(),
                                            rows_affected: deleted,
                                        })
                                    };
                                }
                            }
                        }
                    }
                }

                // Fast path: DELETE WHERE col = literal with no USING, no
                // RLS, no compat-row, no special-resolution filter - same
                // shape as the UPDATE fast path. Use the column's index to
                // find matching tuple_ids in O(log N) instead of scanning
                // every row in the table.
                if using_table_ids.is_empty()
                    && !needs_compat_row
                    && !filter_requires_special_resolution
                    && delete_policies.is_none()
                {
                    if let Some(simple_eq_filter) = filter
                        .as_ref()
                        .and_then(extract_dml_simple_eq_literal_filter)
                    {
                        if let Some(filter_projection_column_ids) = self
                            .table_column_ids_for_ordinals(
                                context,
                                *table_id,
                                &[simple_eq_filter.column_ordinal],
                            )?
                        {
                            if let Some(filter_column_id) =
                                filter_projection_column_ids.first().copied()
                            {
                                let mode = if context.isolation
                                    == aiondb_tx::IsolationLevel::Serializable
                                {
                                    LockMode::PredicateRead
                                } else {
                                    LockMode::AccessShare
                                };
                                self.lock_table(context, *table_id, mode)?;
                                context.record_relation_read(*table_id)?;
                                let filter_indexes = self
                                    .catalog_reader
                                    .list_indexes(context.txn_id, *table_id)?;
                                if let Some(index_id) =
                                    best_eq_lookup_index(&filter_indexes, filter_column_id)
                                {
                                    let mut filter_stream = self.storage_dml.scan_index(
                                        context.txn_id,
                                        &context.snapshot,
                                        index_id,
                                        exact_lookup_key_range(&simple_eq_filter.literal),
                                        Some(vec![filter_column_id]),
                                    )?;
                                    let mut matching_tuple_ids = Vec::new();
                                    while let Some(record) = filter_stream.next()? {
                                        context.check_deadline()?;
                                        if row_matches_dml_simple_eq_literal_filter(
                                            &record.row,
                                            0,
                                            &simple_eq_filter.literal,
                                        )? {
                                            matching_tuple_ids.push(record.tuple_id);
                                        }
                                    }
                                    for tuple_id in matching_tuple_ids {
                                        context.check_deadline()?;
                                        let Some(base_row) = self.storage_dml.fetch(
                                            context.txn_id,
                                            &context.snapshot,
                                            *table_id,
                                            tuple_id,
                                            None,
                                        )?
                                        else {
                                            continue;
                                        };
                                        if !self.fire_before_delete_triggers(
                                            *table_id,
                                            &base_row.values,
                                            context,
                                        )? {
                                            continue;
                                        }
                                        self.enforce_fk_on_delete(
                                            *table_id,
                                            &base_row.values,
                                            context,
                                        )?;
                                        if has_returning {
                                            let returning_row = self
                                                .project_outputs_with_precomputed_ordinals(
                                                    returning,
                                                    returning_direct_column_ordinals.as_deref(),
                                                    &base_row,
                                                    context,
                                                )?;
                                            self.push_returning_row_with_limits(
                                                &mut returning_rows,
                                                returning_row,
                                                context,
                                                &mut returning_result_bytes,
                                            )?;
                                        }
                                        self.delete_locked(
                                            context,
                                            *table_id,
                                            tuple_id,
                                            Some(&base_row),
                                        )?;
                                        self.fire_after_delete_triggers(
                                            *table_id,
                                            &base_row.values,
                                            context,
                                        )?;
                                        deleted += 1;
                                    }

                                    if has_after_delete_statement_triggers {
                                        self.fire_statement_triggers(
                                            *table_id,
                                            TriggerEventDescriptor::Delete,
                                            TriggerTimingDescriptor::After,
                                            context,
                                        )?;
                                    }
                                    return if has_returning {
                                        Ok(ExecutionResult::Query {
                                            columns: returning
                                                .iter()
                                                .map(|r| r.field.clone())
                                                .collect(),
                                            rows: returning_rows,
                                        })
                                    } else {
                                        Ok(ExecutionResult::Command {
                                            tag: "DELETE".to_owned(),
                                            rows_affected: deleted,
                                        })
                                    };
                                }
                            }
                        }
                    }
                }

                // Hash-join DELETE … USING when an equi-join clause is
                // present (single-USING table). Mirrors the
                // UPDATE-side optimisation: per-target O(1) hash
                // lookup instead of O(M) cross-join walk over the
                // USING table.
                let using_hash_join_plan: Option<(
                    UpdateFromHashJoinPlan,
                    HashMap<ValueHashKey, Vec<usize>>,
                )> = if using_table_ids.len() == 1 && !filter_requires_special_resolution {
                    if let Some(filter_expr) = filter.as_ref() {
                        let target_col_count = if needs_compat_row {
                            self.compat_row_width_for_table_id(context, *table_id)?
                        } else {
                            delete_table.columns.len()
                        };
                        let from_col_count = using_rows
                            .first()
                            .and_then(|t| t.first())
                            .map(|r| r.values.len())
                            .unwrap_or(0);
                        if from_col_count > 0 {
                            if let Some(plan) = extract_update_from_hash_join_plan(
                                filter_expr,
                                target_col_count,
                                from_col_count,
                            ) {
                                let mut hash: HashMap<ValueHashKey, Vec<usize>> =
                                    HashMap::with_capacity(using_rows[0].len());
                                for (i, row) in using_rows[0].iter().enumerate() {
                                    if let Some(v) = row.values.get(plan.from_ordinal) {
                                        if !v.is_null() {
                                            if let Ok(key) = build_hash_key(v) {
                                                hash.entry(key).or_default().push(i);
                                            }
                                        }
                                    }
                                }
                                Some((plan, hash))
                            } else {
                                None
                            }
                        } else {
                            None
                        }
                    } else {
                        None
                    }
                } else {
                    None
                };

                let mut stream = self.scan_table_locked(context, *table_id, None)?;
                while let Some(record) = stream.next()? {
                    context.check_deadline()?;
                    if delete_policies.is_some()
                        && !self.compat_rls_allows_existing_row(
                            delete_policies.as_deref(),
                            &record.row,
                            context,
                        )?
                    {
                        continue;
                    }
                    let compat_row = needs_compat_row.then(|| {
                        self.compat_scan_row(&record, include_oid_system_column, Some(*table_id))
                    });
                    let eval_row = compat_row.as_ref().unwrap_or(&record.row);

                    if using_table_ids.is_empty() {
                        // Simple DELETE without USING.
                        if !self.evaluate_optional_predicate_prechecked(
                            filter.as_ref(),
                            eval_row,
                            context,
                            filter_requires_special_resolution,
                        )? {
                            continue;
                        }

                        if !self.fire_before_delete_triggers(
                            *table_id,
                            &record.row.values,
                            context,
                        )? {
                            continue;
                        }
                        self.enforce_fk_on_delete(*table_id, &record.row.values, context)?;
                        if has_returning {
                            let returning_row = self.project_outputs_with_precomputed_ordinals(
                                returning,
                                returning_direct_column_ordinals.as_deref(),
                                eval_row,
                                context,
                            )?;
                            self.push_returning_row_with_limits(
                                &mut returning_rows,
                                returning_row,
                                context,
                                &mut returning_result_bytes,
                            )?;
                        }
                        self.delete_locked(context, *table_id, record.tuple_id, Some(&record.row))?;
                        self.fire_after_delete_triggers(*table_id, &record.row.values, context)?;
                        deleted += 1;
                    } else if let Some((plan, hash)) = using_hash_join_plan.as_ref() {
                        // Hash-join USING path: O(1) bucket lookup per
                        // target row. Mirrors the UPDATE … FROM
                        // hash-join collapse from O(N×M) cross-product
                        // to O(N + M).
                        let target_value = eval_row.values.get(plan.target_ordinal);
                        let bucket = match target_value {
                            Some(v) if !v.is_null() => match build_hash_key(v) {
                                Ok(key) => hash.get(&key),
                                Err(_) => None,
                            },
                            _ => None,
                        };
                        if let Some(match_indices) = bucket {
                            let mut applied = false;
                            for &from_idx in match_indices {
                                if applied {
                                    break;
                                }
                                let from_row = &using_rows[0][from_idx];
                                let mut combined_vals = Vec::with_capacity(
                                    eval_row.values.len() + from_row.values.len(),
                                );
                                combined_vals.extend_from_slice(&eval_row.values);
                                combined_vals.extend_from_slice(&from_row.values);
                                let combined_row = Row::new(combined_vals);

                                if let Some(residual) = plan.residual.as_ref() {
                                    if !self.evaluate_optional_predicate_prechecked(
                                        Some(residual),
                                        &combined_row,
                                        context,
                                        false,
                                    )? {
                                        continue;
                                    }
                                }

                                if !self.fire_before_delete_triggers(
                                    *table_id,
                                    &record.row.values,
                                    context,
                                )? {
                                    applied = true;
                                    continue;
                                }
                                self.enforce_fk_on_delete(*table_id, &record.row.values, context)?;
                                if has_returning {
                                    let returning_row = self
                                        .project_outputs_with_precomputed_ordinals(
                                            returning,
                                            returning_direct_column_ordinals.as_deref(),
                                            &combined_row,
                                            context,
                                        )?;
                                    self.push_returning_row_with_limits(
                                        &mut returning_rows,
                                        returning_row,
                                        context,
                                        &mut returning_result_bytes,
                                    )?;
                                }
                                self.delete_locked(
                                    context,
                                    *table_id,
                                    record.tuple_id,
                                    Some(&record.row),
                                )?;
                                self.fire_after_delete_triggers(
                                    *table_id,
                                    &record.row.values,
                                    context,
                                )?;
                                deleted += 1;
                                applied = true;
                            }
                        }
                    } else {
                        // DELETE ... USING: cross-join target row with USING
                        // table rows and stop after the first match -
                        // returning `Ok(false)` from the closure short
                        // circuits the cross-join walk so we skip the
                        // per-combination `Row::new(scratch.clone())`
                        // allocations after the row is already gone.
                        self.for_each_from_combination(
                            &using_rows,
                            0,
                            eval_row,
                            &mut |combined_row| {
                                if !self.evaluate_optional_predicate_prechecked(
                                    filter.as_ref(),
                                    combined_row,
                                    context,
                                    filter_requires_special_resolution,
                                )? {
                                    return Ok(true);
                                }

                                if !self.fire_before_delete_triggers(
                                    *table_id,
                                    &record.row.values,
                                    context,
                                )? {
                                    return Ok(false);
                                }
                                self.enforce_fk_on_delete(*table_id, &record.row.values, context)?;
                                if has_returning {
                                    let returning_row = self
                                        .project_outputs_with_precomputed_ordinals(
                                            returning,
                                            returning_direct_column_ordinals.as_deref(),
                                            combined_row,
                                            context,
                                        )?;
                                    self.push_returning_row_with_limits(
                                        &mut returning_rows,
                                        returning_row,
                                        context,
                                        &mut returning_result_bytes,
                                    )?;
                                }
                                self.delete_locked(
                                    context,
                                    *table_id,
                                    record.tuple_id,
                                    Some(&record.row),
                                )?;
                                self.fire_after_delete_triggers(
                                    *table_id,
                                    &record.row.values,
                                    context,
                                )?;
                                deleted += 1;
                                Ok(false)
                            },
                        )?;
                    }
                }
                if has_after_delete_statement_triggers {
                    self.fire_statement_triggers(
                        *table_id,
                        TriggerEventDescriptor::Delete,
                        TriggerTimingDescriptor::After,
                        context,
                    )?;
                }
                if has_returning {
                    Ok(ExecutionResult::Query {
                        columns: returning.iter().map(|r| r.field.clone()).collect(),
                        rows: returning_rows,
                    })
                } else {
                    Ok(ExecutionResult::Command {
                        tag: "DELETE".to_owned(),
                        rows_affected: deleted,
                    })
                }
            }
            PhysicalPlan::UpdateTable {
                table_id,
                assignments,
                filter,
                returning,
                from_table_ids,
            } => {
                let updated_ordinal_key = update_assignment_ordinals_key(assignments);
                let session_has_compat_misc = aiondb_eval::with_current_session_context(|ctx| {
                    !ctx.compat_misc_attrs.is_empty()
                });
                let cache_revision = if session_has_compat_misc {
                    None
                } else {
                    Some(self.catalog_reader.catalog_revision(context.txn_id)?)
                };
                let table = if let Some(revision) = cache_revision {
                    if let Some(cached) = SIMPLE_UPDATE_PATH_CACHE.with(|cache| {
                        cache
                            .borrow()
                            .get(&(revision, *table_id, updated_ordinal_key.clone()))
                            .cloned()
                    }) {
                        cached.ok_or_else(|| {
                            DbError::internal(format!("table {table_id:?} not found for UPDATE"))
                        })?
                    } else {
                        let table = self
                            .catalog_reader
                            .get_table_by_id(context.txn_id, *table_id)?
                            .ok_or_else(|| {
                                DbError::internal(format!(
                                    "table {table_id:?} not found for UPDATE"
                                ))
                            })?;
                        SIMPLE_UPDATE_PATH_CACHE.with(|cache| {
                            let mut cache = cache.borrow_mut();
                            if cache.len() >= 256 {
                                cache.clear();
                            }
                            cache.insert(
                                (revision, *table_id, updated_ordinal_key.clone()),
                                Some(table.clone()),
                            );
                        });
                        table
                    }
                } else {
                    self.catalog_reader
                        .get_table_by_id(context.txn_id, *table_id)?
                        .ok_or_else(|| {
                            DbError::internal(format!("table {table_id:?} not found for UPDATE"))
                        })?
                };
                let has_returning = !returning.is_empty();
                let returning_direct_column_ordinals = has_returning
                    .then(|| Self::projection_column_ordinals(returning))
                    .flatten();
                let mut returning_rows = Vec::new();
                let mut returning_result_bytes = 0u64;
                let mut updated = 0u64;
                let assignment_text_type_modifiers: Vec<Option<aiondb_core::TextTypeModifier>> =
                    assignments
                        .iter()
                        .map(|assignment| {
                            table
                                .columns
                                .get(assignment.column_ordinal)
                                .and_then(|column| column.text_type_modifier)
                        })
                        .collect();
                // Identify assignments whose expression is row-independent
                // (literals, casts/arithmetic over literals, …). PostgreSQL
                // performs the equivalent at projection-init time, evaluating
                // `Const` nodes once. We evaluate lazily on the first
                // matching row so that `SET col = 1/0 WHERE never_match`
                // still succeeds without raising.
                let assignment_is_row_independent: Vec<bool> = assignments
                    .iter()
                    .map(|assignment| dml_expr_is_row_independent(&assignment.expr))
                    .collect();
                let mut cached_assignment_values: Vec<Option<Value>> =
                    vec![None; assignments.len()];
                let updated_ordinals: std::collections::HashSet<usize> =
                    updated_ordinal_key.iter().copied().collect();
                let update_triggers =
                    self.list_triggers_cached(*table_id, &table.name.to_string(), context)?;
                let has_before_update_row_triggers = update_triggers.iter().any(|trigger| {
                    (trigger.event == TriggerEventDescriptor::Update
                        || trigger
                            .extra_events
                            .contains(&TriggerEventDescriptor::Update))
                        && trigger.timing == TriggerTimingDescriptor::Before
                        && trigger.for_each_row
                });
                let has_after_update_row_triggers = update_triggers.iter().any(|trigger| {
                    (trigger.event == TriggerEventDescriptor::Update
                        || trigger
                            .extra_events
                            .contains(&TriggerEventDescriptor::Update))
                        && trigger.timing == TriggerTimingDescriptor::After
                        && trigger.for_each_row
                });
                let has_before_update_statement_triggers = update_triggers.iter().any(|trigger| {
                    trigger.event == TriggerEventDescriptor::Update
                        && trigger.timing == TriggerTimingDescriptor::Before
                        && !trigger.for_each_row
                });
                let has_after_update_statement_triggers = update_triggers.iter().any(|trigger| {
                    trigger.event == TriggerEventDescriptor::Update
                        && trigger.timing == TriggerTimingDescriptor::After
                        && !trigger.for_each_row
                });
                // Pre-filter the cached `update_triggers` list once per
                // statement into BEFORE/AFTER row-trigger slices, so the
                // per-row firing path does not redo the catalog walk +
                // sort that `lookup_triggers` would otherwise pay on
                // every modified tuple. Mirrors PostgreSQL's
                // `triggerdesc`-on-relation cache.
                let before_update_row_triggers = if has_before_update_row_triggers {
                    Self::filter_update_row_triggers(
                        &update_triggers,
                        TriggerTimingDescriptor::Before,
                    )
                } else {
                    Vec::new()
                };
                let after_update_row_triggers = if has_after_update_row_triggers {
                    Self::filter_update_row_triggers(
                        &update_triggers,
                        TriggerTimingDescriptor::After,
                    )
                } else {
                    Vec::new()
                };
                if has_before_update_statement_triggers {
                    self.fire_statement_triggers(
                        *table_id,
                        TriggerEventDescriptor::Update,
                        TriggerTimingDescriptor::Before,
                        context,
                    )?;
                }

                // Pre-materialize FROM table rows when UPDATE ... FROM is used.
                let materialize_row_cap = internal_materialize_row_cap(context);
                let from_rows: Vec<Vec<Row>> = from_table_ids
                    .iter()
                    .map(|from_id| {
                        self.materialize_table_rows_with_limits(
                            context,
                            *from_id,
                            materialize_row_cap,
                            "UPDATE ... FROM",
                        )
                    })
                    .collect::<DbResult<Vec<_>>>()?;
                let filter_requires_special_resolution = filter
                    .as_ref()
                    .is_some_and(dml_expr_requires_special_resolution);
                let assignments_require_special_resolution = assignments
                    .iter()
                    .any(|assignment| dml_expr_requires_special_resolution(&assignment.expr));
                let needs_compat_row = !from_table_ids.is_empty()
                    || filter
                        .as_ref()
                        .is_some_and(dml_expr_references_compat_system_column)
                    || assignments.iter().any(|assignment| {
                        dml_expr_references_compat_system_column(&assignment.expr)
                    });
                let include_oid_system_column = if needs_compat_row {
                    self.compat_include_oid_system_column_for_table_id(context, *table_id)?
                } else {
                    false
                };
                let has_check_constraints = !table.check_constraints.is_empty();
                let has_fk_constraints = !table.foreign_keys.is_empty();
                // PostgreSQL only revalidates NOT NULL on attributes
                // listed in the modified-cols bitmap. Precompute the
                // intersection between the UPDATE target list and the
                // relation's NOT NULL columns so the per-row check
                // shrinks to a tiny scan (typically 0..2 columns).
                let updated_not_null_ordinals =
                    updated_not_null_check_ordinals(&updated_ordinals, &table);
                // Precompile CHECK constraint expressions once per
                // UPDATE statement so the per-row enforcement avoids
                // the parser + type-checker round-trip that the
                // catalog-text-based path otherwise pays. Then prune
                // checks that touch no updated column - those cannot
                // be violated by an UPDATE that leaves their inputs
                // unchanged. PG `pg_get_constraint_attnos` gives the
                // same metadata; here we walk the compiled
                // `TypedExpr` once at statement-start time and
                // intersect with `updated_ordinals`.
                let compiled_check_constraints = if has_check_constraints {
                    let all = self.precompile_check_constraints(&table)?;
                    all.into_iter()
                        .filter(|check| {
                            let referenced = dml_expr_local_column_ordinals(&check.typed);
                            referenced.contains(&usize::MAX)
                                || referenced.iter().any(|ord| updated_ordinals.contains(ord))
                        })
                        .collect::<Vec<_>>()
                } else {
                    Vec::new()
                };
                // Re-derive the gating flag against the pruned list so
                // the per-row hot path skips the call entirely when
                // every CHECK is irrelevant to the modified columns.
                let has_check_constraints = !compiled_check_constraints.is_empty();
                let may_affect_unique_indexes =
                    self.update_may_affect_unique_indexes(&table, &updated_ordinals, context)?;
                // Pre-list the UNIQUE indexes once per UPDATE statement
                // so the per-row enforcement skips the catalog walk and
                // the redundant `storage_dml.fetch(exclude_tuple)` round
                // trip the legacy API paid on every tuple.
                let unique_indexes_for_update = if may_affect_unique_indexes {
                    self.list_unique_indexes_for_update(&table, context)?
                } else {
                    Vec::new()
                };
                let has_referencing_update_fks =
                    self.table_has_referencing_update_foreign_keys(*table_id, context)?;
                // When the UPDATE target is referenced by another table's
                // FK, hoist the catalog walk + column-name resolution
                // here once per statement instead of per row. PostgreSQL
                // caches the same metadata in `RelationGetFKeyList` /
                // trigger-desc structures during executor startup.
                let referencing_update_fk_entries = if has_referencing_update_fks {
                    self.list_referencing_update_fks(&table, context)?
                } else {
                    Vec::new()
                };
                // Pre-compile child-side FK metadata once. Skips the
                // catalog walk + name-resolution that
                // `check_fk_values_exist` would otherwise repeat per
                // tuple, mirroring PostgreSQL's `RI_QueryHashEntry`.
                let compiled_child_fk_checks = if has_fk_constraints {
                    self.compile_child_fk_checks(&table, context)?
                } else {
                    Vec::new()
                };
                let update_policies =
                    self.compile_compat_rls_policies(&table, CompatRlsAction::Update, context)?;
                if !from_table_ids.is_empty() && from_rows.iter().any(Vec::is_empty) {
                    if has_after_update_statement_triggers {
                        self.fire_statement_triggers(
                            *table_id,
                            TriggerEventDescriptor::Update,
                            TriggerTimingDescriptor::After,
                            context,
                        )?;
                    }
                    return if has_returning {
                        Ok(ExecutionResult::Query {
                            columns: returning.iter().map(|r| r.field.clone()).collect(),
                            rows: returning_rows,
                        })
                    } else {
                        Ok(ExecutionResult::Command {
                            tag: "UPDATE".to_owned(),
                            rows_affected: 0,
                        })
                    };
                }

                // Constant-folding short-circuit for the WHERE predicate.
                // PostgreSQL's planner runs `eval_const_expressions` on
                // quals before execution; when the WHERE clause folds to
                // FALSE / NULL the entire UPDATE collapses to zero rows.
                // We perform the same check at runtime so that prepared
                // statements whose bound parameters happen to fold to a
                // constant also benefit. Constant TRUE is intentionally
                // left to the per-row predicate path: the per-row check
                // for a boolean Literal is a single evaluator step,
                // shaving it would not be worth a second code path.
                if !filter_requires_special_resolution
                    && filter.as_ref().is_some_and(dml_expr_is_row_independent)
                {
                    if let Some(filter_expr) = filter.as_ref() {
                        if let Ok(folded) = self.evaluator.evaluate(filter_expr) {
                            if matches!(folded, Value::Boolean(false) | Value::Null) {
                                if has_after_update_statement_triggers {
                                    self.fire_statement_triggers(
                                        *table_id,
                                        TriggerEventDescriptor::Update,
                                        TriggerTimingDescriptor::After,
                                        context,
                                    )?;
                                }
                                return if has_returning {
                                    Ok(ExecutionResult::Query {
                                        columns: returning
                                            .iter()
                                            .map(|r| r.field.clone())
                                            .collect(),
                                        rows: returning_rows,
                                    })
                                } else {
                                    Ok(ExecutionResult::Command {
                                        tag: "UPDATE".to_owned(),
                                        rows_affected: 0,
                                    })
                                };
                            }
                        }
                    }
                }

                // Predicate-proves-empty pre-flight: ask the
                // storage backend whether its per-(ordinal, value)
                // count map can prove the WHERE clause matches NO
                // visible row. When yes, return rows_affected=0
                // WITHOUT acquiring the RowExclusive table lock,
                // walking the heap, or running the per-row
                // constraint pre-compilations below — this is the
                // O(log n) equivalent of PG's selectivity-zero
                // shortcut and lifts every "WHERE col CMP lit"
                // shape over a non-indexed column from a 5-15 ms
                // baseline (full seq scan + per-row predicate
                // eval) down to a few µs.
                if from_table_ids.is_empty()
                    && !needs_compat_row
                    && !filter_requires_special_resolution
                {
                    if let Some(filter_expr) = filter.as_ref() {
                        if self.dml_filter_proves_empty(filter_expr, &table, *table_id, context)? {
                            return Ok(ExecutionResult::Command {
                                tag: "UPDATE".to_owned(),
                                rows_affected: 0,
                            });
                        }
                    }
                }

                // Hoist the RowExclusive table lock and the
                // serializable-coordinator relation-write registration once
                // per UPDATE statement. PostgreSQL takes the same
                // RowExclusiveLock once at the start of `ExecUpdate`; both
                // calls are HashMap-keyed and idempotent within a
                // transaction, but pulling them out of the per-row
                // `update_locked` saves N-1 mutex round-trips for bulk
                // UPDATEs.
                self.lock_table(context, *table_id, LockMode::RowExclusive)?;
                context.record_relation_write(*table_id)?;

                let apply_ctx = UpdateApplyCtx {
                    table: &table,
                    table_id: *table_id,
                    filter: filter.as_ref(),
                    filter_requires_special_resolution,
                    assignments,
                    assignment_text_type_modifiers: &assignment_text_type_modifiers,
                    assignment_is_row_independent: &assignment_is_row_independent,
                    assignments_require_special_resolution,
                    update_policies: update_policies.as_deref(),
                    has_before_update_row_triggers,
                    before_update_row_triggers: &before_update_row_triggers,
                    has_after_update_row_triggers,
                    after_update_row_triggers: &after_update_row_triggers,
                    updated_not_null_ordinals: &updated_not_null_ordinals,
                    updated_target_ordinals: &updated_ordinal_key,
                    has_referencing_update_fks,
                    referencing_update_fk_entries: &referencing_update_fk_entries,
                    has_fk_constraints,
                    compiled_child_fk_checks: &compiled_child_fk_checks,
                    has_check_constraints,
                    compiled_check_constraints: &compiled_check_constraints,
                    may_affect_unique_indexes,
                    unique_indexes_for_update: &unique_indexes_for_update,
                    has_returning,
                    returning,
                    returning_direct_column_ordinals: returning_direct_column_ordinals.as_deref(),
                };

                if from_table_ids.is_empty()
                    && !needs_compat_row
                    && !filter_requires_special_resolution
                    && !assignments_require_special_resolution
                {
                    // Fast path covers two filter shapes that both
                    // resolve to "set of tuple_ids matching a list of
                    // equality literals on one column":
                    //   * `WHERE col = literal`   (single literal)
                    //   * `WHERE col IN (lit1, lit2, ..., litN)`
                    // The second shape was previously falling
                    // through to a SeqScan even when `col` was
                    // indexed. Generalise the per-literal collection
                    // into a small in-place loop and rely on the
                    // existing post-collection processing (FK / RLS
                    // / triggers / unique check) below.
                    let in_list_filter = filter.as_ref().and_then(extract_dml_in_literal_filter);
                    let simple_eq_filter = filter
                        .as_ref()
                        .and_then(extract_dml_simple_eq_literal_filter);
                    // OR-of-eq → IN: `col = a OR col = b OR ...` is
                    // semantically identical to `col IN (a, b, ...)`. The
                    // AionDB planner does not currently rewrite the OR
                    // chain to an IN list, so detect it here and feed
                    // the IN/eq fast path. Costs one tree walk per
                    // statement; pays for itself the moment any row is
                    // saved from the seq-scan fallback.
                    let or_eq_filter = filter.as_ref().and_then(extract_dml_or_eq_literal_filter);
                    let multi_eq_filter: Option<(usize, Vec<Value>)> =
                        if let Some(filter) = simple_eq_filter {
                            Some((filter.column_ordinal, vec![filter.literal]))
                        } else if let Some(in_list) = in_list_filter {
                            Some(in_list)
                        } else {
                            or_eq_filter
                        };
                    if let Some((column_ordinal, literals)) = multi_eq_filter {
                        if let Some(filter_projection_column_ids) = self
                            .table_column_ids_for_ordinals(context, *table_id, &[column_ordinal])?
                        {
                            let mut matching_tuple_ids: Vec<aiondb_core::TupleId> = Vec::new();
                            let mut used_pushdown = false;
                            if let Some(filter_column_id) =
                                filter_projection_column_ids.first().copied()
                            {
                                let mode = if context.isolation
                                    == aiondb_tx::IsolationLevel::Serializable
                                {
                                    LockMode::PredicateRead
                                } else {
                                    LockMode::AccessShare
                                };
                                self.lock_table(context, *table_id, mode)?;
                                context.record_relation_read(*table_id)?;
                                let filter_indexes = self
                                    .catalog_reader
                                    .list_indexes(context.txn_id, *table_id)?;
                                if literals.len() > 1 {
                                    if let Some(index_id) =
                                        best_eq_lookup_index(&filter_indexes, filter_column_id)
                                    {
                                        used_pushdown = true;
                                        for literal in &literals {
                                            match self.storage_dml.index_candidate_tuple_ids(
                                                context.txn_id,
                                                &context.snapshot,
                                                index_id,
                                                exact_lookup_key_range(literal),
                                            ) {
                                                Ok(tuple_ids) => {
                                                    matching_tuple_ids.extend(tuple_ids);
                                                }
                                                Err(error)
                                                    if error.report().sqlstate
                                                        == SqlState::FeatureNotSupported =>
                                                {
                                                    used_pushdown = false;
                                                    matching_tuple_ids.clear();
                                                    break;
                                                }
                                                Err(error) => return Err(error),
                                            }
                                        }
                                    }
                                }
                                if !used_pushdown {
                                    if let Some(index_id) =
                                        best_eq_lookup_index(&filter_indexes, filter_column_id)
                                    {
                                        used_pushdown = true;
                                        for literal in &literals {
                                            match self.storage_dml.index_candidate_tuple_ids(
                                                context.txn_id,
                                                &context.snapshot,
                                                index_id,
                                                exact_lookup_key_range(literal),
                                            ) {
                                                Ok(tuple_ids) => {
                                                    matching_tuple_ids.extend(tuple_ids);
                                                }
                                                Err(error)
                                                    if error.report().sqlstate
                                                        == SqlState::FeatureNotSupported =>
                                                {
                                                    let mut filter_stream =
                                                        self.storage_dml.scan_index(
                                                            context.txn_id,
                                                            &context.snapshot,
                                                            index_id,
                                                            exact_lookup_key_range(literal),
                                                            Some(vec![filter_column_id]),
                                                        )?;
                                                    let has_interrupts =
                                                        context.has_execution_interrupts();
                                                    while let Some(record) = filter_stream.next()? {
                                                        if has_interrupts {
                                                            context.check_deadline()?;
                                                        }
                                                        if row_matches_dml_simple_eq_literal_filter(
                                                            &record.row,
                                                            0,
                                                            literal,
                                                        )? {
                                                            matching_tuple_ids
                                                                .push(record.tuple_id);
                                                        }
                                                    }
                                                }
                                                Err(error) => return Err(error),
                                            }
                                        }
                                    } else {
                                        let mut all_supported = true;
                                        for literal in &literals {
                                            match self.storage_dml.scan_table_eq_filter(
                                                context.txn_id,
                                                &context.snapshot,
                                                *table_id,
                                                filter_column_id,
                                                literal,
                                                Some(vec![filter_column_id]),
                                            ) {
                                                Ok(mut filter_stream) => {
                                                    let has_interrupts =
                                                        context.has_execution_interrupts();
                                                    while let Some(record) = filter_stream.next()? {
                                                        if has_interrupts {
                                                            context.check_deadline()?;
                                                        }
                                                        matching_tuple_ids.push(record.tuple_id);
                                                    }
                                                }
                                                Err(error)
                                                    if error.report().sqlstate
                                                        == SqlState::FeatureNotSupported =>
                                                {
                                                    all_supported = false;
                                                    break;
                                                }
                                                Err(error) => return Err(error),
                                            }
                                        }
                                        if all_supported {
                                            used_pushdown = true;
                                        } else {
                                            // Drop any partially-collected tids
                                            // and fall through to the full scan
                                            // below for consistent behaviour.
                                            matching_tuple_ids.clear();
                                        }
                                    }
                                }
                            }

                            // Probe: if matched is 0 we should be near-no-op
                            // from here. The big jump in elapsed up to "after
                            // per-tuple update loop" must therefore live in
                            // one of the steps in the next ~200 lines. Bisect.
                            if !used_pushdown {
                                let literal_keys = build_dml_literal_key_set(&literals);
                                let mut filter_stream =
                                    self.scan_table_locked(context, *table_id, None)?;
                                let has_interrupts = context.has_execution_interrupts();
                                while let Some(record) = filter_stream.next()? {
                                    if has_interrupts {
                                        context.check_deadline()?;
                                    }
                                    let row_matches = match &literal_keys {
                                        Some(keys) => row_matches_dml_literal_key_set(
                                            &record.row,
                                            column_ordinal,
                                            keys,
                                        ),
                                        None => literals.iter().try_fold(false, |acc, lit| {
                                            if acc {
                                                return Ok::<bool, DbError>(true);
                                            }
                                            row_matches_dml_simple_eq_literal_filter(
                                                &record.row,
                                                column_ordinal,
                                                lit,
                                            )
                                        })?,
                                    };
                                    if row_matches {
                                        matching_tuple_ids.push(record.tuple_id);
                                    }
                                }
                            }
                            let mut matching_base_rows: HashMap<aiondb_core::TupleId, Row> =
                                HashMap::new();
                            if used_pushdown && literals.len() > 1 && !matching_tuple_ids.is_empty()
                            {
                                let literal_keys = build_dml_literal_key_set(&literals);
                                let mut live_matching_tuple_ids =
                                    Vec::with_capacity(matching_tuple_ids.len());
                                let has_interrupts = context.has_execution_interrupts();
                                for tuple_id in matching_tuple_ids {
                                    if has_interrupts {
                                        context.check_deadline()?;
                                    }
                                    let Some(base_row) = self.storage_dml.fetch(
                                        context.txn_id,
                                        &context.snapshot,
                                        *table_id,
                                        tuple_id,
                                        None,
                                    )?
                                    else {
                                        continue;
                                    };
                                    let row_matches = match &literal_keys {
                                        Some(keys) => row_matches_dml_literal_key_set(
                                            &base_row,
                                            column_ordinal,
                                            keys,
                                        ),
                                        None => literals.iter().try_fold(false, |acc, lit| {
                                            if acc {
                                                return Ok::<bool, DbError>(true);
                                            }
                                            row_matches_dml_simple_eq_literal_filter(
                                                &base_row,
                                                column_ordinal,
                                                lit,
                                            )
                                        })?,
                                    };
                                    if row_matches {
                                        matching_base_rows.insert(tuple_id, base_row);
                                        live_matching_tuple_ids.push(tuple_id);
                                    }
                                }
                                matching_tuple_ids = live_matching_tuple_ids;
                            }
                            // NOTE: the previous implementation re-ran a
                            // full table seq-scan whenever pushdown
                            // returned 0 matching tuple_ids, as a
                            // belt-and-suspenders safety net.  But both
                            // pushdown paths above (index-based via
                            // `index_candidate_tuple_ids` and
                            // heap-based via `scan_table_eq_filter`)
                            // walk the snapshot-visible data with the
                            // same MVCC predicate the seq-scan would —
                            // an empty result is therefore a legitimate
                            // "no match" answer, not a sign of pushdown
                            // failure.  Re-scanning the whole table to
                            // re-confirm `0 == 0` cost ~5 ms per UPDATE
                            // on a 20k-row table and DEFEATED the
                            // optimisation for the noop case (the
                            // `point_nonindexed_change` /
                            // `range_nonindexed_500` /
                            // `composite_nonindexed_50` /
                            // `mixed_filter_seq` benchmarks all
                            // observed it as ~6 ms baseline regardless
                            // of how cheap the actual filter was).
                            // Trust the pushdown result.
                            // De-dup so a single tuple is never updated
                            // twice if the IN-list contains duplicate
                            // literals (or the same row appears via
                            // different literals - shouldn't happen for
                            // strict equality but stays safe).
                            if literals.len() > 1 {
                                matching_tuple_ids.sort_unstable_by_key(|tid| tid.get());
                                matching_tuple_ids.dedup();
                            }
                            let case_lookup_assignments: Vec<Option<DmlCaseLookupAssignment>> =
                                assignments
                                    .iter()
                                    .map(|assignment| {
                                        extract_dml_case_lookup_assignment(
                                            &assignment.expr,
                                            column_ordinal,
                                            assignment.column_ordinal,
                                        )
                                    })
                                    .collect();

                            let has_interrupts = context.has_execution_interrupts();
                            for tuple_id in matching_tuple_ids {
                                if has_interrupts {
                                    context.check_deadline()?;
                                }
                                let base_row =
                                    if let Some(base_row) = matching_base_rows.remove(&tuple_id) {
                                        base_row
                                    } else {
                                        let Some(base_row) = self.storage_dml.fetch(
                                            context.txn_id,
                                            &context.snapshot,
                                            *table_id,
                                            tuple_id,
                                            None,
                                        )?
                                        else {
                                            continue;
                                        };
                                        base_row
                                    };
                                let case_row_key = base_row
                                    .values
                                    .get(column_ordinal)
                                    .and_then(|value| build_hash_key(value).ok());
                                if update_policies.is_some()
                                    && !self.compat_rls_allows_existing_row(
                                        update_policies.as_deref(),
                                        &base_row,
                                        context,
                                    )?
                                {
                                    continue;
                                }
                                let mut values = base_row.values.clone();
                                for (idx, ((assignment, text_type_modifier), case_lookup)) in
                                    assignments
                                        .iter()
                                        .zip(assignment_text_type_modifiers.iter())
                                        .zip(case_lookup_assignments.iter())
                                        .enumerate()
                                {
                                    let value = if assignment_is_row_independent[idx] {
                                        if let Some(cached) = &cached_assignment_values[idx] {
                                            cached.clone()
                                        } else {
                                            let raw = self.evaluator.evaluate(&assignment.expr)?;
                                            let coerced = coerce_assigned_value(
                                                raw,
                                                &assignment.data_type,
                                                assignment.nullable,
                                                *text_type_modifier,
                                            )?;
                                            cached_assignment_values[idx] = Some(coerced.clone());
                                            coerced
                                        }
                                    } else {
                                        let raw = if let Some(case_lookup) = case_lookup {
                                            if let Some(value) = case_row_key
                                                .as_ref()
                                                .and_then(|key| case_lookup.value_for_key(key))
                                            {
                                                value
                                            } else {
                                                self.evaluate_expr_with_row_prechecked(
                                                    &assignment.expr,
                                                    &base_row,
                                                    context,
                                                    false,
                                                )?
                                            }
                                        } else {
                                            self.evaluate_expr_with_row_prechecked(
                                                &assignment.expr,
                                                &base_row,
                                                context,
                                                false,
                                            )?
                                        };
                                        coerce_assigned_value(
                                            raw,
                                            &assignment.data_type,
                                            assignment.nullable,
                                            *text_type_modifier,
                                        )?
                                    };
                                    if assignment.column_ordinal < values.len() {
                                        values[assignment.column_ordinal] = value;
                                    }
                                }
                                if self.apply_update_to_tuple(
                                    &apply_ctx,
                                    &mut cached_assignment_values,
                                    &base_row,
                                    tuple_id,
                                    values,
                                    None,
                                    &mut returning_rows,
                                    &mut returning_result_bytes,
                                    context,
                                )? {
                                    updated += 1;
                                }
                            }

                            if has_after_update_statement_triggers {
                                self.fire_statement_triggers(
                                    *table_id,
                                    TriggerEventDescriptor::Update,
                                    TriggerTimingDescriptor::After,
                                    context,
                                )?;
                            }
                            return if has_returning {
                                Ok(ExecutionResult::Query {
                                    columns: returning.iter().map(|r| r.field.clone()).collect(),
                                    rows: returning_rows,
                                })
                            } else {
                                Ok(ExecutionResult::Command {
                                    tag: "UPDATE".to_owned(),
                                    rows_affected: updated,
                                })
                            };
                        }
                    }

                    // Composite-eq access path: `col1 = lit1 AND col2 =
                    // lit2 [AND ...]` where the bound columns form a
                    // leading prefix of a multi-column btree index.
                    // PostgreSQL's `match_clause_to_indexable_clause`
                    // collapses this to a single point lookup; we do
                    // the same. Re-validation of the predicate on the
                    // base row is unnecessary for full-prefix point
                    // lookups (btree is exact), but we still re-check
                    // the residual eq clauses that did not enter the
                    // index key in case the live row drifted.
                    if let Some(eq_clauses) = filter
                        .as_ref()
                        .and_then(extract_dml_composite_eq_literal_filter)
                    {
                        let column_ordinals: Vec<usize> =
                            eq_clauses.iter().map(|(o, _)| *o).collect();
                        if let Some(filter_projection_column_ids) = self
                            .table_column_ids_for_ordinals(context, *table_id, &column_ordinals)?
                        {
                            let mut clauses_by_column_id: HashMap<ColumnId, Value> =
                                HashMap::with_capacity(eq_clauses.len());
                            for ((_, literal), column_id) in
                                eq_clauses.iter().zip(filter_projection_column_ids.iter())
                            {
                                clauses_by_column_id.insert(*column_id, literal.clone());
                            }
                            let mode =
                                if context.isolation == aiondb_tx::IsolationLevel::Serializable {
                                    LockMode::PredicateRead
                                } else {
                                    LockMode::AccessShare
                                };
                            self.lock_table(context, *table_id, mode)?;
                            context.record_relation_read(*table_id)?;
                            let filter_indexes = self
                                .catalog_reader
                                .list_indexes(context.txn_id, *table_id)?;
                            if let Some((index_id, prefix_values)) = best_composite_eq_lookup_index(
                                &filter_indexes,
                                &clauses_by_column_id,
                            ) {
                                // Per-column ordinal → literal lookup
                                // for the residual re-check (only the
                                // clauses whose column did not enter
                                // the matched index prefix).
                                let prefix_column_ids: std::collections::HashSet<ColumnId> = {
                                    let chosen = filter_indexes
                                        .iter()
                                        .find(|i| i.index_id == index_id)
                                        .map(|i| {
                                            i.key_columns
                                                .iter()
                                                .take(prefix_values.len())
                                                .map(|c| c.column_id)
                                                .collect()
                                        })
                                        .unwrap_or_default();
                                    chosen
                                };
                                let residual_clauses: Vec<(usize, Value)> = eq_clauses
                                    .iter()
                                    .zip(filter_projection_column_ids.iter())
                                    .filter(|&((_ordinal, _literal), column_id)| {
                                        !prefix_column_ids.contains(column_id)
                                    })
                                    .map(|((ordinal, literal), _column_id)| {
                                        (*ordinal, literal.clone())
                                    })
                                    .collect();
                                let key_range = composite_lookup_key_range(&prefix_values);
                                match self.storage_dml.scan_index(
                                    context.txn_id,
                                    &context.snapshot,
                                    index_id,
                                    key_range,
                                    None,
                                ) {
                                    Ok(mut filter_stream) => {
                                        let has_interrupts = context.has_execution_interrupts();
                                        while let Some(record) = filter_stream.next()? {
                                            if has_interrupts {
                                                context.check_deadline()?;
                                            }
                                            let mut residual_ok = true;
                                            for (ordinal, literal) in &residual_clauses {
                                                if !row_matches_dml_simple_eq_literal_filter(
                                                    &record.row,
                                                    *ordinal,
                                                    literal,
                                                )? {
                                                    residual_ok = false;
                                                    break;
                                                }
                                            }
                                            if !residual_ok {
                                                continue;
                                            }
                                            if update_policies.is_some()
                                                && !self.compat_rls_allows_existing_row(
                                                    update_policies.as_deref(),
                                                    &record.row,
                                                    context,
                                                )?
                                            {
                                                continue;
                                            }
                                            let tuple_id = record.tuple_id;
                                            let mut values = record.row.values.clone();
                                            for (idx, (assignment, text_type_modifier)) in
                                                assignments
                                                    .iter()
                                                    .zip(assignment_text_type_modifiers.iter())
                                                    .enumerate()
                                            {
                                                let value = if assignment_is_row_independent[idx] {
                                                    if let Some(cached) =
                                                        &cached_assignment_values[idx]
                                                    {
                                                        cached.clone()
                                                    } else {
                                                        let raw = self
                                                            .evaluator
                                                            .evaluate(&assignment.expr)?;
                                                        let coerced = coerce_assigned_value(
                                                            raw,
                                                            &assignment.data_type,
                                                            assignment.nullable,
                                                            *text_type_modifier,
                                                        )?;
                                                        cached_assignment_values[idx] =
                                                            Some(coerced.clone());
                                                        coerced
                                                    }
                                                } else {
                                                    let raw = self
                                                        .evaluate_expr_with_row_prechecked(
                                                            &assignment.expr,
                                                            &record.row,
                                                            context,
                                                            false,
                                                        )?;
                                                    coerce_assigned_value(
                                                        raw,
                                                        &assignment.data_type,
                                                        assignment.nullable,
                                                        *text_type_modifier,
                                                    )?
                                                };
                                                if assignment.column_ordinal < values.len() {
                                                    values[assignment.column_ordinal] = value;
                                                }
                                            }
                                            if self.apply_update_to_tuple(
                                                &apply_ctx,
                                                &mut cached_assignment_values,
                                                &record.row,
                                                tuple_id,
                                                values,
                                                None,
                                                &mut returning_rows,
                                                &mut returning_result_bytes,
                                                context,
                                            )? {
                                                updated += 1;
                                            }
                                        }
                                        if has_after_update_statement_triggers {
                                            self.fire_statement_triggers(
                                                *table_id,
                                                TriggerEventDescriptor::Update,
                                                TriggerTimingDescriptor::After,
                                                context,
                                            )?;
                                        }
                                        return if has_returning {
                                            Ok(ExecutionResult::Query {
                                                columns: returning
                                                    .iter()
                                                    .map(|r| r.field.clone())
                                                    .collect(),
                                                rows: returning_rows,
                                            })
                                        } else {
                                            Ok(ExecutionResult::Command {
                                                tag: "UPDATE".to_owned(),
                                                rows_affected: updated,
                                            })
                                        };
                                    }
                                    Err(error)
                                        if error.report().sqlstate
                                            == SqlState::FeatureNotSupported =>
                                    {
                                        // Fall through to range / seq scan.
                                    }
                                    Err(error) => return Err(error),
                                }
                            }
                        }
                    }

                    // Range / BETWEEN access path. Mirrors PostgreSQL's
                    // btree-driven index scan selection for `col OP lit`,
                    // `col >= lo AND col <= hi`, and `col BETWEEN lo AND
                    // hi`. We only use it when an index whose first key
                    // column matches the filter column exists - exactly
                    // the same shape `match_clause_to_indexable_clause`
                    // accepts on the planner side. The shared
                    // `apply_update_to_tuple` helper handles all
                    // post-collection work, so this block stays focused
                    // on the access path itself.
                    if let Some(range_bound) =
                        filter.as_ref().and_then(extract_dml_range_literal_filter)
                    {
                        if let Some(filter_projection_column_ids) = self
                            .table_column_ids_for_ordinals(
                                context,
                                *table_id,
                                &[range_bound.column_ordinal],
                            )?
                        {
                            if let Some(filter_column_id) =
                                filter_projection_column_ids.first().copied()
                            {
                                let mode = if context.isolation
                                    == aiondb_tx::IsolationLevel::Serializable
                                {
                                    LockMode::PredicateRead
                                } else {
                                    LockMode::AccessShare
                                };
                                self.lock_table(context, *table_id, mode)?;
                                context.record_relation_read(*table_id)?;
                                let filter_indexes = self
                                    .catalog_reader
                                    .list_indexes(context.txn_id, *table_id)?;
                                if let Some(index_id) =
                                    best_eq_lookup_index(&filter_indexes, filter_column_id)
                                {
                                    let key_range = range_bound.to_key_range();
                                    match self.storage_dml.scan_index(
                                        context.txn_id,
                                        &context.snapshot,
                                        index_id,
                                        key_range,
                                        None,
                                    ) {
                                        Ok(mut filter_stream) => {
                                            let has_interrupts = context.has_execution_interrupts();
                                            while let Some(record) = filter_stream.next()? {
                                                if has_interrupts {
                                                    context.check_deadline()?;
                                                }
                                                // Re-validate the bound on the
                                                // base row: the index scan is
                                                // MVCC-filtered to our snapshot,
                                                // but for non-unique indexes
                                                // (and after concurrent
                                                // updates) the live row's value
                                                // for the column may have moved
                                                // outside the requested range.
                                                if !row_matches_dml_range_bound(
                                                    &record.row,
                                                    &range_bound,
                                                )? {
                                                    continue;
                                                }
                                                if update_policies.is_some()
                                                    && !self.compat_rls_allows_existing_row(
                                                        update_policies.as_deref(),
                                                        &record.row,
                                                        context,
                                                    )?
                                                {
                                                    continue;
                                                }
                                                let tuple_id = record.tuple_id;
                                                let mut values = record.row.values.clone();
                                                for (idx, (assignment, text_type_modifier)) in
                                                    assignments
                                                        .iter()
                                                        .zip(assignment_text_type_modifiers.iter())
                                                        .enumerate()
                                                {
                                                    let value = if assignment_is_row_independent
                                                        [idx]
                                                    {
                                                        if let Some(cached) =
                                                            &cached_assignment_values[idx]
                                                        {
                                                            cached.clone()
                                                        } else {
                                                            let raw = self
                                                                .evaluator
                                                                .evaluate(&assignment.expr)?;
                                                            let coerced = coerce_assigned_value(
                                                                raw,
                                                                &assignment.data_type,
                                                                assignment.nullable,
                                                                *text_type_modifier,
                                                            )?;
                                                            cached_assignment_values[idx] =
                                                                Some(coerced.clone());
                                                            coerced
                                                        }
                                                    } else {
                                                        let raw = self
                                                            .evaluate_expr_with_row_prechecked(
                                                                &assignment.expr,
                                                                &record.row,
                                                                context,
                                                                false,
                                                            )?;
                                                        coerce_assigned_value(
                                                            raw,
                                                            &assignment.data_type,
                                                            assignment.nullable,
                                                            *text_type_modifier,
                                                        )?
                                                    };
                                                    if assignment.column_ordinal < values.len() {
                                                        values[assignment.column_ordinal] = value;
                                                    }
                                                }
                                                if self.apply_update_to_tuple(
                                                    &apply_ctx,
                                                    &mut cached_assignment_values,
                                                    &record.row,
                                                    tuple_id,
                                                    values,
                                                    None,
                                                    &mut returning_rows,
                                                    &mut returning_result_bytes,
                                                    context,
                                                )? {
                                                    updated += 1;
                                                }
                                            }
                                            if has_after_update_statement_triggers {
                                                self.fire_statement_triggers(
                                                    *table_id,
                                                    TriggerEventDescriptor::Update,
                                                    TriggerTimingDescriptor::After,
                                                    context,
                                                )?;
                                            }
                                            return if has_returning {
                                                Ok(ExecutionResult::Query {
                                                    columns: returning
                                                        .iter()
                                                        .map(|r| r.field.clone())
                                                        .collect(),
                                                    rows: returning_rows,
                                                })
                                            } else {
                                                Ok(ExecutionResult::Command {
                                                    tag: "UPDATE".to_owned(),
                                                    rows_affected: updated,
                                                })
                                            };
                                        }
                                        Err(error)
                                            if error.report().sqlstate
                                                == SqlState::FeatureNotSupported =>
                                        {
                                            // Fall through to the seq scan
                                            // path - the underlying index
                                            // type does not support range
                                            // scans on this column.
                                        }
                                        Err(error) => return Err(error),
                                    }
                                }
                            }
                        }
                    }
                }

                // AND-of-bounds storage pushdown: when the WHERE
                // clause decomposes cleanly into per-column
                // `(col, lower, upper)` bounds (single eq, single
                // range, AND-of-eqs, AND-of-ranges), push the entire
                // predicate set into
                // `scan_table_multi_range_filter`. The storage
                // backend evaluates every conjunct inline at decode
                // time, materialising only matching rows — vs the
                // generic `scan_table_locked` + per-row
                // `evaluate_optional_predicate_prechecked` path that
                // pays a full ExpressionEvaluator dispatch for every
                // tuple. Mirrors PG's `qpqual` integrated into the
                // `seqgetnext` loop. Lifts the
                // `range_nonindexed_500` / `composite_nonindexed_50`
                // / `mixed_filter_seq` benches without touching the
                // existing index-based fast paths above.
                // AND-of-(IN-list, residual) shortcut: when the
                // WHERE clause is `col IN (lit1, …, litN) AND rest`
                // (where `rest` is anything we can't push — sub-
                // queries, function calls, …), use
                // `scan_table_eq_filter` with the literal list to
                // shrink the candidate set first, then evaluate
                // `rest` per candidate. This is the only pushdown
                // path that helps the `exists_semijoin` /
                // `in_subquery_with_filter` shapes the bench
                // exercises, and it's NOT subsumed by
                // `dml_extract_pushdown_bounds` because IN-list
                // is OR-of-eqs which `scan_table_multi_range_filter`
                // (AND-of-bounds) cannot express.
                if from_table_ids.is_empty()
                    && !needs_compat_row
                    && !assignments_require_special_resolution
                {
                    if let Some(filter_expr) = filter.as_ref() {
                        if let Some((in_col_id, in_literals, in_residual)) =
                            self.dml_extract_in_list_with_residual(filter_expr, &table)
                        {
                            let in_residual_requires_special = in_residual
                                .as_ref()
                                .is_some_and(dml_expr_requires_special_resolution);
                            let mode =
                                if context.isolation == aiondb_tx::IsolationLevel::Serializable {
                                    LockMode::PredicateRead
                                } else {
                                    LockMode::AccessShare
                                };
                            self.lock_table(context, *table_id, mode)?;
                            context.record_relation_read(*table_id)?;
                            match self.storage_dml.scan_table_in_filter(
                                context.txn_id,
                                &context.snapshot,
                                *table_id,
                                in_col_id,
                                &in_literals,
                                None,
                            ) {
                                Ok(mut filter_stream) => {
                                    let has_interrupts = context.has_execution_interrupts();
                                    while let Some(record) = filter_stream.next()? {
                                        if has_interrupts {
                                            context.check_deadline()?;
                                        }
                                        if let Some(residual_expr) = in_residual.as_ref() {
                                            if !self.evaluate_optional_predicate_prechecked(
                                                Some(residual_expr),
                                                &record.row,
                                                context,
                                                in_residual_requires_special,
                                            )? {
                                                continue;
                                            }
                                        }
                                        if update_policies.is_some()
                                            && !self.compat_rls_allows_existing_row(
                                                update_policies.as_deref(),
                                                &record.row,
                                                context,
                                            )?
                                        {
                                            continue;
                                        }
                                        let tuple_id = record.tuple_id;
                                        let mut values = record.row.values.clone();
                                        for (idx, (assignment, text_type_modifier)) in assignments
                                            .iter()
                                            .zip(assignment_text_type_modifiers.iter())
                                            .enumerate()
                                        {
                                            let value = if assignment_is_row_independent[idx] {
                                                if let Some(cached) = &cached_assignment_values[idx]
                                                {
                                                    cached.clone()
                                                } else {
                                                    let raw = self
                                                        .evaluator
                                                        .evaluate(&assignment.expr)?;
                                                    let coerced = coerce_assigned_value(
                                                        raw,
                                                        &assignment.data_type,
                                                        assignment.nullable,
                                                        *text_type_modifier,
                                                    )?;
                                                    cached_assignment_values[idx] =
                                                        Some(coerced.clone());
                                                    coerced
                                                }
                                            } else {
                                                let raw = self.evaluate_expr_with_row_prechecked(
                                                    &assignment.expr,
                                                    &record.row,
                                                    context,
                                                    false,
                                                )?;
                                                coerce_assigned_value(
                                                    raw,
                                                    &assignment.data_type,
                                                    assignment.nullable,
                                                    *text_type_modifier,
                                                )?
                                            };
                                            if assignment.column_ordinal < values.len() {
                                                values[assignment.column_ordinal] = value;
                                            }
                                        }
                                        if self.apply_update_to_tuple(
                                            &apply_ctx,
                                            &mut cached_assignment_values,
                                            &record.row,
                                            tuple_id,
                                            values,
                                            None,
                                            &mut returning_rows,
                                            &mut returning_result_bytes,
                                            context,
                                        )? {
                                            updated += 1;
                                        }
                                    }
                                    if has_after_update_statement_triggers {
                                        self.fire_statement_triggers(
                                            *table_id,
                                            TriggerEventDescriptor::Update,
                                            TriggerTimingDescriptor::After,
                                            context,
                                        )?;
                                    }
                                    return if has_returning {
                                        Ok(ExecutionResult::Query {
                                            columns: returning
                                                .iter()
                                                .map(|r| r.field.clone())
                                                .collect(),
                                            rows: returning_rows,
                                        })
                                    } else {
                                        Ok(ExecutionResult::Command {
                                            tag: "UPDATE".to_owned(),
                                            rows_affected: updated,
                                        })
                                    };
                                }
                                Err(error)
                                    if error.report().sqlstate == SqlState::FeatureNotSupported => {
                                }
                                Err(error) => return Err(error),
                            }
                        }
                    }
                }

                if from_table_ids.is_empty()
                    && !needs_compat_row
                    && !assignments_require_special_resolution
                {
                    if let Some(filter_expr) = filter.as_ref() {
                        if let Some((eq_clauses, residual)) =
                            self.dml_extract_eq_conjuncts_with_residual(filter_expr, &table)
                        {
                            if eq_clauses.len() >= 2 {
                                let eq_ordinals: Vec<usize> =
                                    eq_clauses.iter().map(|(ordinal, _)| *ordinal).collect();
                                if let Some(filter_projection_column_ids) = self
                                    .table_column_ids_for_ordinals(
                                        context,
                                        *table_id,
                                        &eq_ordinals,
                                    )?
                                {
                                    let filter_indexes = self
                                        .catalog_reader
                                        .list_indexes(context.txn_id, *table_id)?;
                                    let mut seen_indexes = std::collections::HashSet::new();
                                    let mut child_paths = Vec::new();
                                    for ((_, literal), column_id) in
                                        eq_clauses.iter().zip(filter_projection_column_ids.iter())
                                    {
                                        if let Some(index_id) =
                                            best_eq_lookup_index(&filter_indexes, *column_id)
                                        {
                                            if seen_indexes.insert(index_id) {
                                                child_paths.push(ScanAccessPath::IndexEq {
                                                    index_id,
                                                    value: literal.clone(),
                                                });
                                            }
                                        }
                                    }
                                    if child_paths.len() >= 2 {
                                        let residual_requires_special = residual
                                            .as_ref()
                                            .is_some_and(dml_expr_requires_special_resolution);
                                        let mut filter_stream = self.execute_bitmap_scan(
                                            context,
                                            *table_id,
                                            &child_paths,
                                            None,
                                            true,
                                        )?;
                                        let has_interrupts = context.has_execution_interrupts();
                                        while let Some(record) = filter_stream.next()? {
                                            if has_interrupts {
                                                context.check_deadline()?;
                                            }
                                            let mut eq_matches = true;
                                            for (ordinal, literal) in &eq_clauses {
                                                if !row_matches_dml_simple_eq_literal_filter(
                                                    &record.row,
                                                    *ordinal,
                                                    literal,
                                                )? {
                                                    eq_matches = false;
                                                    break;
                                                }
                                            }
                                            if !eq_matches {
                                                continue;
                                            }
                                            if let Some(residual_expr) = residual.as_ref() {
                                                if !self.evaluate_optional_predicate_prechecked(
                                                    Some(residual_expr),
                                                    &record.row,
                                                    context,
                                                    residual_requires_special,
                                                )? {
                                                    continue;
                                                }
                                            }
                                            if update_policies.is_some()
                                                && !self.compat_rls_allows_existing_row(
                                                    update_policies.as_deref(),
                                                    &record.row,
                                                    context,
                                                )?
                                            {
                                                continue;
                                            }
                                            let tuple_id = record.tuple_id;
                                            let mut values = record.row.values.clone();
                                            for (idx, (assignment, text_type_modifier)) in
                                                assignments
                                                    .iter()
                                                    .zip(assignment_text_type_modifiers.iter())
                                                    .enumerate()
                                            {
                                                let value = if assignment_is_row_independent[idx] {
                                                    if let Some(cached) =
                                                        &cached_assignment_values[idx]
                                                    {
                                                        cached.clone()
                                                    } else {
                                                        let raw = self
                                                            .evaluator
                                                            .evaluate(&assignment.expr)?;
                                                        let coerced = coerce_assigned_value(
                                                            raw,
                                                            &assignment.data_type,
                                                            assignment.nullable,
                                                            *text_type_modifier,
                                                        )?;
                                                        cached_assignment_values[idx] =
                                                            Some(coerced.clone());
                                                        coerced
                                                    }
                                                } else {
                                                    let raw = self
                                                        .evaluate_expr_with_row_prechecked(
                                                            &assignment.expr,
                                                            &record.row,
                                                            context,
                                                            false,
                                                        )?;
                                                    coerce_assigned_value(
                                                        raw,
                                                        &assignment.data_type,
                                                        assignment.nullable,
                                                        *text_type_modifier,
                                                    )?
                                                };
                                                if assignment.column_ordinal < values.len() {
                                                    values[assignment.column_ordinal] = value;
                                                }
                                            }
                                            if self.apply_update_to_tuple(
                                                &apply_ctx,
                                                &mut cached_assignment_values,
                                                &record.row,
                                                tuple_id,
                                                values,
                                                None,
                                                &mut returning_rows,
                                                &mut returning_result_bytes,
                                                context,
                                            )? {
                                                updated += 1;
                                            }
                                        }
                                        if has_after_update_statement_triggers {
                                            self.fire_statement_triggers(
                                                *table_id,
                                                TriggerEventDescriptor::Update,
                                                TriggerTimingDescriptor::After,
                                                context,
                                            )?;
                                        }
                                        return if has_returning {
                                            Ok(ExecutionResult::Query {
                                                columns: returning
                                                    .iter()
                                                    .map(|r| r.field.clone())
                                                    .collect(),
                                                rows: returning_rows,
                                            })
                                        } else {
                                            Ok(ExecutionResult::Command {
                                                tag: "UPDATE".to_owned(),
                                                rows_affected: updated,
                                            })
                                        };
                                    }
                                }
                            }
                        }

                        if let Some((column_predicates, residual)) =
                            self.dml_extract_pushdown_bounds(filter_expr, &table)
                        {
                            // Avoid clobbering the simple-eq fast
                            // path above when there's no residual:
                            // single-eq and IN-list shapes already
                            // have specialised post-collection
                            // handling (residual re-validation,
                            // dedup, case-lookup batching). When a
                            // residual EXISTS — e.g.
                            // `EXISTS(...) AND cat IN ('c0','c1')`
                            // — that branch wasn't entered (the
                            // top-level expr is `LogicalAnd` not a
                            // bare `InList`), so we DO want to
                            // take this path even for shapes whose
                            // bounds reduce to a single eq.
                            let is_single_eq_no_residual = residual.is_none()
                                && matches!(
                                    column_predicates.as_slice(),
                                    [(_, std::ops::Bound::Included(lo), std::ops::Bound::Included(hi))]
                                    if lo == hi
                                );
                            if !is_single_eq_no_residual {
                                let residual_requires_special = residual
                                    .as_ref()
                                    .is_some_and(dml_expr_requires_special_resolution);
                                // The pushdown does NOT need
                                // `filter_requires_special_resolution`
                                // because the special part is now
                                // isolated as `residual` and
                                // re-evaluated through
                                // `evaluate_expr_with_row_prechecked`
                                // per candidate (which DOES handle
                                // sub-queries via
                                // `resolve_special_expr`).
                                let mode = if context.isolation
                                    == aiondb_tx::IsolationLevel::Serializable
                                {
                                    LockMode::PredicateRead
                                } else {
                                    LockMode::AccessShare
                                };
                                self.lock_table(context, *table_id, mode)?;
                                context.record_relation_read(*table_id)?;
                                let bounds_for_call: Vec<(
                                    ColumnId,
                                    std::ops::Bound<Value>,
                                    std::ops::Bound<Value>,
                                )> = column_predicates.clone();
                                match self.storage_dml.scan_table_multi_range_filter(
                                    context.txn_id,
                                    &context.snapshot,
                                    *table_id,
                                    &bounds_for_call,
                                    None,
                                ) {
                                    Ok(mut filter_stream) => {
                                        let has_interrupts = context.has_execution_interrupts();
                                        while let Some(record) = filter_stream.next()? {
                                            if has_interrupts {
                                                context.check_deadline()?;
                                            }
                                            // Residual: the
                                            // sub-filter the
                                            // storage layer can't
                                            // push down (sub-query,
                                            // function call, etc.)
                                            // — re-evaluate per
                                            // candidate via the
                                            // executor's
                                            // expression evaluator.
                                            if let Some(residual_expr) = residual.as_ref() {
                                                if !self.evaluate_optional_predicate_prechecked(
                                                    Some(residual_expr),
                                                    &record.row,
                                                    context,
                                                    residual_requires_special,
                                                )? {
                                                    continue;
                                                }
                                            }
                                            if update_policies.is_some()
                                                && !self.compat_rls_allows_existing_row(
                                                    update_policies.as_deref(),
                                                    &record.row,
                                                    context,
                                                )?
                                            {
                                                continue;
                                            }
                                            let tuple_id = record.tuple_id;
                                            let mut values = record.row.values.clone();
                                            for (idx, (assignment, text_type_modifier)) in
                                                assignments
                                                    .iter()
                                                    .zip(assignment_text_type_modifiers.iter())
                                                    .enumerate()
                                            {
                                                let value = if assignment_is_row_independent[idx] {
                                                    if let Some(cached) =
                                                        &cached_assignment_values[idx]
                                                    {
                                                        cached.clone()
                                                    } else {
                                                        let raw = self
                                                            .evaluator
                                                            .evaluate(&assignment.expr)?;
                                                        let coerced = coerce_assigned_value(
                                                            raw,
                                                            &assignment.data_type,
                                                            assignment.nullable,
                                                            *text_type_modifier,
                                                        )?;
                                                        cached_assignment_values[idx] =
                                                            Some(coerced.clone());
                                                        coerced
                                                    }
                                                } else {
                                                    let raw = self
                                                        .evaluate_expr_with_row_prechecked(
                                                            &assignment.expr,
                                                            &record.row,
                                                            context,
                                                            false,
                                                        )?;
                                                    coerce_assigned_value(
                                                        raw,
                                                        &assignment.data_type,
                                                        assignment.nullable,
                                                        *text_type_modifier,
                                                    )?
                                                };
                                                if assignment.column_ordinal < values.len() {
                                                    values[assignment.column_ordinal] = value;
                                                }
                                            }
                                            if self.apply_update_to_tuple(
                                                &apply_ctx,
                                                &mut cached_assignment_values,
                                                &record.row,
                                                tuple_id,
                                                values,
                                                None,
                                                &mut returning_rows,
                                                &mut returning_result_bytes,
                                                context,
                                            )? {
                                                updated += 1;
                                            }
                                        }
                                        if has_after_update_statement_triggers {
                                            self.fire_statement_triggers(
                                                *table_id,
                                                TriggerEventDescriptor::Update,
                                                TriggerTimingDescriptor::After,
                                                context,
                                            )?;
                                        }
                                        return if has_returning {
                                            Ok(ExecutionResult::Query {
                                                columns: returning
                                                    .iter()
                                                    .map(|r| r.field.clone())
                                                    .collect(),
                                                rows: returning_rows,
                                            })
                                        } else {
                                            Ok(ExecutionResult::Command {
                                                tag: "UPDATE".to_owned(),
                                                rows_affected: updated,
                                            })
                                        };
                                    }
                                    Err(error)
                                        if error.report().sqlstate
                                            == SqlState::FeatureNotSupported => {}
                                    Err(error) => return Err(error),
                                }
                            }
                        }
                    }
                }

                // Detect a single-FROM equi-join `target.X = from.Y`
                // and pre-build a hash table on the FROM side. Per
                // target row we then do an O(1) bucket lookup
                // instead of the O(M) cross-join walk - the same
                // promotion PostgreSQL's planner does when it picks
                // a Hash Join over a Nested Loop. Falls back to the
                // legacy cross-join when no equi-join clause exists,
                // multi-FROM is in play, or the filter requires
                // special expression resolution.
                #[allow(unused_variables)]
                let from_hash_join_plan: Option<(
                    UpdateFromHashJoinPlan,
                    HashMap<ValueHashKey, Vec<usize>>,
                )> = if from_table_ids.len() == 1 && !filter_requires_special_resolution {
                    if let Some(filter_expr) = filter.as_ref() {
                        // Planner ordinals number system columns
                        // alongside user columns, so the boundary
                        // between target and FROM in the combined-row
                        // schema is `compat_row_width_for_table_id`,
                        // not `table.columns.len()`. Without this,
                        // ordinal arithmetic in the hash-join detector
                        // would either point into the system-column
                        // gap (false-positive from-side) or sit past
                        // the FROM row width (false-negative).
                        let target_col_count = if needs_compat_row {
                            self.compat_row_width_for_table_id(context, *table_id)?
                        } else {
                            table.columns.len()
                        };
                        let from_col_count = from_rows
                            .first()
                            .and_then(|t| t.first())
                            .map(|r| r.values.len())
                            .unwrap_or(0);
                        if from_col_count > 0 {
                            if let Some(plan) = extract_update_from_hash_join_plan(
                                filter_expr,
                                target_col_count,
                                from_col_count,
                            ) {
                                let mut hash: HashMap<ValueHashKey, Vec<usize>> =
                                    HashMap::with_capacity(from_rows[0].len());
                                for (i, row) in from_rows[0].iter().enumerate() {
                                    if let Some(v) = row.values.get(plan.from_ordinal) {
                                        if !v.is_null() {
                                            if let Ok(key) = build_hash_key(v) {
                                                hash.entry(key).or_default().push(i);
                                            }
                                        }
                                    }
                                }
                                Some((plan, hash))
                            } else {
                                None
                            }
                        } else {
                            None
                        }
                    } else {
                        None
                    }
                } else {
                    None
                };

                let mut stream = self.scan_table_locked(context, *table_id, None)?;
                let has_interrupts = context.has_execution_interrupts();
                while let Some(record) = stream.next()? {
                    if has_interrupts {
                        context.check_deadline()?;
                    }
                    if update_policies.is_some()
                        && !self.compat_rls_allows_existing_row(
                            update_policies.as_deref(),
                            &record.row,
                            context,
                        )?
                    {
                        continue;
                    }
                    let compat_row = needs_compat_row.then(|| {
                        self.compat_scan_row(&record, include_oid_system_column, Some(*table_id))
                    });
                    let eval_row = compat_row.as_ref().unwrap_or(&record.row);

                    if from_table_ids.is_empty() {
                        // Simple UPDATE without FROM.
                        if !self.evaluate_optional_predicate_prechecked(
                            filter.as_ref(),
                            eval_row,
                            context,
                            filter_requires_special_resolution,
                        )? {
                            continue;
                        }

                        let tuple_id = record.tuple_id;
                        let mut values = record.row.values.clone();
                        for (idx, (assignment, text_type_modifier)) in assignments
                            .iter()
                            .zip(assignment_text_type_modifiers.iter())
                            .enumerate()
                        {
                            let value = if assignment_is_row_independent[idx] {
                                if let Some(cached) = &cached_assignment_values[idx] {
                                    cached.clone()
                                } else {
                                    let raw = self.evaluator.evaluate(&assignment.expr)?;
                                    let coerced = coerce_assigned_value(
                                        raw,
                                        &assignment.data_type,
                                        assignment.nullable,
                                        *text_type_modifier,
                                    )?;
                                    cached_assignment_values[idx] = Some(coerced.clone());
                                    coerced
                                }
                            } else {
                                let raw = self.evaluate_expr_with_row_prechecked(
                                    &assignment.expr,
                                    eval_row,
                                    context,
                                    assignments_require_special_resolution,
                                )?;
                                coerce_assigned_value(
                                    raw,
                                    &assignment.data_type,
                                    assignment.nullable,
                                    *text_type_modifier,
                                )?
                            };
                            if assignment.column_ordinal < values.len() {
                                values[assignment.column_ordinal] = value;
                            }
                        }
                        if self.apply_update_to_tuple(
                            &apply_ctx,
                            &mut cached_assignment_values,
                            &record.row,
                            tuple_id,
                            values,
                            None,
                            &mut returning_rows,
                            &mut returning_result_bytes,
                            context,
                        )? {
                            updated += 1;
                        }
                    } else if let Some((plan, hash)) = from_hash_join_plan.as_ref() {
                        // Hash-join FROM path: O(1) bucket lookup per
                        // target row. PG promotes the equivalent
                        // `target.X = from.Y` shape from a Nested Loop
                        // to a Hash Join when the planner detects an
                        // equi-join clause; we replicate the same
                        // promotion at runtime to lift the previously
                        // O(N×M) cross-join walk to O(N + M).
                        let target_value = eval_row.values.get(plan.target_ordinal);
                        let bucket = match target_value {
                            Some(v) if !v.is_null() => match build_hash_key(v) {
                                Ok(key) => hash.get(&key),
                                Err(_) => None,
                            },
                            _ => None,
                        };
                        if let Some(match_indices) = bucket {
                            let mut applied = false;
                            for &from_idx in match_indices {
                                if applied {
                                    break;
                                }
                                let from_row = &from_rows[0][from_idx];
                                let mut combined_vals = Vec::with_capacity(
                                    eval_row.values.len() + from_row.values.len(),
                                );
                                combined_vals.extend_from_slice(&eval_row.values);
                                combined_vals.extend_from_slice(&from_row.values);
                                let combined_row = Row::new(combined_vals);

                                if let Some(residual) = plan.residual.as_ref() {
                                    if !self.evaluate_optional_predicate_prechecked(
                                        Some(residual),
                                        &combined_row,
                                        context,
                                        false,
                                    )? {
                                        continue;
                                    }
                                }

                                let tuple_id = record.tuple_id;
                                let mut values = record.row.values.clone();
                                for (idx, (assignment, text_type_modifier)) in assignments
                                    .iter()
                                    .zip(assignment_text_type_modifiers.iter())
                                    .enumerate()
                                {
                                    let value = if assignment_is_row_independent[idx] {
                                        if let Some(cached) = &cached_assignment_values[idx] {
                                            cached.clone()
                                        } else {
                                            let raw = self.evaluator.evaluate(&assignment.expr)?;
                                            let coerced = coerce_assigned_value(
                                                raw,
                                                &assignment.data_type,
                                                assignment.nullable,
                                                *text_type_modifier,
                                            )?;
                                            cached_assignment_values[idx] = Some(coerced.clone());
                                            coerced
                                        }
                                    } else {
                                        let raw = self.evaluate_expr_with_row_prechecked(
                                            &assignment.expr,
                                            &combined_row,
                                            context,
                                            assignments_require_special_resolution,
                                        )?;
                                        coerce_assigned_value(
                                            raw,
                                            &assignment.data_type,
                                            assignment.nullable,
                                            *text_type_modifier,
                                        )?
                                    };
                                    if assignment.column_ordinal < values.len() {
                                        values[assignment.column_ordinal] = value;
                                    }
                                }
                                if self.apply_update_to_tuple(
                                    &apply_ctx,
                                    &mut cached_assignment_values,
                                    &record.row,
                                    tuple_id,
                                    values,
                                    Some(&combined_row),
                                    &mut returning_rows,
                                    &mut returning_result_bytes,
                                    context,
                                )? {
                                    updated += 1;
                                }
                                applied = true;
                            }
                        }
                    } else {
                        // UPDATE ... FROM: cross-join target row with FROM
                        // table rows and find the first match. The
                        // closure returns `Ok(false)` once it has
                        // applied the update so the cross-join walk
                        // stops descending into the remaining
                        // combinations.
                        self.for_each_from_combination(
                            &from_rows,
                            0,
                            eval_row,
                            &mut |combined_row| {
                                if !self.evaluate_optional_predicate_prechecked(
                                    filter.as_ref(),
                                    combined_row,
                                    context,
                                    filter_requires_special_resolution,
                                )? {
                                    return Ok(true);
                                }

                                let tuple_id = record.tuple_id;
                                let mut values = record.row.values.clone();
                                for (idx, (assignment, text_type_modifier)) in assignments
                                    .iter()
                                    .zip(assignment_text_type_modifiers.iter())
                                    .enumerate()
                                {
                                    let value = if assignment_is_row_independent[idx] {
                                        if let Some(cached) = &cached_assignment_values[idx] {
                                            cached.clone()
                                        } else {
                                            let raw = self.evaluator.evaluate(&assignment.expr)?;
                                            let coerced = coerce_assigned_value(
                                                raw,
                                                &assignment.data_type,
                                                assignment.nullable,
                                                *text_type_modifier,
                                            )?;
                                            cached_assignment_values[idx] = Some(coerced.clone());
                                            coerced
                                        }
                                    } else {
                                        let raw = self.evaluate_expr_with_row_prechecked(
                                            &assignment.expr,
                                            combined_row,
                                            context,
                                            assignments_require_special_resolution,
                                        )?;
                                        coerce_assigned_value(
                                            raw,
                                            &assignment.data_type,
                                            assignment.nullable,
                                            *text_type_modifier,
                                        )?
                                    };
                                    if assignment.column_ordinal < values.len() {
                                        values[assignment.column_ordinal] = value;
                                    }
                                }
                                if self.apply_update_to_tuple(
                                    &apply_ctx,
                                    &mut cached_assignment_values,
                                    &record.row,
                                    tuple_id,
                                    values,
                                    Some(combined_row),
                                    &mut returning_rows,
                                    &mut returning_result_bytes,
                                    context,
                                )? {
                                    updated += 1;
                                }
                                Ok(false)
                            },
                        )?;
                    }
                }
                if has_after_update_statement_triggers {
                    self.fire_statement_triggers(
                        *table_id,
                        TriggerEventDescriptor::Update,
                        TriggerTimingDescriptor::After,
                        context,
                    )?;
                }

                if has_returning {
                    Ok(ExecutionResult::Query {
                        columns: returning.iter().map(|r| r.field.clone()).collect(),
                        rows: returning_rows,
                    })
                } else {
                    Ok(ExecutionResult::Command {
                        tag: "UPDATE".to_owned(),
                        rows_affected: updated,
                    })
                }
            }
            PhysicalPlan::MergeTable(merge_plan) => {
                context.check_deadline()?;
                use aiondb_plan::dml::MergeActionPlan;

                // Scan source and target tables, build lookup for target by ON condition.
                let target_id = merge_plan.target_table_id;
                let target_col_count = merge_plan.target_column_count;
                let target_table = self
                    .catalog_reader
                    .get_table_by_id(context.txn_id, target_id)?
                    .ok_or_else(|| {
                        DbError::internal(format!("target table {target_id:?} not found for MERGE"))
                    })?;

                // Collect source rows from either a table scan or an embedded
                // source subquery plan when present.
                let source_rows = self.materialize_merge_source_rows(merge_plan, context)?;

                // Snapshot target rows once for the whole MERGE statement.
                // PostgreSQL evaluates match candidates against a stable target
                // snapshot; rows updated/inserted by earlier source rows must
                // not become candidates for later source rows in the same MERGE.
                let mut target_snapshot: Vec<(aiondb_core::TupleId, Row, Row)> = Vec::new();
                {
                    let mut tgt_stream = self.scan_table_locked(context, target_id, None)?;
                    while let Some(tgt_record) = tgt_stream.next()? {
                        let compat_tgt_row =
                            self.compat_scan_row_for_table_id(context, target_id, &tgt_record)?;
                        target_snapshot.push((tgt_record.tuple_id, tgt_record.row, compat_tgt_row));
                    }
                }

                let mut rows_affected = 0u64;
                let mut affected_target_rows = std::collections::BTreeSet::new();

                for source_row in &source_rows {
                    context.check_deadline()?;
                    let mut matched_targets: Vec<(aiondb_core::TupleId, Row, Row)> = Vec::new();
                    for (tuple_id, tgt_row, compat_tgt_row) in &target_snapshot {
                        let mut combined_vals = Vec::with_capacity(
                            compat_tgt_row.values.len() + source_row.values.len(),
                        );
                        combined_vals.extend_from_slice(&compat_tgt_row.values);
                        combined_vals.extend_from_slice(&source_row.values);
                        let combined_row = Row::new(combined_vals);
                        let matched = self.evaluate_expr_with_row(
                            &merge_plan.on_condition,
                            &combined_row,
                            context,
                        )?;
                        if matched == Value::Boolean(true) {
                            matched_targets.push((
                                *tuple_id,
                                tgt_row.clone(),
                                compat_tgt_row.clone(),
                            ));
                        }
                    }

                    if matched_targets.is_empty() {
                        let mut vals =
                            Vec::with_capacity(target_col_count + source_row.values.len());
                        vals.resize(target_col_count, Value::Null);
                        vals.extend_from_slice(&source_row.values);
                        let combined_row = Row::new(vals);

                        for when_clause in &merge_plan.when_clauses {
                            if when_clause.matched {
                                continue;
                            }
                            if let Some(ref cond) = when_clause.condition {
                                let cond_val =
                                    self.evaluate_expr_with_row(cond, &combined_row, context)?;
                                if cond_val != Value::Boolean(true) {
                                    continue;
                                }
                            }

                            match &when_clause.action {
                                MergeActionPlan::Insert { values } => {
                                    // The MERGE NOT MATCHED INSERT
                                    // value expressions are bound at
                                    // type-check time against the
                                    // source relation only (PostgreSQL
                                    // semantics: only `source.col`
                                    // refs are valid in the unmatched
                                    // INSERT branch). Evaluating them
                                    // against `combined_row` (which
                                    // prepends `target_col_count`
                                    // NULLs) shifts every ColumnRef
                                    // ordinal by `target_col_count`,
                                    // so `src.id` reads the NULL
                                    // padding instead of the actual
                                    // source value. Use `source_row`
                                    // directly to honour the bound
                                    // ordinals.
                                    let insert_values: Vec<Value> = values
                                        .iter()
                                        .map(|expr| {
                                            self.evaluate_expr_with_row(expr, source_row, context)
                                        })
                                        .collect::<DbResult<Vec<_>>>()?;
                                    self.insert_locked(
                                        context,
                                        target_id,
                                        Row::new(insert_values),
                                    )?;
                                    rows_affected += 1;
                                }
                                MergeActionPlan::InsertDefaultValues => {
                                    let insert_values = vec![Value::Null; target_col_count];
                                    self.insert_locked(
                                        context,
                                        target_id,
                                        Row::new(insert_values),
                                    )?;
                                    rows_affected += 1;
                                }
                                MergeActionPlan::DoNothing
                                | MergeActionPlan::Update { .. }
                                | MergeActionPlan::Delete => {}
                            }
                            break;
                        }
                        continue;
                    }

                    for (tuple_id, tgt_row, compat_tgt_row) in matched_targets {
                        context.check_deadline()?;
                        let mut combined_vals = Vec::with_capacity(
                            compat_tgt_row.values.len() + source_row.values.len(),
                        );
                        combined_vals.extend_from_slice(&compat_tgt_row.values);
                        combined_vals.extend_from_slice(&source_row.values);
                        let combined_row = Row::new(combined_vals);

                        for when_clause in &merge_plan.when_clauses {
                            if !when_clause.matched {
                                continue;
                            }
                            if let Some(ref cond) = when_clause.condition {
                                let cond_val =
                                    self.evaluate_expr_with_row(cond, &combined_row, context)?;
                                if cond_val != Value::Boolean(true) {
                                    continue;
                                }
                            }

                            match &when_clause.action {
                                MergeActionPlan::Update { assignments } => {
                                    if !affected_target_rows.insert(tuple_id) {
                                        return Err(DbError::constraint_error(
                                            SqlState::InvalidParameterValue,
                                            "MERGE command cannot affect row a second time",
                                        )
                                        .with_client_hint(
                                            "Ensure that not more than one source row matches any one target row.",
                                        ));
                                    }

                                    let mut values = tgt_row.values.clone();
                                    for assignment in assignments {
                                        let value = self.evaluate_expr_with_row(
                                            &assignment.expr,
                                            &combined_row,
                                            context,
                                        )?;
                                        let text_type_modifier = target_table
                                            .columns
                                            .get(assignment.column_ordinal)
                                            .and_then(|column| column.text_type_modifier);
                                        let value = coerce_assigned_value(
                                            value,
                                            &assignment.data_type,
                                            assignment.nullable,
                                            text_type_modifier,
                                        )?;
                                        if assignment.column_ordinal < values.len() {
                                            values[assignment.column_ordinal] = value;
                                        }
                                    }
                                    enforce_not_null_constraints_for_table(&values, &target_table)?;
                                    let updated_row = Row::new(values);
                                    self.update_locked(
                                        context,
                                        target_id,
                                        tuple_id,
                                        Some(&tgt_row),
                                        updated_row,
                                    )?;
                                    rows_affected += 1;
                                }
                                MergeActionPlan::Delete => {
                                    if !affected_target_rows.insert(tuple_id) {
                                        return Err(DbError::constraint_error(
                                            SqlState::InvalidParameterValue,
                                            "MERGE command cannot affect row a second time",
                                        )
                                        .with_client_hint(
                                            "Ensure that not more than one source row matches any one target row.",
                                        ));
                                    }

                                    self.delete_locked(
                                        context,
                                        target_id,
                                        tuple_id,
                                        Some(&tgt_row),
                                    )?;
                                    rows_affected += 1;
                                }
                                MergeActionPlan::DoNothing
                                | MergeActionPlan::Insert { .. }
                                | MergeActionPlan::InsertDefaultValues => {}
                            }
                            break;
                        }
                    }
                }

                Ok(ExecutionResult::Command {
                    tag: "MERGE".to_owned(),
                    rows_affected,
                })
            }
            _ => Err(DbError::internal("non-DML plan routed to DML executor")),
        }
    }

    fn push_returning_row_with_limits(
        &self,
        returning_rows: &mut Vec<Row>,
        row: Row,
        context: &ExecutionContext,
        result_bytes: &mut u64,
    ) -> DbResult<()> {
        if usize_to_u64(returning_rows.len()) >= context.max_result_rows {
            return Err(DbError::program_limit(
                "maximum number of result rows reached",
            ));
        }
        *result_bytes = ensure_result_bytes_fit_and_track_query_row(context, &row, *result_bytes)?;
        returning_rows.push(row);
        Ok(())
    }

    fn materialize_merge_source_rows(
        &self,
        merge_plan: &aiondb_plan::dml::MergePlan,
        context: &ExecutionContext,
    ) -> DbResult<Vec<Row>> {
        let source_row_cap = internal_materialize_row_cap(context);
        if let Some(source_subquery_plan) = Self::merge_source_subquery_plan(merge_plan) {
            let mut source_context = context.clone();
            source_context.max_result_rows = source_context.max_result_rows.min(source_row_cap);
            source_context.collect_row_limit = None;
            source_context.collect_row_offset = 0;
            source_context.max_result_bytes =
                source_context.max_result_bytes.max(context.max_temp_bytes);

            let source_result = self.execute(&source_subquery_plan, &source_context)?;
            let ExecutionResult::Query { rows, .. } = source_result else {
                return Err(DbError::internal(
                    "MERGE source_subquery_plan did not produce query rows",
                ));
            };
            if usize_to_u64(rows.len()) > source_row_cap {
                return Err(DbError::program_limit(
                    "maximum number of internally materialized rows reached for MERGE source",
                ));
            }
            for _ in &rows {
                // `execute` already accounts for row payload bytes in query
                // rows. Track a small per-row Vec/materialization overhead to
                // stay aligned with table-source MERGE accounting.
                context.track_memory(64)?;
            }
            return Ok(rows);
        }

        self.materialize_table_rows_with_limits(
            context,
            merge_plan.source_table_id,
            source_row_cap,
            "MERGE source",
        )
    }

    fn merge_source_subquery_plan(
        merge_plan: &aiondb_plan::dml::MergePlan,
    ) -> Option<PhysicalPlan> {
        merge_plan
            .source_subquery_plan
            .as_ref()
            .map(|plan| plan.as_ref().clone())
    }

    fn materialize_table_rows_with_limits(
        &self,
        context: &ExecutionContext,
        table_id: RelationId,
        row_cap: u64,
        clause: &str,
    ) -> DbResult<Vec<Row>> {
        let mut rows = Vec::new();
        let mut stream = self.scan_table_locked(context, table_id, None)?;
        while let Some(record) = stream.next()? {
            context.check_deadline()?;
            if usize_to_u64(rows.len()) >= row_cap {
                return Err(DbError::program_limit(format!(
                    "maximum number of internally materialized rows reached for {clause}",
                )));
            }
            let row = self.compat_scan_row_for_table_id(context, table_id, &record)?;
            context.track_memory(estimate_row_bytes(&row).saturating_add(64))?;
            rows.push(row);
        }
        Ok(rows)
    }

    /// Recursively iterate through the Cartesian product of `from_rows` tables,
    /// building a combined row with the target row's columns followed by each
    /// FROM table's columns.  Calls `callback` for each combination.
    /// Walk every `from_rows` cross-join combination, prepending the
    /// `current_combined` target row in front of each. The callback
    /// returns `Ok(true)` to keep iterating and `Ok(false)` to stop -
    /// the recursion unwinds without touching any further combinations.
    /// UPDATE ... FROM uses the stop signal to skip the rest of the
    /// cross product once the first matching FROM combination has been
    /// applied to the target tuple, avoiding the per-combination
    /// `Row::new(scratch.clone())` allocation that the original
    /// continue-always loop paid even after the caller had already
    /// matched and stopped doing useful work.
    fn for_each_from_combination(
        &self,
        from_rows: &[Vec<Row>],
        table_index: usize,
        current_combined: &Row,
        callback: &mut dyn FnMut(&Row) -> DbResult<bool>,
    ) -> DbResult<()> {
        let mut scratch = current_combined.values.clone();
        let mut keep_going = true;
        self.for_each_from_combination_with_scratch(
            from_rows,
            table_index,
            &mut scratch,
            &mut keep_going,
            callback,
        )
    }

    fn for_each_from_combination_with_scratch(
        &self,
        from_rows: &[Vec<Row>],
        table_index: usize,
        scratch: &mut Vec<Value>,
        keep_going: &mut bool,
        callback: &mut dyn FnMut(&Row) -> DbResult<bool>,
    ) -> DbResult<()> {
        if !*keep_going {
            return Ok(());
        }
        if table_index >= from_rows.len() {
            let combined = Row::new(scratch.clone());
            *keep_going = callback(&combined)?;
            return Ok(());
        }
        for from_row in &from_rows[table_index] {
            if !*keep_going {
                return Ok(());
            }
            let checkpoint = scratch.len();
            scratch.extend_from_slice(&from_row.values);
            self.for_each_from_combination_with_scratch(
                from_rows,
                table_index + 1,
                scratch,
                keep_going,
                callback,
            )?;
            scratch.truncate(checkpoint);
        }
        Ok(())
    }

    /// Per-row UPDATE apply hot path. Runs every check + the storage
    /// update + AFTER trigger + post-update FK action that the legacy
    /// fast-path / seq-scan / FROM closure copies all duplicated. Each
    /// access path (eq/IN, range, full scan, FROM cross-join) calls
    /// this with the new `values` it produced; the helper handles:
    ///
    /// 1. RLS WITH CHECK enforcement
    /// 2. BEFORE row trigger firing (returns `Ok(false)` if the trigger
    ///    chose to suppress the row)
    /// 3. NOT NULL / FK / CHECK / UNIQUE constraint enforcement
    /// 4. RETURNING projection (with optional FROM combined-row
    ///    materialisation)
    /// 5. AFTER row trigger firing
    /// 6. Storage update + parent-side FK referential actions
    ///
    /// Pulling these into one method removes ~150 lines of triplicated
    /// code and is a prerequisite for adding new index access paths
    /// (range / BETWEEN / composite) without re-duplicating the body.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn apply_update_to_tuple(
        &self,
        ctx: &UpdateApplyCtx<'_>,
        cached_assignment_values: &mut [Option<Value>],
        old_row: &Row,
        tuple_id: aiondb_core::TupleId,
        mut values: Vec<Value>,
        returning_combined: Option<&Row>,
        returning_rows: &mut Vec<Row>,
        returning_result_bytes: &mut u64,
        context: &ExecutionContext,
    ) -> DbResult<bool> {
        if ctx.update_policies.is_some() {
            let row = Row::new(std::mem::take(&mut values));
            self.compat_rls_enforce_new_row(ctx.table, ctx.update_policies, &row, context)?;
            values = row.values;
        }

        if ctx.has_before_update_row_triggers
            && !self.fire_before_update_triggers_with_list(
                ctx.table_id,
                ctx.before_update_row_triggers,
                &mut values,
                &old_row.values,
                context,
            )?
        {
            return Ok(false);
        }

        enforce_not_null_constraints_on_updated_columns(
            &values,
            ctx.updated_not_null_ordinals,
            ctx.table,
        )?;
        if ctx.has_referencing_update_fks {
            self.enforce_fk_referenced_on_parent_update_restrict_with_entries(
                ctx.referencing_update_fk_entries,
                ctx.table,
                &old_row.values,
                &values,
                context,
            )?;
        }
        if ctx.has_fk_constraints {
            self.enforce_fk_on_update_with_compiled_diff(
                ctx.compiled_child_fk_checks,
                &old_row.values,
                &values,
                context,
            )?;
        }
        if ctx.has_check_constraints {
            self.enforce_compiled_check_constraints(
                ctx.compiled_check_constraints,
                &values,
                ctx.table,
                context,
            )?;
        }
        if ctx.may_affect_unique_indexes {
            self.enforce_unique_on_update_with_old_row(
                ctx.table,
                ctx.unique_indexes_for_update,
                old_row,
                &values,
                tuple_id,
                context,
            )?;
        }

        let updated_row = Row::new(values);
        if ctx.has_returning {
            // For UPDATE ... FROM, RETURNING may reference both target
            // columns (carrying the post-update values) and the joined
            // FROM table columns (which keep their pre-update values).
            // The caller passes the pre-update combined row via
            // `returning_combined`; we splice the updated target
            // columns over it before projecting.
            let returning_row = if let Some(combined_row) = returning_combined {
                let target_col_count = updated_row.values.len();
                let mut returning_combined_vals = Vec::with_capacity(combined_row.values.len());
                returning_combined_vals.extend_from_slice(&updated_row.values);
                if combined_row.values.len() > target_col_count {
                    returning_combined_vals
                        .extend_from_slice(&combined_row.values[target_col_count..]);
                }
                self.project_outputs_with_precomputed_ordinals(
                    ctx.returning,
                    ctx.returning_direct_column_ordinals,
                    &Row::new(returning_combined_vals),
                    context,
                )?
            } else {
                self.project_outputs_with_precomputed_ordinals(
                    ctx.returning,
                    ctx.returning_direct_column_ordinals,
                    &updated_row,
                    context,
                )?
            };
            self.push_returning_row_with_limits(
                returning_rows,
                returning_row,
                context,
                returning_result_bytes,
            )?;
        }
        if ctx.has_after_update_row_triggers {
            self.fire_after_update_triggers_with_list(
                ctx.table_id,
                ctx.after_update_row_triggers,
                &updated_row.values,
                &old_row.values,
                context,
            )?;
        }
        // Custom AionDB no-op-write elision: when the post-projection
        // row matches the pre-update row byte-for-byte AND no observer
        // can detect the missed write (no AFTER row trigger, no
        // referencing-FK referential action), skip the storage write
        // entirely. PostgreSQL always writes a new tuple version, but
        // for AionDB the only externally-visible side effect of a
        // no-op write is the bumped xmin/xmax history, which clients
        // cannot observe. The bench harness validates that this saves
        // meaningful time on `SET col = col` migration backfills
        // without changing query results.
        // Determine whether the post-projection row differs from the
        // pre-update row. When no BEFORE row trigger ran, only the
        // UPDATE target ordinals could have moved, so we shrink the
        // O(table.cols) compare to O(updated_target_ordinals). A
        // BEFORE trigger, however, is free to write `NEW.col` for
        // columns the UPDATE did not target (or to revert one of them
        // back to the OLD value), so when triggers fired we have to
        // compare the full row to stay correct.
        let row_unchanged = if ctx.has_before_update_row_triggers {
            updated_row.values == old_row.values
        } else {
            ctx.updated_target_ordinals
                .iter()
                .all(|&ord| updated_row.values.get(ord) == old_row.values.get(ord))
        };
        let safe_to_skip_write =
            row_unchanged && !ctx.has_after_update_row_triggers && !ctx.has_referencing_update_fks;
        if safe_to_skip_write {
            return Ok(true);
        }
        // NOTE: AionDB intentionally returns `SerializationFailure`
        // (strict lost-update prevention) on a concurrent-write
        // collision at every isolation level. PostgreSQL would run
        // EvalPlanQual at READ COMMITTED and silently retry; the
        // `update_after_table_locked_with_epq` helper + the EPQ
        // ctx fields below are wired and tested but kept opt-in
        // because the engine-level concurrency tests
        // (`lost_update_prevention`,
        // `concurrent_update_fails_instead_of_losing_an_update`)
        // enforce the strict contract. A future session GUC can flip
        // the apply path to the EPQ variant per workload.
        if ctx.has_referencing_update_fks {
            self.update_after_table_locked(
                context,
                ctx.table_id,
                tuple_id,
                Some(old_row),
                updated_row.clone(),
            )?;
            self.apply_fk_referenced_on_parent_update_actions_with_entries(
                ctx.referencing_update_fk_entries,
                &old_row.values,
                &updated_row.values,
                context,
            )?;
        } else {
            self.update_after_table_locked(
                context,
                ctx.table_id,
                tuple_id,
                Some(old_row),
                updated_row,
            )?;
        }
        // Suppress unused-variable lints for the EPQ-only fields kept
        // on `UpdateApplyCtx` for the future opt-in path.
        let _ = (
            ctx.filter,
            ctx.filter_requires_special_resolution,
            ctx.assignments,
            ctx.assignment_text_type_modifiers,
            ctx.assignment_is_row_independent,
            ctx.assignments_require_special_resolution,
            cached_assignment_values,
        );
        Ok(true)
    }
}

/// Per-statement state shared by every per-row UPDATE callsite. Mirrors
/// the PostgreSQL `ResultRelInfo` / `triggerdesc` pattern: the catalog
/// reads, constraint compilations, trigger filtering, and FK metadata
/// resolution are all done once at executor-start time and reused for
/// every modified tuple.
pub(super) struct UpdateApplyCtx<'a> {
    pub table: &'a TableDescriptor,
    pub table_id: RelationId,
    /// `WHERE` predicate, when present. EvalPlanQual retries call back
    /// here to recheck the latest visible tuple version against the
    /// original UPDATE filter.
    pub filter: Option<&'a TypedExpr>,
    pub filter_requires_special_resolution: bool,
    /// Pre-resolved `UpdateAssignment` slice. EPQ retries rebuild the
    /// `Vec<Value>` against the latest row using this same list (so
    /// `SET v = v + 1` re-evaluates against the current `v`, not the
    /// stale snapshot value).
    pub assignments: &'a [PlanUpdateAssignment],
    pub assignment_text_type_modifiers: &'a [Option<aiondb_core::TextTypeModifier>],
    pub assignment_is_row_independent: &'a [bool],
    pub assignments_require_special_resolution: bool,
    pub update_policies: Option<&'a [CompatRlsPolicy]>,
    pub has_before_update_row_triggers: bool,
    pub before_update_row_triggers: &'a [TriggerDescriptor],
    pub has_after_update_row_triggers: bool,
    pub after_update_row_triggers: &'a [TriggerDescriptor],
    pub updated_not_null_ordinals: &'a [usize],
    /// Ordinals targeted by any UPDATE assignment, deduped/sorted.
    /// Used by the no-op-write elision heuristic so it inspects only
    /// the columns the UPDATE could have moved instead of comparing
    /// the full row.
    pub updated_target_ordinals: &'a [usize],
    pub has_referencing_update_fks: bool,
    pub referencing_update_fk_entries: &'a [ReferencingUpdateFkEntry],
    pub has_fk_constraints: bool,
    pub compiled_child_fk_checks: &'a [CompiledChildFkCheck],
    pub has_check_constraints: bool,
    pub compiled_check_constraints: &'a [CompiledCheckConstraint],
    pub may_affect_unique_indexes: bool,
    pub unique_indexes_for_update: &'a [UniqueIndexForUpdate],
    pub has_returning: bool,
    pub returning: &'a [ProjectionExpr],
    pub returning_direct_column_ordinals: Option<&'a [usize]>,
}

fn dml_expr_requires_special_resolution(expr: &TypedExpr) -> bool {
    match &expr.kind {
        TypedExprKind::Literal(_) | TypedExprKind::ColumnRef { .. } => false,
        TypedExprKind::OuterColumnRef { .. }
        | TypedExprKind::NextValue { .. }
        | TypedExprKind::ScalarSubquery { .. }
        | TypedExprKind::ArraySubquery { .. }
        | TypedExprKind::InSubquery { .. }
        | TypedExprKind::ExistsSubquery { .. }
        | TypedExprKind::UserFunction { .. } => true,
        TypedExprKind::ScalarFunction { .. } => true,
        TypedExprKind::BinaryEq { left, right }
        | TypedExprKind::BinaryNe { left, right }
        | TypedExprKind::BinaryGe { left, right }
        | TypedExprKind::BinaryGt { left, right }
        | TypedExprKind::BinaryLe { left, right }
        | TypedExprKind::BinaryLt { left, right }
        | TypedExprKind::LogicalAnd { left, right }
        | TypedExprKind::LogicalOr { left, right }
        | TypedExprKind::ArithAdd { left, right }
        | TypedExprKind::ArithSub { left, right }
        | TypedExprKind::ArithMul { left, right }
        | TypedExprKind::ArithDiv { left, right }
        | TypedExprKind::ArithMod { left, right }
        | TypedExprKind::Concat { left, right }
        | TypedExprKind::JsonGet { left, right }
        | TypedExprKind::JsonGetText { left, right }
        | TypedExprKind::JsonPathGet { left, right }
        | TypedExprKind::JsonPathGetText { left, right }
        | TypedExprKind::JsonContains { left, right }
        | TypedExprKind::JsonContainedBy { left, right }
        | TypedExprKind::JsonKeyExists { left, right }
        | TypedExprKind::JsonAnyKeyExists { left, right }
        | TypedExprKind::JsonAllKeysExist { left, right }
        | TypedExprKind::ArrayConcat { left, right }
        | TypedExprKind::ArrayContains { left, right }
        | TypedExprKind::ArrayContainedBy { left, right }
        | TypedExprKind::ArrayOverlap { left, right }
        | TypedExprKind::IsDistinctFrom { left, right, .. }
        | TypedExprKind::Nullif { left, right } => {
            dml_expr_requires_special_resolution(left)
                || dml_expr_requires_special_resolution(right)
        }
        TypedExprKind::LogicalNot { expr }
        | TypedExprKind::Negate { expr }
        | TypedExprKind::IsNull { expr, .. }
        | TypedExprKind::Cast { expr, .. } => dml_expr_requires_special_resolution(expr),
        TypedExprKind::Like { expr, pattern, .. } => {
            dml_expr_requires_special_resolution(expr)
                || dml_expr_requires_special_resolution(pattern)
        }
        TypedExprKind::InList { expr, list, .. } => {
            dml_expr_requires_special_resolution(expr)
                || list.iter().any(dml_expr_requires_special_resolution)
        }
        TypedExprKind::Between {
            expr, low, high, ..
        } => {
            dml_expr_requires_special_resolution(expr)
                || dml_expr_requires_special_resolution(low)
                || dml_expr_requires_special_resolution(high)
        }
        TypedExprKind::CaseWhen {
            conditions,
            results,
            else_result,
        } => {
            conditions.iter().any(dml_expr_requires_special_resolution)
                || results.iter().any(dml_expr_requires_special_resolution)
                || else_result
                    .as_ref()
                    .is_some_and(|expr| dml_expr_requires_special_resolution(expr))
        }
        TypedExprKind::Coalesce { args } | TypedExprKind::ArrayConstruct { elements: args } => {
            args.iter().any(dml_expr_requires_special_resolution)
        }
        _ => true,
    }
}

fn dml_expr_references_compat_system_column(expr: &TypedExpr) -> bool {
    match &expr.kind {
        TypedExprKind::ColumnRef { name, .. } | TypedExprKind::OuterColumnRef { name, .. } => {
            matches!(
                name.to_ascii_lowercase().as_str(),
                "ctid" | "tableoid" | "xmin" | "xmax" | "cmin" | "cmax" | "oid"
            )
        }
        TypedExprKind::BinaryEq { left, right }
        | TypedExprKind::BinaryNe { left, right }
        | TypedExprKind::BinaryGe { left, right }
        | TypedExprKind::BinaryGt { left, right }
        | TypedExprKind::BinaryLe { left, right }
        | TypedExprKind::BinaryLt { left, right }
        | TypedExprKind::LogicalAnd { left, right }
        | TypedExprKind::LogicalOr { left, right }
        | TypedExprKind::ArithAdd { left, right }
        | TypedExprKind::ArithSub { left, right }
        | TypedExprKind::ArithMul { left, right }
        | TypedExprKind::ArithDiv { left, right }
        | TypedExprKind::ArithMod { left, right }
        | TypedExprKind::Concat { left, right }
        | TypedExprKind::JsonGet { left, right }
        | TypedExprKind::JsonGetText { left, right }
        | TypedExprKind::JsonPathGet { left, right }
        | TypedExprKind::JsonPathGetText { left, right }
        | TypedExprKind::JsonContains { left, right }
        | TypedExprKind::JsonContainedBy { left, right }
        | TypedExprKind::JsonKeyExists { left, right }
        | TypedExprKind::JsonAnyKeyExists { left, right }
        | TypedExprKind::JsonAllKeysExist { left, right }
        | TypedExprKind::ArrayConcat { left, right }
        | TypedExprKind::ArrayContains { left, right }
        | TypedExprKind::ArrayContainedBy { left, right }
        | TypedExprKind::ArrayOverlap { left, right }
        | TypedExprKind::IsDistinctFrom { left, right, .. }
        | TypedExprKind::Nullif { left, right } => {
            dml_expr_references_compat_system_column(left)
                || dml_expr_references_compat_system_column(right)
        }
        TypedExprKind::LogicalNot { expr }
        | TypedExprKind::Negate { expr }
        | TypedExprKind::IsNull { expr, .. }
        | TypedExprKind::Cast { expr, .. } => dml_expr_references_compat_system_column(expr),
        TypedExprKind::Like { expr, pattern, .. } => {
            dml_expr_references_compat_system_column(expr)
                || dml_expr_references_compat_system_column(pattern)
        }
        TypedExprKind::InList { expr, list, .. } => {
            dml_expr_references_compat_system_column(expr)
                || list.iter().any(dml_expr_references_compat_system_column)
        }
        TypedExprKind::Between {
            expr, low, high, ..
        } => {
            dml_expr_references_compat_system_column(expr)
                || dml_expr_references_compat_system_column(low)
                || dml_expr_references_compat_system_column(high)
        }
        TypedExprKind::CaseWhen {
            conditions,
            results,
            else_result,
        } => {
            conditions
                .iter()
                .any(dml_expr_references_compat_system_column)
                || results.iter().any(dml_expr_references_compat_system_column)
                || else_result
                    .as_ref()
                    .is_some_and(|expr| dml_expr_references_compat_system_column(expr))
        }
        TypedExprKind::Coalesce { args }
        | TypedExprKind::ScalarFunction { args, .. }
        | TypedExprKind::ArrayConstruct { elements: args }
        | TypedExprKind::UserFunction { args, .. } => {
            args.iter().any(dml_expr_references_compat_system_column)
        }
        TypedExprKind::AggCount { expr, filter, .. } => {
            expr.as_deref()
                .is_some_and(dml_expr_references_compat_system_column)
                || filter
                    .as_deref()
                    .is_some_and(dml_expr_references_compat_system_column)
        }
        TypedExprKind::AggSum { expr, filter, .. }
        | TypedExprKind::AggAvg { expr, filter, .. }
        | TypedExprKind::AggAnyValue { expr, filter }
        | TypedExprKind::AggMin { expr, filter }
        | TypedExprKind::AggMax { expr, filter }
        | TypedExprKind::AggBoolAnd { expr, filter }
        | TypedExprKind::AggBoolOr { expr, filter }
        | TypedExprKind::AggStddevPop { expr, filter }
        | TypedExprKind::AggStddevSamp { expr, filter }
        | TypedExprKind::AggVarPop { expr, filter }
        | TypedExprKind::AggVarSamp { expr, filter } => {
            dml_expr_references_compat_system_column(expr)
                || filter
                    .as_deref()
                    .is_some_and(dml_expr_references_compat_system_column)
        }
        TypedExprKind::AggStringAgg {
            expr,
            delimiter,
            filter,
            ..
        } => {
            dml_expr_references_compat_system_column(expr)
                || dml_expr_references_compat_system_column(delimiter)
                || filter
                    .as_deref()
                    .is_some_and(dml_expr_references_compat_system_column)
        }
        TypedExprKind::AggArrayAgg { expr, filter, .. } => {
            dml_expr_references_compat_system_column(expr)
                || filter
                    .as_deref()
                    .is_some_and(dml_expr_references_compat_system_column)
        }
        TypedExprKind::WindowFunction {
            args,
            partition_by,
            order_by,
            ..
        } => {
            args.iter().any(dml_expr_references_compat_system_column)
                || partition_by
                    .iter()
                    .any(dml_expr_references_compat_system_column)
                || order_by
                    .iter()
                    .any(|sort| dml_expr_references_compat_system_column(&sort.expr))
        }
        _ => false,
    }
}

/// Returns true iff `expr` references some column from the input row
/// (either the target row or, in UPDATE ... FROM, the joined row).
/// Used together with `dml_expr_requires_special_resolution` to identify
/// row-independent assignment expressions that can be evaluated once and
/// reused across every updated row, mirroring PostgreSQL's projection
/// initialization that pre-evaluates `Const` nodes.
fn dml_expr_references_any_column(expr: &TypedExpr) -> bool {
    match &expr.kind {
        TypedExprKind::ColumnRef { .. } | TypedExprKind::OuterColumnRef { .. } => true,
        TypedExprKind::Literal(_) | TypedExprKind::NextValue { .. } => false,
        TypedExprKind::BinaryEq { left, right }
        | TypedExprKind::BinaryNe { left, right }
        | TypedExprKind::BinaryGe { left, right }
        | TypedExprKind::BinaryGt { left, right }
        | TypedExprKind::BinaryLe { left, right }
        | TypedExprKind::BinaryLt { left, right }
        | TypedExprKind::LogicalAnd { left, right }
        | TypedExprKind::LogicalOr { left, right }
        | TypedExprKind::ArithAdd { left, right }
        | TypedExprKind::ArithSub { left, right }
        | TypedExprKind::ArithMul { left, right }
        | TypedExprKind::ArithDiv { left, right }
        | TypedExprKind::ArithMod { left, right }
        | TypedExprKind::Concat { left, right }
        | TypedExprKind::JsonGet { left, right }
        | TypedExprKind::JsonGetText { left, right }
        | TypedExprKind::JsonPathGet { left, right }
        | TypedExprKind::JsonPathGetText { left, right }
        | TypedExprKind::JsonContains { left, right }
        | TypedExprKind::JsonContainedBy { left, right }
        | TypedExprKind::JsonKeyExists { left, right }
        | TypedExprKind::JsonAnyKeyExists { left, right }
        | TypedExprKind::JsonAllKeysExist { left, right }
        | TypedExprKind::ArrayConcat { left, right }
        | TypedExprKind::ArrayContains { left, right }
        | TypedExprKind::ArrayContainedBy { left, right }
        | TypedExprKind::ArrayOverlap { left, right }
        | TypedExprKind::IsDistinctFrom { left, right, .. }
        | TypedExprKind::Nullif { left, right } => {
            dml_expr_references_any_column(left) || dml_expr_references_any_column(right)
        }
        TypedExprKind::LogicalNot { expr }
        | TypedExprKind::Negate { expr }
        | TypedExprKind::IsNull { expr, .. }
        | TypedExprKind::Cast { expr, .. } => dml_expr_references_any_column(expr),
        TypedExprKind::Like { expr, pattern, .. } => {
            dml_expr_references_any_column(expr) || dml_expr_references_any_column(pattern)
        }
        TypedExprKind::InList { expr, list, .. } => {
            dml_expr_references_any_column(expr) || list.iter().any(dml_expr_references_any_column)
        }
        TypedExprKind::Between {
            expr, low, high, ..
        } => {
            dml_expr_references_any_column(expr)
                || dml_expr_references_any_column(low)
                || dml_expr_references_any_column(high)
        }
        TypedExprKind::CaseWhen {
            conditions,
            results,
            else_result,
        } => {
            conditions.iter().any(dml_expr_references_any_column)
                || results.iter().any(dml_expr_references_any_column)
                || else_result
                    .as_ref()
                    .is_some_and(|e| dml_expr_references_any_column(e))
        }
        TypedExprKind::Coalesce { args }
        | TypedExprKind::ScalarFunction { args, .. }
        | TypedExprKind::ArrayConstruct { elements: args }
        | TypedExprKind::UserFunction { args, .. } => {
            args.iter().any(dml_expr_references_any_column)
        }
        // Aggregates, window functions, and subqueries: conservatively
        // treat as row-dependent. They should not appear in UPDATE
        // assignment expressions, but stay safe if they ever do.
        _ => true,
    }
}

/// An UPDATE assignment expression is "row-independent" when it can be
/// safely evaluated once and reused for every updated row, matching the
/// PostgreSQL projection-init-time `Const` evaluation. We require both
/// (a) no per-row-special resolution (rules out `now()`, `nextval()`,
/// `random()`, subqueries, parameterized lookups, …) and (b) no column
/// reference of any kind.
pub(super) fn dml_expr_is_row_independent(expr: &TypedExpr) -> bool {
    !dml_expr_requires_special_resolution(expr) && !dml_expr_references_any_column(expr)
}

/// Walk `expr` collecting every `ColumnRef` ordinal it touches (only
/// the local-row references; OuterColumnRef belongs to a different row
/// and is irrelevant for the per-row UPDATE constraint hot path).
/// Used to decide whether a CHECK constraint can be skipped on UPDATE
/// because none of its referenced columns appear in the modified set -
/// the same logic PostgreSQL applies via `pg_get_constraintdef` /
/// modified-attrs intersection.
fn collect_dml_expr_local_column_ordinals(
    expr: &TypedExpr,
    out: &mut std::collections::HashSet<usize>,
) {
    match &expr.kind {
        TypedExprKind::ColumnRef { ordinal, .. } => {
            out.insert(*ordinal);
        }
        TypedExprKind::Literal(_)
        | TypedExprKind::OuterColumnRef { .. }
        | TypedExprKind::NextValue { .. } => {}
        TypedExprKind::BinaryEq { left, right }
        | TypedExprKind::BinaryNe { left, right }
        | TypedExprKind::BinaryGe { left, right }
        | TypedExprKind::BinaryGt { left, right }
        | TypedExprKind::BinaryLe { left, right }
        | TypedExprKind::BinaryLt { left, right }
        | TypedExprKind::LogicalAnd { left, right }
        | TypedExprKind::LogicalOr { left, right }
        | TypedExprKind::ArithAdd { left, right }
        | TypedExprKind::ArithSub { left, right }
        | TypedExprKind::ArithMul { left, right }
        | TypedExprKind::ArithDiv { left, right }
        | TypedExprKind::ArithMod { left, right }
        | TypedExprKind::Concat { left, right }
        | TypedExprKind::JsonGet { left, right }
        | TypedExprKind::JsonGetText { left, right }
        | TypedExprKind::JsonPathGet { left, right }
        | TypedExprKind::JsonPathGetText { left, right }
        | TypedExprKind::JsonContains { left, right }
        | TypedExprKind::JsonContainedBy { left, right }
        | TypedExprKind::JsonKeyExists { left, right }
        | TypedExprKind::JsonAnyKeyExists { left, right }
        | TypedExprKind::JsonAllKeysExist { left, right }
        | TypedExprKind::ArrayConcat { left, right }
        | TypedExprKind::ArrayContains { left, right }
        | TypedExprKind::ArrayContainedBy { left, right }
        | TypedExprKind::ArrayOverlap { left, right }
        | TypedExprKind::IsDistinctFrom { left, right, .. }
        | TypedExprKind::Nullif { left, right } => {
            collect_dml_expr_local_column_ordinals(left, out);
            collect_dml_expr_local_column_ordinals(right, out);
        }
        TypedExprKind::LogicalNot { expr }
        | TypedExprKind::Negate { expr }
        | TypedExprKind::IsNull { expr, .. }
        | TypedExprKind::Cast { expr, .. } => {
            collect_dml_expr_local_column_ordinals(expr, out);
        }
        TypedExprKind::Like { expr, pattern, .. } => {
            collect_dml_expr_local_column_ordinals(expr, out);
            collect_dml_expr_local_column_ordinals(pattern, out);
        }
        TypedExprKind::InList { expr, list, .. } => {
            collect_dml_expr_local_column_ordinals(expr, out);
            for item in list {
                collect_dml_expr_local_column_ordinals(item, out);
            }
        }
        TypedExprKind::Between {
            expr, low, high, ..
        } => {
            collect_dml_expr_local_column_ordinals(expr, out);
            collect_dml_expr_local_column_ordinals(low, out);
            collect_dml_expr_local_column_ordinals(high, out);
        }
        TypedExprKind::CaseWhen {
            conditions,
            results,
            else_result,
        } => {
            for c in conditions {
                collect_dml_expr_local_column_ordinals(c, out);
            }
            for r in results {
                collect_dml_expr_local_column_ordinals(r, out);
            }
            if let Some(e) = else_result.as_deref() {
                collect_dml_expr_local_column_ordinals(e, out);
            }
        }
        TypedExprKind::Coalesce { args }
        | TypedExprKind::ScalarFunction { args, .. }
        | TypedExprKind::ArrayConstruct { elements: args }
        | TypedExprKind::UserFunction { args, .. } => {
            for a in args {
                collect_dml_expr_local_column_ordinals(a, out);
            }
        }
        // Aggregates / window / subqueries: be conservative and treat
        // them as touching every column. They should not appear in
        // CHECK constraint expressions, but the worst-case branch
        // keeps the check active rather than silently skipping it.
        _ => {
            out.insert(usize::MAX);
        }
    }
}

/// Returns the ordinals referenced by `expr` so the UPDATE handler can
/// decide whether the constraint needs revalidation.
pub(super) fn dml_expr_local_column_ordinals(expr: &TypedExpr) -> std::collections::HashSet<usize> {
    let mut out = std::collections::HashSet::new();
    collect_dml_expr_local_column_ordinals(expr, &mut out);
    out
}
