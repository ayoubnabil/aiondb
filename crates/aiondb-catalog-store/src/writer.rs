mod alter;
mod tenant;

use std::collections::BTreeSet;

use aiondb_catalog::{
    CastDescriptor, CatalogWriter, ColumnDescriptor, CommentDescriptor, DomainDescriptor,
    EdgeLabelDescriptor, FunctionDescriptor, IndexAlteration, IndexDescriptor, NodeLabelDescriptor,
    PolicyDescriptor, PrivilegeDescriptor, PrivilegeTarget, QualifiedName, RoleDescriptor,
    RuleDescriptor, SchemaDescriptor, SequenceAlteration, SequenceDescriptor, TableAlteration,
    TableDescriptor, TableStatistics, TenantDescriptor, TriggerDescriptor, UserTypeDescriptor,
    ViewDescriptor, MAX_CATALOG_HASH_RING_VIRTUAL_NODES, MAX_CATALOG_SHARD_COUNT,
    MAX_CATALOG_VIRTUAL_NODES_PER_SHARD,
};
use aiondb_core::{
    ColumnId, DbError, DbResult, ErrorReport, IndexId, RelationId, SchemaId, SequenceId, SqlState,
    TxnId,
};

use crate::{
    catalog_wal, duplicate_schema, invalid_sequence_ownership, undefined_index_id, undefined_role,
    undefined_table_id, unique_violation, CatalogStore, CatalogTxnChange, DroppedSequenceState,
    DroppedTableState,
};

fn table_has_column(table: &TableDescriptor, column_name: &str) -> bool {
    table
        .columns
        .iter()
        .any(|column| column.name.eq_ignore_ascii_case(column_name))
}

fn table_display_name(table: &TableDescriptor) -> String {
    table.name.to_string()
}

fn validate_table_shard_config(table: &TableDescriptor) -> DbResult<()> {
    let Some(shard_config) = &table.shard_config else {
        return Ok(());
    };

    if shard_config.shard_key_columns.is_empty() {
        return Err(DbError::bind_error(
            SqlState::InvalidParameterValue,
            format!(
                "shard config on table {} must specify at least one shard key column",
                table_display_name(table)
            ),
        ));
    }
    if shard_config.shard_count == 0 {
        return Err(DbError::bind_error(
            SqlState::InvalidParameterValue,
            format!(
                "shard_count must be >= 1 for table {}",
                table_display_name(table)
            ),
        ));
    }
    if shard_config.shard_count > MAX_CATALOG_SHARD_COUNT {
        return Err(DbError::bind_error(
            SqlState::InvalidParameterValue,
            format!(
                "shard_count must be <= {MAX_CATALOG_SHARD_COUNT} for table {}",
                table_display_name(table)
            ),
        ));
    }
    if shard_config.virtual_nodes_per_shard == 0 {
        return Err(DbError::bind_error(
            SqlState::InvalidParameterValue,
            format!(
                "virtual_nodes_per_shard must be >= 1 for table {}",
                table_display_name(table)
            ),
        ));
    }
    if shard_config.virtual_nodes_per_shard > MAX_CATALOG_VIRTUAL_NODES_PER_SHARD {
        return Err(DbError::bind_error(
            SqlState::InvalidParameterValue,
            format!(
                "virtual_nodes_per_shard must be <= {MAX_CATALOG_VIRTUAL_NODES_PER_SHARD} for table {}",
                table_display_name(table)
            ),
        ));
    }
    let total_virtual_nodes =
        u64::from(shard_config.shard_count) * u64::from(shard_config.virtual_nodes_per_shard);
    if total_virtual_nodes > MAX_CATALOG_HASH_RING_VIRTUAL_NODES {
        return Err(DbError::bind_error(
            SqlState::InvalidParameterValue,
            format!(
                "shard hash ring would contain {total_virtual_nodes} virtual nodes, exceeding {MAX_CATALOG_HASH_RING_VIRTUAL_NODES} for table {}",
                table_display_name(table)
            ),
        ));
    }

    let mut seen_shard_keys = BTreeSet::new();
    for shard_key in &shard_config.shard_key_columns {
        let normalized = CatalogStore::normalize_identifier(shard_key);
        if !seen_shard_keys.insert(normalized) {
            return Err(DbError::bind_error(
                SqlState::InvalidParameterValue,
                format!(
                    "duplicate shard key column \"{shard_key}\" in table {}",
                    table_display_name(table)
                ),
            ));
        }
        if !table_has_column(table, shard_key) {
            return Err(DbError::bind_error(
                SqlState::UndefinedColumn,
                format!(
                    "shard key column \"{shard_key}\" does not exist in table {}",
                    table_display_name(table)
                ),
            ));
        }
    }

    Ok(())
}

impl CatalogWriter for CatalogStore {
    fn create_schema(
        &self,
        txn: TxnId,
        mut schema: aiondb_catalog::SchemaDescriptor,
    ) -> DbResult<SchemaId> {
        let schema_name = schema.name.clone();
        if !CatalogStore::is_autocommit_txn(txn) {
            schema.schema_id = self.reserve_schema_id(schema.schema_id)?;
        }

        let schema_id = self.write_catalog_state_with_record(
            txn,
            true,
            CatalogTxnChange::ComplexWrite,
            |state| {
                let lookup_name = CatalogStore::normalize_identifier(&schema.name);
                if state.schema_names.contains_key(&lookup_name) {
                    return Err(duplicate_schema(&schema.name));
                }

                let schema_id = CatalogStore::next_schema_id(state, schema.schema_id);
                schema.schema_id = schema_id;
                state.schemas_by_id.insert(schema_id, schema.clone());
                state.schema_names.insert(lookup_name, schema_id);
                Ok(schema_id)
            },
            |schema_id| {
                catalog_wal::create_schema_record(
                    txn,
                    &SchemaDescriptor {
                        schema_id: *schema_id,
                        name: schema_name.clone(),
                    },
                )
            },
        )?;
        Ok(schema_id)
    }

    fn drop_schema(&self, txn: TxnId, schema_id: SchemaId) -> DbResult<()> {
        self.write_catalog_state_with_record(
            txn,
            true,
            CatalogTxnChange::ComplexWrite,
            |state| {
                let schema = state
                    .schemas_by_id
                    .get(&schema_id)
                    .cloned()
                    .ok_or_else(|| {
                        DbError::bind_error(
                            SqlState::InvalidSchemaName,
                            format!("schema id {} does not exist", schema_id.get()),
                        )
                    })?;

                let has_tables = state
                    .tables_by_id
                    .values()
                    .any(|table| table.schema_id == schema_id);
                let has_sequences = state
                    .sequences_by_id
                    .values()
                    .any(|sequence| sequence.schema_id == schema_id);
                let has_views = state
                    .views_by_id
                    .values()
                    .any(|view| view.schema_id == schema_id);

                if has_tables || has_sequences || has_views {
                    return Err(DbError::bind_error(
                        SqlState::DependentObjectsStillExist,
                        format!(
                            "cannot drop schema \"{}\" because it is not empty",
                            schema.name
                        ),
                    ));
                }

                CatalogStore::remove_privileges_for_dropped_schema(state, &schema.name);
                state.schemas_by_id.remove(&schema_id);
                state
                    .schema_names
                    .remove(&CatalogStore::normalize_identifier(&schema.name));
                Ok(())
            },
            |()| Ok(catalog_wal::drop_schema_record(txn, schema_id)),
        )
    }

    fn create_table(&self, txn: TxnId, mut table: TableDescriptor) -> DbResult<RelationId> {
        if !CatalogStore::is_autocommit_txn(txn) {
            table.table_id = self.reserve_table_id(table.table_id)?;
            for column in &mut table.columns {
                column.column_id = self.reserve_column_id(column.column_id)?;
            }
        }

        let table_id = self.write_catalog_state_with_record(
            txn,
            true,
            CatalogTxnChange::CreateTable(table.table_id),
            |state| {
                let schema_id = if table.schema_id.get() != 0 {
                    CatalogStore::ensure_schema_exists(state, table.schema_id)?;
                    table.schema_id
                } else {
                    CatalogStore::resolve_schema_id(state, &table.name)?
                };
                let schema_name = CatalogStore::schema_name_by_id(state, schema_id)?;
                let lookup_name = CatalogStore::object_lookup_name(&table.name);
                let name_key = (schema_id, lookup_name);
                if state.table_names.contains_key(&name_key) {
                    return Err(unique_violation(format!(
                        "relation \"{}\" already exists",
                        table.name.name
                    )));
                }

                let table_id = CatalogStore::next_table_id(state, table.table_id);
                let mut seen_columns = BTreeSet::new();
                for (index, column) in table.columns.iter_mut().enumerate() {
                    let normalized = CatalogStore::normalize_identifier(&column.name);
                    if !seen_columns.insert(normalized) {
                        return Err(unique_violation(format!(
                            "column \"{}\" already exists in table {}.{}",
                            column.name, schema_name, table.name.name
                        )));
                    }

                    column.column_id = CatalogStore::next_column_id(state, column.column_id);
                    if column.ordinal_position == 0 {
                        column.ordinal_position = u32::try_from(index + 1)
                            .map_err(|_| DbError::internal("column ordinal exceeds u32"))?;
                    }
                }

                validate_table_shard_config(&table)?;

                // Remap primary key column IDs from provisional ordinals
                // to the real IDs just assigned by next_column_id().
                if let Some(ref mut pk_ids) = table.primary_key {
                    for pk_id in pk_ids.iter_mut() {
                        // The executor stores ordinal_position as the
                        // provisional PK column ID. Find the column whose
                        // ordinal_position matches and use its real ID.
                        if let Some(col) = table
                            .columns
                            .iter()
                            .find(|c| ColumnId::new(u64::from(c.ordinal_position)) == *pk_id)
                        {
                            *pk_id = col.column_id;
                        }
                    }
                }

                table.table_id = table_id;
                table.schema_id = schema_id;
                table.name = QualifiedName::qualified(schema_name, table.name.name);
                state.table_names.insert(name_key, table_id);
                state.tables_by_id.insert(table_id, table.clone());
                Ok((table_id, table))
            },
            |table_id| catalog_wal::set_table_descriptor_record(txn, &table_id.1),
        )?;
        Ok(table_id.0)
    }

    fn set_table_type_name(
        &self,
        txn: TxnId,
        table_id: RelationId,
        type_name: Option<String>,
    ) -> DbResult<()> {
        self.write_catalog_state(txn, true, CatalogTxnChange::ComplexWrite, |state| {
            if !state.tables_by_id.contains_key(&table_id) {
                return Err(undefined_table_id(table_id));
            }
            match type_name {
                Some(ref value) => {
                    state
                        .typed_table_types_by_id
                        .insert(table_id, value.clone());
                }
                None => {
                    state.typed_table_types_by_id.remove(&table_id);
                }
            }
            Ok(())
        })
    }

    fn create_index(&self, txn: TxnId, mut index: IndexDescriptor) -> DbResult<IndexId> {
        if !CatalogStore::is_autocommit_txn(txn) {
            index.index_id = self.reserve_index_id(index.index_id)?;
        }

        let (index_id, _) = self.write_catalog_state_with_record(
            txn,
            true,
            CatalogTxnChange::CreateIndex(index.index_id),
            |state| {
                let table = state
                    .tables_by_id
                    .get(&index.table_id)
                    .ok_or_else(|| undefined_table_id(index.table_id))?;
                let table_schema_id = table.schema_id;
                let table_id = table.table_id;

                let schema_id = if index.schema_id.get() != 0 {
                    CatalogStore::ensure_schema_exists(state, index.schema_id)?;
                    index.schema_id
                } else {
                    table_schema_id
                };
                let schema_name = CatalogStore::schema_name_by_id(state, schema_id)?;
                let lookup_name = CatalogStore::object_lookup_name(&index.name);
                let name_key = (schema_id, lookup_name);
                if state.index_names.contains_key(&name_key) {
                    return Err(unique_violation(format!(
                        "index \"{}.{}\" already exists",
                        schema_name, index.name.name
                    )));
                }

                let index_id = CatalogStore::next_index_id(state, index.index_id);
                index.index_id = index_id;
                index.schema_id = schema_id;
                index.name = QualifiedName::qualified(schema_name, index.name.name);
                state.index_names.insert(name_key, index_id);
                state.indexes_by_id.insert(index_id, index.clone());
                state
                    .indexes_by_table
                    .entry(table_id)
                    .or_default()
                    .push(index_id);
                Ok((index_id, index))
            },
            |(_, final_index)| catalog_wal::set_index_descriptor_record(txn, final_index),
        )?;
        Ok(index_id)
    }

    fn create_sequence(
        &self,
        txn: TxnId,
        mut sequence: SequenceDescriptor,
    ) -> DbResult<SequenceId> {
        if !CatalogStore::is_autocommit_txn(txn) {
            sequence.sequence_id = self.reserve_sequence_id(sequence.sequence_id)?;
        }

        let (seq_id, _) = self.write_catalog_state_with_record(
            txn,
            true,
            CatalogTxnChange::CreateSequence(sequence.sequence_id),
            |state| {
                let schema_id = if sequence.schema_id.get() != 0 {
                    CatalogStore::ensure_schema_exists(state, sequence.schema_id)?;
                    sequence.schema_id
                } else {
                    CatalogStore::resolve_schema_id(state, &sequence.name)?
                };
                let schema_name = CatalogStore::schema_name_by_id(state, schema_id)?;
                let lookup_name = CatalogStore::object_lookup_name(&sequence.name);
                let name_key = (schema_id, lookup_name);
                if state.sequence_names.contains_key(&name_key) {
                    return Err(unique_violation(format!(
                        "sequence \"{}.{}\" already exists",
                        schema_name, sequence.name.name
                    )));
                }

                let sequence_id = CatalogStore::next_sequence_id(state, sequence.sequence_id);
                sequence.sequence_id = sequence_id;
                sequence.schema_id = schema_id;
                sequence.name = QualifiedName::qualified(schema_name, sequence.name.name);
                state.sequence_names.insert(name_key, sequence_id);
                state
                    .sequence_values
                    .insert(sequence_id, CatalogStore::default_sequence_state(&sequence));
                state.sequences_by_id.insert(sequence_id, sequence.clone());
                Ok((sequence_id, sequence))
            },
            |(_, final_seq)| catalog_wal::create_sequence_record(txn, final_seq),
        )?;
        Ok(seq_id)
    }

    fn alter_table(
        &self,
        txn: TxnId,
        table_id: RelationId,
        alteration: TableAlteration,
    ) -> DbResult<()> {
        let alteration = if CatalogStore::is_autocommit_txn(txn) {
            alteration
        } else {
            match alteration {
                TableAlteration::AddColumn(mut column) => {
                    column.column_id = self.reserve_column_id(column.column_id)?;
                    TableAlteration::AddColumn(column)
                }
                other => other,
            }
        };

        self.write_catalog_state_with_record(
            txn,
            true,
            CatalogTxnChange::ComplexWrite,
            |state| {
                let mut table = state
                    .tables_by_id
                    .remove(&table_id)
                    .ok_or_else(|| undefined_table_id(table_id))?;
                let previous_key = (
                    table.schema_id,
                    CatalogStore::normalize_identifier(&table.name.name),
                );
                alter::apply_alteration(state, table_id, &mut table, &previous_key, alteration)?;
                state.tables_by_id.insert(table_id, table.clone());
                Ok(table)
            },
            |final_table| catalog_wal::set_table_descriptor_record(txn, final_table),
        )?;
        Ok(())
    }

    fn alter_index(
        &self,
        txn: TxnId,
        index_id: IndexId,
        alteration: IndexAlteration,
    ) -> DbResult<()> {
        self.write_catalog_state_with_record(
            txn,
            true,
            CatalogTxnChange::ComplexWrite,
            |state| {
                let mut index = state
                    .indexes_by_id
                    .remove(&index_id)
                    .ok_or_else(|| undefined_index_id(index_id))?;
                let previous_key = (
                    index.schema_id,
                    CatalogStore::normalize_identifier(&index.name.name),
                );

                match alteration {
                    IndexAlteration::Rename { new_name } => {
                        let target_schema_id = if new_name.schema_name().is_some() {
                            CatalogStore::resolve_schema_id(state, &new_name)?
                        } else {
                            index.schema_id
                        };
                        let target_schema_name =
                            CatalogStore::schema_name_by_id(state, target_schema_id)?;
                        let target_key = (
                            target_schema_id,
                            CatalogStore::normalize_identifier(&new_name.name),
                        );
                        if target_key != previous_key && state.index_names.contains_key(&target_key)
                        {
                            state.indexes_by_id.insert(index_id, index);
                            return Err(unique_violation(format!(
                                "index \"{}.{}\" already exists",
                                target_schema_name, new_name.name
                            )));
                        }
                        index.schema_id = target_schema_id;
                        index.name = QualifiedName::qualified(target_schema_name, new_name.name);
                        state.index_names.remove(&previous_key);
                        state.index_names.insert(target_key, index_id);
                    }
                }

                state.indexes_by_id.insert(index_id, index.clone());
                Ok(index)
            },
            |final_index| catalog_wal::set_index_descriptor_record(txn, final_index),
        )?;
        Ok(())
    }

    fn alter_sequence(
        &self,
        txn: TxnId,
        sequence_id: SequenceId,
        alteration: SequenceAlteration,
    ) -> DbResult<()> {
        self.write_catalog_state_with_record(
            txn,
            true,
            CatalogTxnChange::ComplexWrite,
            |state| {
                let mut sequence = state
                    .sequences_by_id
                    .remove(&sequence_id)
                    .ok_or_else(|| crate::undefined_sequence_id(sequence_id))?;
                let previous_key = (
                    sequence.schema_id,
                    CatalogStore::normalize_identifier(&sequence.name.name),
                );

                match alteration {
                    SequenceAlteration::Rename { new_name } => {
                        let target_schema_id = if new_name.schema_name().is_some() {
                            CatalogStore::resolve_schema_id(state, &new_name)?
                        } else {
                            sequence.schema_id
                        };
                        let target_schema_name =
                            CatalogStore::schema_name_by_id(state, target_schema_id)?;
                        let target_key = (
                            target_schema_id,
                            CatalogStore::normalize_identifier(&new_name.name),
                        );
                        if target_key != previous_key
                            && state.sequence_names.contains_key(&target_key)
                        {
                            state.sequences_by_id.insert(sequence_id, sequence);
                            return Err(unique_violation(format!(
                                "sequence \"{}.{}\" already exists",
                                target_schema_name, new_name.name
                            )));
                        }
                        sequence.schema_id = target_schema_id;
                        sequence.name = QualifiedName::qualified(target_schema_name, new_name.name);
                        state.sequence_names.remove(&previous_key);
                        state.sequence_names.insert(target_key, sequence_id);
                    }
                    SequenceAlteration::RestartWith { value } => {
                        sequence.start_value = value;
                        let runtime = state
                            .sequence_values
                            .get_mut(&sequence_id)
                            .ok_or_else(|| crate::undefined_sequence_id(sequence_id))?;
                        runtime.current_value = value;
                        runtime.is_called = false;
                    }
                    SequenceAlteration::SetOwnedBy {
                        table_id,
                        column_id,
                    } => match (table_id, column_id) {
                        (Some(table_id), Some(column_id)) => {
                            let (has_column, table_name) = {
                                let table = state
                                    .tables_by_id
                                    .get(&table_id)
                                    .ok_or_else(|| undefined_table_id(table_id))?;
                                (
                                    table
                                        .columns
                                        .iter()
                                        .any(|column| column.column_id == column_id),
                                    table.name.clone(),
                                )
                            };
                            if !has_column {
                                state.sequences_by_id.insert(sequence_id, sequence);
                                return Err(DbError::Bind(Box::new(ErrorReport::new(
                                    SqlState::UndefinedColumn,
                                    format!(
                                        "column id {} does not exist in table {}",
                                        column_id.get(),
                                        table_name
                                    ),
                                ))));
                            }
                            sequence.owned_by = Some((table_id, column_id));
                        }
                        (None, None) => sequence.owned_by = None,
                        _ => {
                            state.sequences_by_id.insert(sequence_id, sequence);
                            return Err(invalid_sequence_ownership());
                        }
                    },
                }

                state.sequences_by_id.insert(sequence_id, sequence.clone());
                Ok(sequence)
            },
            |final_seq| catalog_wal::alter_sequence_record(txn, final_seq),
        )?;
        Ok(())
    }

    fn drop_table(&self, txn: TxnId, table_id: RelationId) -> DbResult<()> {
        let dropped_table = self.read_catalog_state(txn, |state| {
            let descriptor = state
                .tables_by_id
                .get(&table_id)
                .cloned()
                .ok_or_else(|| undefined_table_id(table_id))?;
            let statistics = state.statistics.get(&table_id).cloned();
            Ok(DroppedTableState {
                descriptor,
                statistics,
            })
        })?;

        self.write_catalog_state_with_record(
            txn,
            true,
            CatalogTxnChange::DropTable(dropped_table),
            |state| {
                let table = state
                    .tables_by_id
                    .remove(&table_id)
                    .ok_or_else(|| undefined_table_id(table_id))?;
                CatalogStore::remove_privileges_for_dropped_relation(state, &table.name);
                state.typed_table_types_by_id.remove(&table_id);
                let table_key = (
                    table.schema_id,
                    CatalogStore::normalize_identifier(&table.name.name),
                );
                state.table_names.remove(&table_key);
                state.statistics.remove(&table_id);

                if let Some(index_ids) = state.indexes_by_table.remove(&table_id) {
                    for index_id in index_ids {
                        if let Some(index) = state.indexes_by_id.remove(&index_id) {
                            let key = (
                                index.schema_id,
                                CatalogStore::normalize_identifier(&index.name.name),
                            );
                            state.index_names.remove(&key);
                        }
                    }
                }

                // Cascade-delete triggers associated with the dropped table.
                // Triggers can be stored with either the bare or qualified
                // form ("trigtest" vs "public.trigtest") depending on the call
                // site that created them, so rely on the lookup-key matcher
                // rather than a strict string compare. Otherwise stale
                // triggers survive a DROP TABLE / CREATE TABLE pair and a
                // recreate of the same trigger name fails with "already
                // exists" - the bug exposed by the regress `triggers` suite.
                let table_name_for_match = table.name.name.clone();
                state.triggers.retain(|t| {
                    !CatalogStore::trigger_table_matches(&t.table_name, &table_name_for_match)
                });

                // Cascade to sequences owned by columns of the dropped table.
                let owned_seq_ids: Vec<SequenceId> = state
                    .sequences_by_id
                    .iter()
                    .filter(|(_, seq)| {
                        seq.owned_by
                            .as_ref()
                            .is_some_and(|(tid, _)| *tid == table_id)
                    })
                    .map(|(sid, _)| *sid)
                    .collect();
                for seq_id in owned_seq_ids {
                    if let Some(seq) = state.sequences_by_id.remove(&seq_id) {
                        CatalogStore::remove_privileges_for_dropped_relation(state, &seq.name);
                        let key = (
                            seq.schema_id,
                            CatalogStore::normalize_identifier(&seq.name.name),
                        );
                        state.sequence_names.remove(&key);
                        state.sequence_values.remove(&seq_id);
                    }
                }

                Ok(())
            },
            |()| Ok(catalog_wal::drop_table_record(txn, table_id)),
        )
    }

    fn drop_index(&self, txn: TxnId, index_id: IndexId) -> DbResult<()> {
        let dropped_index = self.read_catalog_state(txn, |state| {
            state
                .indexes_by_id
                .get(&index_id)
                .cloned()
                .ok_or_else(|| undefined_index_id(index_id))
        })?;

        self.write_catalog_state_with_record(
            txn,
            true,
            CatalogTxnChange::DropIndex(dropped_index),
            |state| {
                let index = state
                    .indexes_by_id
                    .remove(&index_id)
                    .ok_or_else(|| undefined_index_id(index_id))?;
                let key = (
                    index.schema_id,
                    CatalogStore::normalize_identifier(&index.name.name),
                );
                state.index_names.remove(&key);
                let mut remove_table_entry = false;
                if let Some(indexes) = state.indexes_by_table.get_mut(&index.table_id) {
                    indexes.retain(|existing| *existing != index_id);
                    remove_table_entry = indexes.is_empty();
                }
                if remove_table_entry {
                    state.indexes_by_table.remove(&index.table_id);
                }
                Ok(())
            },
            |()| Ok(catalog_wal::drop_index_record(txn, index_id)),
        )
    }

    fn drop_sequence(&self, txn: TxnId, sequence_id: SequenceId) -> DbResult<()> {
        let dropped_sequence = self.read_catalog_state(txn, |state| {
            let descriptor = state
                .sequences_by_id
                .get(&sequence_id)
                .cloned()
                .ok_or_else(|| crate::undefined_sequence_id(sequence_id))?;
            let runtime = state
                .sequence_values
                .get(&sequence_id)
                .copied()
                .ok_or_else(|| crate::undefined_sequence_id(sequence_id))?;
            Ok(DroppedSequenceState {
                descriptor,
                runtime,
            })
        })?;

        self.write_catalog_state_with_record(
            txn,
            true,
            CatalogTxnChange::DropSequence(dropped_sequence),
            |state| {
                let sequence = state
                    .sequences_by_id
                    .remove(&sequence_id)
                    .ok_or_else(|| crate::undefined_sequence_id(sequence_id))?;
                CatalogStore::remove_privileges_for_dropped_relation(state, &sequence.name);
                let key = (
                    sequence.schema_id,
                    CatalogStore::normalize_identifier(&sequence.name.name),
                );
                state.sequence_names.remove(&key);
                state.sequence_values.remove(&sequence_id);
                Ok(())
            },
            |()| Ok(catalog_wal::drop_sequence_record(txn, sequence_id)),
        )
    }

    fn create_view(&self, txn: TxnId, mut view: ViewDescriptor) -> DbResult<RelationId> {
        if !CatalogStore::is_autocommit_txn(txn) {
            view.view_id = self.reserve_table_id(view.view_id)?;
        }

        let (view_id, _) = self.write_catalog_state_with_record(
            txn,
            true,
            CatalogTxnChange::ComplexWrite,
            |state| {
                let schema_id = if view.schema_id.get() != 0 {
                    CatalogStore::ensure_schema_exists(state, view.schema_id)?;
                    view.schema_id
                } else {
                    CatalogStore::resolve_schema_id(state, &view.name)?
                };
                let schema_name = CatalogStore::schema_name_by_id(state, schema_id)?;
                let lookup_name = CatalogStore::object_lookup_name(&view.name);
                let name_key = (schema_id, lookup_name);
                if state.view_names.contains_key(&name_key) {
                    return Err(unique_violation(format!(
                        "relation \"{}\" already exists",
                        view.name.name
                    )));
                }

                let view_id = CatalogStore::next_table_id(state, view.view_id);
                view.view_id = view_id;
                view.schema_id = schema_id;
                view.name = QualifiedName::qualified(schema_name, view.name.name);
                state.view_names.insert(name_key, view_id);
                state.views_by_id.insert(view_id, view.clone());
                Ok((view_id, view))
            },
            |(_, final_view)| catalog_wal::create_view_record(txn, final_view),
        )?;
        Ok(view_id)
    }

    fn drop_view(&self, txn: TxnId, view_id: RelationId) -> DbResult<()> {
        self.write_catalog_state_with_record(
            txn,
            true,
            CatalogTxnChange::ComplexWrite,
            |state| {
                let view = state.views_by_id.remove(&view_id).ok_or_else(|| {
                    DbError::bind_error(
                        SqlState::UndefinedObject,
                        format!("view id {} does not exist", view_id.get()),
                    )
                })?;
                CatalogStore::remove_privileges_for_dropped_relation(state, &view.name);
                let view_key = (
                    view.schema_id,
                    CatalogStore::normalize_identifier(&view.name.name),
                );
                state.view_names.remove(&view_key);
                Ok(())
            },
            |()| Ok(catalog_wal::drop_view_record(txn, view_id)),
        )
    }

    fn update_statistics(&self, txn: TxnId, stats: TableStatistics) -> DbResult<()> {
        let stats_for_state = stats.clone();
        self.write_catalog_state_with_record(
            txn,
            true,
            CatalogTxnChange::ComplexWrite,
            |state| {
                if !state.tables_by_id.contains_key(&stats_for_state.table_id) {
                    return Err(undefined_table_id(stats_for_state.table_id));
                }
                // Reject the update when an existing snapshot was written
                // by a *newer* txn - concurrent ANALYZE that started later
                // already published fresher numbers and the older one
                // would otherwise roll them back. Match by `txn` ordering
                // since `last_updated_by` is the source of truth here.
                if let Some(existing) = state.statistics.get(&stats_for_state.table_id) {
                    if let (Some(prev), Some(incoming)) =
                        (existing.last_updated_by, stats_for_state.last_updated_by)
                    {
                        if prev.get() > incoming.get() {
                            return Ok(());
                        }
                    }
                }
                state
                    .statistics
                    .insert(stats_for_state.table_id, stats_for_state.clone());
                Ok(())
            },
            |()| catalog_wal::update_statistics_record(txn, &stats),
        )
    }

    fn create_node_label(&self, txn: TxnId, label: NodeLabelDescriptor) -> DbResult<()> {
        let label_for_state = label.clone();
        self.write_catalog_state_with_record(
            txn,
            true,
            CatalogTxnChange::ComplexWrite,
            |state| {
                let table = state
                    .tables_by_id
                    .get(&label_for_state.table_id)
                    .ok_or_else(|| undefined_table_id(label_for_state.table_id))?;
                if !matches!(table.columns.first(), Some(column) if column.name.eq_ignore_ascii_case("id"))
                {
                    return Err(DbError::bind_error(
                        SqlState::InvalidTableDefinition,
                        format!(
                            "node label \"{}\" requires backing table \"{}\" to expose \"id\" as its first column",
                            label_for_state.label,
                            table_display_name(table),
                        ),
                    ));
                }
                let lookup = CatalogStore::normalize_identifier(&label_for_state.label);
                if state.node_labels.contains_key(&lookup) {
                    return Err(unique_violation(format!(
                        "node label \"{}\" already exists",
                        label_for_state.label
                    )));
                }
                if let Some(existing) = state
                    .node_labels
                    .values()
                    .find(|existing| existing.table_id == label_for_state.table_id)
                {
                    return Err(DbError::bind_error(
                        SqlState::DuplicateObject,
                        format!(
                            "table \"{}\" is already registered as node label \"{}\"",
                            table_display_name(table),
                            existing.label,
                        ),
                    ));
                }
                if let Some(existing) = state
                    .edge_labels
                    .values()
                    .find(|existing| existing.table_id == label_for_state.table_id)
                {
                    if existing.endpoints.is_none() {
                        return Err(DbError::bind_error(
                            SqlState::WrongObjectType,
                            format!(
                                "table \"{}\" is already registered as previous edge label \"{}\" and cannot back node label \"{}\"",
                                table_display_name(table),
                                existing.label,
                                label_for_state.label,
                            ),
                        ));
                    }
                }
                state.node_labels.insert(lookup, label_for_state.clone());
                Ok(())
            },
            |()| catalog_wal::create_node_label_record(txn, &label),
        )
    }

    fn create_edge_label(&self, txn: TxnId, label: EdgeLabelDescriptor) -> DbResult<()> {
        let label_for_state = label.clone();
        self.write_catalog_state_with_record(
            txn,
            true,
            CatalogTxnChange::ComplexWrite,
            |state| {
                let source_lookup =
                    CatalogStore::normalize_identifier(&label_for_state.source_label);
                if !state.node_labels.contains_key(&source_lookup) {
                    return Err(DbError::bind_error(
                        SqlState::UndefinedObject,
                        format!(
                            "source node label \"{}\" does not exist",
                            label_for_state.source_label
                        ),
                    ));
                }
                let target_lookup =
                    CatalogStore::normalize_identifier(&label_for_state.target_label);
                if !state.node_labels.contains_key(&target_lookup) {
                    return Err(DbError::bind_error(
                        SqlState::UndefinedObject,
                        format!(
                            "target node label \"{}\" does not exist",
                            label_for_state.target_label
                        ),
                    ));
                }
                let table = state
                    .tables_by_id
                    .get(&label_for_state.table_id)
                    .ok_or_else(|| undefined_table_id(label_for_state.table_id))?;
                let (source_column, target_column) =
                    label_for_state.endpoints.as_ref().map_or(
                        ("source_id", "target_id"),
                        |endpoints| {
                            (
                                endpoints.source_id_column.as_str(),
                                endpoints.target_id_column.as_str(),
                            )
                        },
                    );
                if !table_has_column(table, source_column) || !table_has_column(table, target_column)
                {
                    return Err(DbError::bind_error(
                        SqlState::InvalidTableDefinition,
                        format!(
                            "edge label \"{}\" requires backing table \"{}\" to expose both \"{}\" and \"{}\" columns",
                            label_for_state.label,
                            table_display_name(table),
                            source_column,
                            target_column,
                        ),
                    ));
                }
                let lookup = CatalogStore::normalize_identifier(&label_for_state.label);
                if state.edge_labels.contains_key(&lookup) {
                    return Err(unique_violation(format!(
                        "edge label \"{}\" already exists",
                        label_for_state.label
                    )));
                }
                if let Some(existing) = state
                    .edge_labels
                    .values()
                    .find(|existing| existing.table_id == label_for_state.table_id)
                {
                    if existing.endpoints.is_none() || label_for_state.endpoints.is_none() {
                        return Err(DbError::bind_error(
                            SqlState::DuplicateObject,
                            format!(
                                "table \"{}\" is already registered as edge label \"{}\" and can only be shared by FK-backed edge labels with explicit endpoint KEY columns",
                                table_display_name(table),
                                existing.label,
                            ),
                        ));
                    }
                }
                if let Some(existing) = state
                    .node_labels
                    .values()
                    .find(|existing| existing.table_id == label_for_state.table_id)
                {
                    if label_for_state.endpoints.is_none() {
                        return Err(DbError::bind_error(
                            SqlState::WrongObjectType,
                            format!(
                                "table \"{}\" is already registered as node label \"{}\" and cannot back edge label \"{}\" without explicit endpoint KEY columns",
                                table_display_name(table),
                                existing.label,
                                label_for_state.label,
                            ),
                        ));
                    }
                }
                state.edge_labels.insert(lookup, label_for_state.clone());
                Ok(())
            },
            |()| catalog_wal::create_edge_label_record(txn, &label),
        )
    }

    fn drop_node_label(&self, txn: TxnId, label: &str) -> DbResult<()> {
        self.write_catalog_state_with_record(
            txn,
            true,
            CatalogTxnChange::ComplexWrite,
            |state| {
                let lookup = CatalogStore::normalize_identifier(label);
                if !state.node_labels.contains_key(&lookup) {
                    return Err(DbError::bind_error(
                        SqlState::UndefinedObject,
                        format!("node label \"{label}\" does not exist"),
                    ));
                }
                // Check for dependent edge labels
                for edge in state.edge_labels.values() {
                    let src = CatalogStore::normalize_identifier(&edge.source_label);
                    let tgt = CatalogStore::normalize_identifier(&edge.target_label);
                    if src == lookup || tgt == lookup {
                        return Err(DbError::bind_error(
                            SqlState::DependentObjectsStillExist,
                            format!(
                                "cannot drop node label \"{label}\": edge label \"{}\" depends on it",
                                edge.label
                            ),
                        ));
                    }
                }
                state.node_labels.remove(&lookup);
                Ok(())
            },
            |()| Ok(catalog_wal::drop_node_label_record(txn, label)),
        )
    }

    fn drop_edge_label(&self, txn: TxnId, label: &str) -> DbResult<()> {
        self.write_catalog_state_with_record(
            txn,
            true,
            CatalogTxnChange::ComplexWrite,
            |state| {
                let lookup = CatalogStore::normalize_identifier(label);
                if state.edge_labels.remove(&lookup).is_none() {
                    return Err(DbError::bind_error(
                        SqlState::UndefinedObject,
                        format!("edge label \"{label}\" does not exist"),
                    ));
                }
                Ok(())
            },
            |()| Ok(catalog_wal::drop_edge_label_record(txn, label)),
        )
    }

    fn create_role(&self, txn: TxnId, role: RoleDescriptor) -> DbResult<()> {
        self.write_catalog_state_with_record(
            txn,
            true,
            CatalogTxnChange::ComplexWrite,
            |state| {
                let lookup = CatalogStore::normalize_identifier(&role.name);
                if state.roles.contains_key(&lookup) {
                    return Err(unique_violation(format!(
                        "role \"{}\" already exists",
                        role.name
                    )));
                }
                state.roles.insert(lookup, role.clone());
                Ok(role)
            },
            |role| catalog_wal::create_role_record(txn, role),
        )?;
        Ok(())
    }

    fn alter_role(&self, txn: TxnId, name: &str, role: RoleDescriptor) -> DbResult<()> {
        self.write_catalog_state_with_record(
            txn,
            true,
            CatalogTxnChange::ComplexWrite,
            |state| {
                let lookup = CatalogStore::normalize_identifier(name);
                if !state.roles.contains_key(&lookup) {
                    return Err(undefined_role(name));
                }
                state.roles.insert(lookup, role.clone());
                Ok(role)
            },
            |role| catalog_wal::alter_role_record(txn, role),
        )?;
        Ok(())
    }

    fn drop_role(&self, txn: TxnId, name: &str) -> DbResult<()> {
        self.write_catalog_state_with_record(
            txn,
            true,
            CatalogTxnChange::ComplexWrite,
            |state| {
                let lookup = CatalogStore::normalize_identifier(name);
                if !state.roles.contains_key(&lookup) {
                    return Err(undefined_role(name));
                }

                // Drop-role dependency checks should only consider live ACL
                // entries. Purge stale privileges referencing missing roles
                // or dropped objects first.
                let existing_privileges = std::mem::take(&mut state.privileges);
                state.privileges = existing_privileges
                    .into_iter()
                    .filter(|privilege| {
                        let role_lookup = CatalogStore::normalize_identifier(&privilege.role_name);
                        let role_exists =
                            role_lookup == "public" || state.roles.contains_key(&role_lookup);
                        role_exists
                            && CatalogStore::privilege_target_exists(state, &privilege.target)
                    })
                    .collect();

                state.roles.remove(&lookup);
                state.privileges.retain(|privilege| {
                    if CatalogStore::normalize_identifier(&privilege.role_name) == lookup {
                        return false;
                    }
                    !CatalogStore::privilege_target_references_role(&privilege.target, &lookup)
                });
                Ok(())
            },
            |()| Ok(catalog_wal::drop_role_record(txn, name)),
        )
    }

    fn grant_privilege(&self, txn: TxnId, privilege: PrivilegeDescriptor) -> DbResult<()> {
        let privilege = CatalogStore::canonicalize_function_privilege(privilege);
        self.write_catalog_state_with_record(
            txn,
            true,
            CatalogTxnChange::ComplexWrite,
            |state| {
                let role_key = CatalogStore::normalize_identifier(&privilege.role_name);
                // "public" is a built-in pseudo-role in PostgreSQL
                if role_key != "public" && !state.roles.contains_key(&role_key) {
                    return Err(undefined_role(&privilege.role_name));
                }
                // Reject grants on tables that do not exist; otherwise an
                // attacker can pre-position a privilege on a name that gets
                // created later (TOCTOU on object creation), and the grant
                // becomes durable through WAL replay (audit catalog F1).
                // Scoped to Table targets so the legacy function-shape
                // migration path stays untouched.
                if matches!(&privilege.target, PrivilegeTarget::Table(_))
                    && !CatalogStore::privilege_target_exists(state, &privilege.target)
                {
                    return Err(DbError::bind_error(
                        aiondb_core::SqlState::UndefinedObject,
                        format!("GRANT target does not exist: {:?}", privilege.target),
                    ));
                }
                if !state.privileges.contains(&privilege) {
                    state.privileges.push(privilege.clone());
                }
                Ok(privilege)
            },
            |priv_desc| catalog_wal::grant_privilege_record(txn, priv_desc),
        )?;
        Ok(())
    }

    fn revoke_privilege(&self, txn: TxnId, privilege: PrivilegeDescriptor) -> DbResult<()> {
        let privilege = CatalogStore::canonicalize_function_privilege(privilege);
        self.write_catalog_state_with_record(
            txn,
            true,
            CatalogTxnChange::ComplexWrite,
            |state| {
                let role_key = CatalogStore::normalize_identifier(&privilege.role_name);
                if role_key != "public" && !state.roles.contains_key(&role_key) {
                    return Err(undefined_role(&privilege.role_name));
                }
                state
                    .privileges
                    .retain(|p| !CatalogStore::privilege_matches_revoke(p, &privilege));
                Ok(privilege)
            },
            |priv_desc| catalog_wal::revoke_privilege_record(txn, priv_desc),
        )?;
        Ok(())
    }

    fn create_tenant(&self, txn: TxnId, name: &str) -> DbResult<TenantDescriptor> {
        tenant::create_tenant(self, txn, name)
    }

    fn drop_tenant(&self, txn: TxnId, name: &str) -> DbResult<()> {
        tenant::drop_tenant(self, txn, name)
    }

    fn create_function(&self, txn: TxnId, func: FunctionDescriptor) -> DbResult<()> {
        self.write_catalog_state_with_record(
            txn,
            true,
            CatalogTxnChange::ComplexWrite,
            |state| {
                let lookup = CatalogStore::normalize_identifier(&func.name);
                let overloads = state.functions.entry(lookup).or_default();
                if overloads
                    .iter()
                    .any(|existing| CatalogStore::same_function_signature(existing, &func))
                {
                    return Err(unique_violation(format!(
                        "function \"{}\" already exists",
                        func.name
                    )));
                }
                overloads.push(func.clone());
                Ok(func)
            },
            |func| catalog_wal::create_function_record(txn, func),
        )?;
        Ok(())
    }

    fn drop_function(&self, txn: TxnId, name: &str) -> DbResult<()> {
        self.write_catalog_state_with_record(
            txn,
            true,
            CatalogTxnChange::ComplexWrite,
            |state| {
                let lookup = CatalogStore::normalize_identifier(name);
                if state.functions.remove(&lookup).is_none() {
                    return Err(DbError::bind_error(
                        SqlState::UndefinedObject,
                        format!("function \"{name}\" does not exist"),
                    ));
                }
                CatalogStore::remove_privileges_for_dropped_function(state, name);
                Ok(())
            },
            |()| Ok(catalog_wal::drop_function_record(txn, name)),
        )
    }

    fn replace_or_create_function(&self, txn: TxnId, func: FunctionDescriptor) -> DbResult<()> {
        // Single-shot in-place swap so OR REPLACE never enters a window
        // where surviving overloads are temporarily missing from the
        // catalog. The closure runs under the catalog write-lock so any
        // failure leaves no half-applied state.
        self.write_catalog_state_with_record(
            txn,
            true,
            CatalogTxnChange::ComplexWrite,
            |state| {
                let lookup = CatalogStore::normalize_identifier(&func.name);
                let overloads = state.functions.entry(lookup).or_default();
                if let Some(slot) = overloads
                    .iter_mut()
                    .find(|existing| CatalogStore::same_function_signature(existing, &func))
                {
                    *slot = func.clone();
                } else {
                    overloads.push(func.clone());
                }
                Ok(func)
            },
            |func| catalog_wal::create_function_record(txn, func),
        )?;
        Ok(())
    }

    fn drop_function_overload(
        &self,
        txn: TxnId,
        name: &str,
        param_types: &[aiondb_core::DataType],
    ) -> DbResult<bool> {
        // Atomic: scan the overload list and remove only the matching
        // signature under the catalog write-lock, never going through
        // the drop-all + recreate-survivors loop.
        let found =
            self.write_catalog_state(txn, true, CatalogTxnChange::ComplexWrite, |state| {
                let lookup = CatalogStore::normalize_identifier(name);
                let Some(overloads) = state.functions.get_mut(&lookup) else {
                    return Ok(false);
                };
                let target_arity = param_types.len();
                let pos = overloads.iter().position(|existing| {
                    existing.params.len() == target_arity
                        && existing
                            .params
                            .iter()
                            .zip(param_types.iter())
                            .all(|(p, t)| p.data_type == *t)
                });
                let Some(idx) = pos else {
                    return Ok(false);
                };
                overloads.remove(idx);
                if overloads.is_empty() {
                    state.functions.remove(&lookup);
                    CatalogStore::remove_privileges_for_dropped_function(state, name);
                }
                Ok(true)
            })?;
        if found {
            // Buffer the WAL drop record on the pending txn (or eager-log
            // for unregistered txns) - same pathway as `drop_function`.
            let record = catalog_wal::drop_function_record(txn, name);
            let mut active_txns = self.write_active_txns()?;
            if let Some(pending) = active_txns.get_mut(&txn) {
                pending.pending_wal_records.push(record);
            } else {
                drop(active_txns);
                self.log_catalog_record(&record)?;
            }
        }
        Ok(found)
    }

    fn create_domain(&self, txn: TxnId, domain: DomainDescriptor) -> DbResult<()> {
        self.write_catalog_state_with_record(
            txn,
            true,
            CatalogTxnChange::ComplexWrite,
            |state| {
                let lookup = CatalogStore::normalize_identifier(&domain.name);
                if state.domains.contains_key(&lookup) {
                    return Err(unique_violation(format!(
                        "type \"{}\" already exists",
                        domain.name
                    )));
                }
                state.domains.insert(lookup, domain.clone());
                Ok(domain)
            },
            |domain| catalog_wal::create_domain_record(txn, domain),
        )?;
        Ok(())
    }

    fn drop_domain(&self, txn: TxnId, name: &str) -> DbResult<()> {
        self.write_catalog_state_with_record(
            txn,
            true,
            CatalogTxnChange::ComplexWrite,
            |state| {
                let lookup = CatalogStore::normalize_identifier(name);
                if state.domains.remove(&lookup).is_none() {
                    return Err(DbError::bind_error(
                        SqlState::UndefinedObject,
                        format!("type \"{name}\" does not exist"),
                    ));
                }
                Ok(())
            },
            |()| Ok(catalog_wal::drop_domain_record(txn, name)),
        )
    }

    fn alter_domain(&self, txn: TxnId, domain: DomainDescriptor) -> DbResult<()> {
        self.write_catalog_state_with_record(
            txn,
            true,
            CatalogTxnChange::ComplexWrite,
            |state| {
                let lookup = CatalogStore::normalize_identifier(&domain.name);
                if !state.domains.contains_key(&lookup) {
                    return Err(DbError::bind_error(
                        SqlState::UndefinedObject,
                        format!("type \"{}\" does not exist", domain.name),
                    ));
                }
                state.domains.insert(lookup, domain.clone());
                Ok(domain)
            },
            |domain| catalog_wal::alter_domain_record(txn, domain),
        )?;
        Ok(())
    }

    fn create_user_type(&self, txn: TxnId, user_type: UserTypeDescriptor) -> DbResult<()> {
        self.write_catalog_state_with_record(
            txn,
            true,
            CatalogTxnChange::ComplexWrite,
            |state| {
                let lookup = CatalogStore::normalize_identifier(&user_type.name);
                if state.user_types.contains_key(&lookup) {
                    return Err(unique_violation(format!(
                        "type \"{}\" already exists",
                        user_type.name
                    )));
                }
                state.user_types.insert(lookup, user_type.clone());
                Ok(user_type)
            },
            |user_type| catalog_wal::create_user_type_record(txn, user_type),
        )?;
        Ok(())
    }

    fn drop_user_type(&self, txn: TxnId, name: &str) -> DbResult<()> {
        self.write_catalog_state_with_record(
            txn,
            true,
            CatalogTxnChange::ComplexWrite,
            |state| {
                let lookup = CatalogStore::normalize_identifier(name);
                if state.user_types.remove(&lookup).is_none() {
                    return Err(DbError::bind_error(
                        SqlState::UndefinedObject,
                        format!("type \"{name}\" does not exist"),
                    ));
                }
                Ok(())
            },
            |()| Ok(catalog_wal::drop_user_type_record(txn, name)),
        )
    }

    fn alter_user_type(&self, txn: TxnId, user_type: UserTypeDescriptor) -> DbResult<()> {
        self.write_catalog_state_with_record(
            txn,
            true,
            CatalogTxnChange::ComplexWrite,
            |state| {
                let lookup = CatalogStore::normalize_identifier(&user_type.name);
                if !state.user_types.contains_key(&lookup) {
                    return Err(DbError::bind_error(
                        SqlState::UndefinedObject,
                        format!("type \"{}\" does not exist", user_type.name),
                    ));
                }
                state.user_types.insert(lookup, user_type.clone());
                Ok(user_type)
            },
            |user_type| catalog_wal::alter_user_type_record(txn, user_type),
        )?;
        Ok(())
    }

    fn create_cast(&self, txn: TxnId, cast: CastDescriptor) -> DbResult<()> {
        self.write_catalog_state_with_record(
            txn,
            true,
            CatalogTxnChange::ComplexWrite,
            |state| {
                let key = (
                    CatalogStore::normalize_identifier(&cast.source_type),
                    CatalogStore::normalize_identifier(&cast.target_type),
                );
                if state.casts.contains_key(&key) {
                    return Err(unique_violation(format!(
                        "cast from {} to {} already exists",
                        cast.source_type, cast.target_type
                    )));
                }
                state.casts.insert(key, cast.clone());
                Ok(cast)
            },
            |cast| catalog_wal::create_cast_record(txn, cast),
        )?;
        Ok(())
    }

    fn drop_cast(&self, txn: TxnId, source_type: &str, target_type: &str) -> DbResult<()> {
        self.write_catalog_state_with_record(
            txn,
            true,
            CatalogTxnChange::ComplexWrite,
            |state| {
                let key = (
                    CatalogStore::normalize_identifier(source_type),
                    CatalogStore::normalize_identifier(target_type),
                );
                if state.casts.remove(&key).is_none() {
                    return Err(DbError::bind_error(
                        SqlState::UndefinedObject,
                        format!("cast from {source_type} to {target_type} does not exist"),
                    ));
                }
                Ok(())
            },
            |()| Ok(catalog_wal::drop_cast_record(txn, source_type, target_type)),
        )
    }

    fn create_policy(&self, txn: TxnId, policy: PolicyDescriptor) -> DbResult<()> {
        self.write_catalog_state_with_record(
            txn,
            true,
            CatalogTxnChange::ComplexWrite,
            |state| {
                let key = (
                    CatalogStore::normalize_identifier(&policy.name),
                    CatalogStore::normalize_identifier(&policy.table_name),
                );
                if state.policies.contains_key(&key) {
                    return Err(unique_violation(format!(
                        "policy \"{}\" for table \"{}\" already exists",
                        policy.name, policy.table_name
                    )));
                }
                state.policies.insert(key, policy.clone());
                Ok(policy)
            },
            |policy| catalog_wal::create_policy_record(txn, policy),
        )?;
        Ok(())
    }

    fn drop_policy(&self, txn: TxnId, name: &str, table_name: &str) -> DbResult<()> {
        self.write_catalog_state_with_record(
            txn,
            true,
            CatalogTxnChange::ComplexWrite,
            |state| {
                let key = (
                    CatalogStore::normalize_identifier(name),
                    CatalogStore::normalize_identifier(table_name),
                );
                if state.policies.remove(&key).is_none() {
                    return Err(DbError::bind_error(
                        SqlState::UndefinedObject,
                        format!("policy \"{name}\" for table \"{table_name}\" does not exist"),
                    ));
                }
                Ok(())
            },
            |()| Ok(catalog_wal::drop_policy_record(txn, name, table_name)),
        )
    }

    fn alter_policy(&self, txn: TxnId, policy: PolicyDescriptor) -> DbResult<()> {
        self.write_catalog_state_with_record(
            txn,
            true,
            CatalogTxnChange::ComplexWrite,
            |state| {
                let key = (
                    CatalogStore::normalize_identifier(&policy.name),
                    CatalogStore::normalize_identifier(&policy.table_name),
                );
                if !state.policies.contains_key(&key) {
                    return Err(DbError::bind_error(
                        SqlState::UndefinedObject,
                        format!(
                            "policy \"{}\" for table \"{}\" does not exist",
                            policy.name, policy.table_name
                        ),
                    ));
                }
                state.policies.insert(key, policy.clone());
                Ok(policy)
            },
            |policy| catalog_wal::alter_policy_record(txn, policy),
        )?;
        Ok(())
    }

    fn create_rule(&self, txn: TxnId, rule: RuleDescriptor) -> DbResult<()> {
        self.write_catalog_state_with_record(
            txn,
            true,
            CatalogTxnChange::ComplexWrite,
            |state| {
                let key = (
                    CatalogStore::normalize_identifier(&rule.name),
                    CatalogStore::normalize_identifier(&rule.table_name),
                );
                if state.rules.contains_key(&key) {
                    return Err(unique_violation(format!(
                        "rule \"{}\" for relation \"{}\" already exists",
                        rule.name, rule.table_name
                    )));
                }
                state.rules.insert(key, rule.clone());
                Ok(rule)
            },
            |rule| catalog_wal::create_rule_record(txn, rule),
        )?;
        Ok(())
    }

    fn drop_rule(&self, txn: TxnId, name: &str, table_name: &str) -> DbResult<()> {
        self.write_catalog_state_with_record(
            txn,
            true,
            CatalogTxnChange::ComplexWrite,
            |state| {
                let key = (
                    CatalogStore::normalize_identifier(name),
                    CatalogStore::normalize_identifier(table_name),
                );
                if state.rules.remove(&key).is_none() {
                    return Err(DbError::bind_error(
                        SqlState::UndefinedObject,
                        format!("rule \"{name}\" for relation \"{table_name}\" does not exist"),
                    ));
                }
                Ok(())
            },
            |()| Ok(catalog_wal::drop_rule_record(txn, name, table_name)),
        )
    }

    fn set_comment(&self, txn: TxnId, comment: CommentDescriptor) -> DbResult<()> {
        self.write_catalog_state_with_record(
            txn,
            true,
            CatalogTxnChange::ComplexWrite,
            |state| {
                state.comments.insert(
                    (comment.object_type.clone(), comment.object_identity.clone()),
                    comment.comment.clone(),
                );
                Ok(comment.clone())
            },
            |desc| catalog_wal::set_comment_record(txn, &desc),
        )?;
        Ok(())
    }

    fn drop_comment(&self, txn: TxnId, object_type: &str, object_identity: &str) -> DbResult<()> {
        let object_type = object_type.to_owned();
        let object_identity = object_identity.to_owned();
        self.write_catalog_state_with_record(
            txn,
            true,
            CatalogTxnChange::ComplexWrite,
            |state| {
                state
                    .comments
                    .remove(&(object_type.clone(), object_identity.clone()));
                Ok(())
            },
            |()| {
                Ok(catalog_wal::drop_comment_record(
                    txn,
                    &object_type,
                    &object_identity,
                ))
            },
        )?;
        Ok(())
    }

    fn create_trigger(&self, txn: TxnId, trigger: TriggerDescriptor) -> DbResult<()> {
        self.write_catalog_state_with_record(
            txn,
            true,
            CatalogTxnChange::ComplexWrite,
            |state| {
                let trig_name = CatalogStore::normalize_identifier(&trigger.name);
                let exists = state.triggers.iter().any(|t| {
                    CatalogStore::normalize_identifier(&t.name) == trig_name
                        && CatalogStore::trigger_table_matches(&t.table_name, &trigger.table_name)
                });
                if exists {
                    let display_table = trigger
                        .table_name
                        .rsplit_once('.')
                        .map(|(_, n)| n)
                        .unwrap_or(trigger.table_name.as_str());
                    return Err(unique_violation(format!(
                        "trigger \"{}\" for relation \"{}\" already exists",
                        trigger.name, display_table
                    )));
                }
                state.triggers.push(trigger.clone());
                Ok(trigger)
            },
            |trigger| catalog_wal::create_trigger_record(txn, trigger),
        )?;
        Ok(())
    }

    fn drop_trigger(&self, txn: TxnId, name: &str, table_name: &str) -> DbResult<()> {
        self.write_catalog_state_with_record(
            txn,
            true,
            CatalogTxnChange::ComplexWrite,
            |state| {
                let trig_name = CatalogStore::normalize_identifier(name);
                let before = state.triggers.len();
                state.triggers.retain(|t| {
                    !(CatalogStore::normalize_identifier(&t.name) == trig_name
                        && CatalogStore::trigger_table_matches(&t.table_name, table_name))
                });
                if state.triggers.len() == before {
                    return Err(DbError::bind_error(
                        SqlState::UndefinedObject,
                        format!("trigger \"{name}\" for table \"{table_name}\" does not exist"),
                    ));
                }
                Ok(())
            },
            |()| Ok(catalog_wal::drop_trigger_record(txn, name, table_name)),
        )
    }

    fn rename_trigger(
        &self,
        txn: TxnId,
        name: &str,
        table_name: &str,
        new_name: &str,
    ) -> DbResult<()> {
        self.write_catalog_change(
            txn,
            CatalogTxnChange::ComplexWrite,
            |state| {
                let trig_name = CatalogStore::normalize_identifier(name);
                let new_trig_name = CatalogStore::normalize_identifier(new_name);

                let dup = state.triggers.iter().any(|t| {
                    CatalogStore::normalize_identifier(&t.name) == new_trig_name
                        && CatalogStore::trigger_table_matches(&t.table_name, table_name)
                });
                if dup {
                    return Err(unique_violation(format!(
                        "trigger \"{new_name}\" for relation \"{table_name}\" already exists"
                    )));
                }

                for t in &mut state.triggers {
                    if CatalogStore::normalize_identifier(&t.name) == trig_name
                        && CatalogStore::trigger_table_matches(&t.table_name, table_name)
                    {
                        new_name.clone_into(&mut t.name);
                        return Ok(t.clone());
                    }
                }
                Err(DbError::bind_error(
                    SqlState::UndefinedObject,
                    format!("trigger \"{name}\" for relation \"{table_name}\" does not exist"),
                ))
            },
            |renamed| {
                Ok(vec![
                    catalog_wal::drop_trigger_record(txn, name, table_name),
                    catalog_wal::create_trigger_record(txn, renamed)?,
                ])
            },
        )
        .map(|_| ())
    }
}

#[cfg(test)]
mod tests;
