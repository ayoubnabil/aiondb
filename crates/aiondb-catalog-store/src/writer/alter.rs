use super::*;

/// Find a mutable column by id, returning an error if not found.
fn find_column_mut(
    table: &mut TableDescriptor,
    column_id: ColumnId,
) -> DbResult<&mut ColumnDescriptor> {
    let table_name = table.name.clone();
    table
        .columns
        .iter_mut()
        .find(|c| c.column_id == column_id)
        .ok_or_else(|| {
            DbError::Bind(Box::new(ErrorReport::new(
                SqlState::UndefinedColumn,
                format!(
                    "column id {} does not exist in table {}",
                    column_id.get(),
                    table_name
                ),
            )))
        })
}

/// Apply a `TableAlteration` to a mutable table descriptor within the catalog
/// state.  Returns `Ok(())` on success; on error the table is re-inserted into
/// state before returning.
pub(crate) fn apply_alteration(
    state: &mut crate::CatalogState,
    table_id: RelationId,
    table: &mut TableDescriptor,
    previous_key: &(SchemaId, String),
    alteration: TableAlteration,
) -> DbResult<()> {
    match alteration {
        TableAlteration::AddColumn(mut column) => {
            let normalized = CatalogStore::normalize_identifier(&column.name);
            if table
                .columns
                .iter()
                .any(|existing| CatalogStore::normalize_identifier(&existing.name) == normalized)
            {
                let msg = format!(
                    "column \"{}\" already exists in table {}",
                    column.name, table.name
                );
                state.tables_by_id.insert(table_id, table.clone());
                return Err(unique_violation(msg));
            }
            column.column_id = CatalogStore::next_column_id(state, column.column_id);
            if column.ordinal_position == 0 {
                column.ordinal_position = u32::try_from(table.columns.len() + 1)
                    .map_err(|_| DbError::internal("column ordinal exceeds u32"))?;
            }
            table.columns.push(column);
        }
        TableAlteration::DropColumn { column_id } => {
            // Resolve the column name first so we can both build a descriptive
            // error and check trigger dependencies before mutating the table.
            let column_name = table
                .columns
                .iter()
                .find(|column| column.column_id == column_id)
                .map(|column| column.name.clone());
            let Some(column_name) = column_name else {
                let msg = format!(
                    "column id {} does not exist in table {}",
                    column_id.get(),
                    table.name
                );
                state.tables_by_id.insert(table_id, table.clone());
                return Err(DbError::Bind(Box::new(ErrorReport::new(
                    SqlState::UndefinedColumn,
                    msg,
                ))));
            };
            let column_lower = column_name.to_ascii_lowercase();
            let table_object_name = table.name.object_name().to_owned();
            let dependent_triggers: Vec<String> = state
                .triggers
                .iter()
                .filter(|t| {
                    t.table_name
                        .rsplit('.')
                        .next()
                        .is_some_and(|n| n.eq_ignore_ascii_case(&table_object_name))
                        && t.update_columns.contains(&column_lower)
                })
                .map(|t| t.name.clone())
                .collect();
            // Indexes that reference the dropped column (key or include)
            // would survive pointing at a non-existent column, breaking
            // pg_index semantics and any planner lookup. Reject ALTER
            // unless those indexes are explicitly dropped first.
            let dependent_indexes: Vec<String> = state
                .indexes_by_id
                .values()
                .filter(|idx| {
                    idx.table_id == table_id
                        && (idx.key_columns.iter().any(|k| k.column_id == column_id)
                            || idx.include_columns.contains(&column_id))
                })
                .map(|idx| idx.name.object_name().to_owned())
                .collect();
            if !dependent_triggers.is_empty() || !dependent_indexes.is_empty() {
                state.tables_by_id.insert(table_id, table.clone());
                let mut report = ErrorReport::new(
                    SqlState::DependentObjectsStillExist,
                    format!(
                        "cannot drop column {column_name} of table {table_object_name} because other objects depend on it"
                    ),
                );
                let mut detail_lines: Vec<String> = dependent_triggers
                    .iter()
                    .map(|name| {
                        format!(
                            "trigger {name} on table {table_object_name} depends on column {column_name} of table {table_object_name}"
                        )
                    })
                    .collect();
                detail_lines.extend(dependent_indexes.iter().map(|name| {
                    format!(
                        "index {name} on table {table_object_name} depends on column {column_name} of table {table_object_name}"
                    )
                }));
                report = report
                    .with_client_detail(detail_lines.join("\n"))
                    .with_client_hint("Use DROP ... CASCADE to drop the dependent objects too.");
                return Err(DbError::Bind(Box::new(report)));
            }
            table.columns.retain(|column| column.column_id != column_id);
            for (position, column) in table.columns.iter_mut().enumerate() {
                column.ordinal_position = u32::try_from(position + 1)
                    .map_err(|_| DbError::internal("column ordinal exceeds u32"))?;
            }
            if let Some(primary_key) = &mut table.primary_key {
                primary_key.retain(|existing| *existing != column_id);
                if primary_key.is_empty() {
                    table.primary_key = None;
                }
            }
        }
        TableAlteration::RenameTable { new_name } => {
            let target_schema_id = if new_name.schema_name().is_some() {
                CatalogStore::resolve_schema_id(state, &new_name)?
            } else {
                table.schema_id
            };
            let target_schema_name = CatalogStore::schema_name_by_id(state, target_schema_id)?;
            let target_key = (
                target_schema_id,
                CatalogStore::normalize_identifier(&new_name.name),
            );
            if target_key != *previous_key && state.table_names.contains_key(&target_key) {
                state.tables_by_id.insert(table_id, table.clone());
                return Err(unique_violation(format!(
                    "table \"{}.{}\" already exists",
                    target_schema_name, new_name.name
                )));
            }
            table.schema_id = target_schema_id;
            table.name = QualifiedName::qualified(target_schema_name, new_name.name);
            state.table_names.remove(previous_key);
            state.table_names.insert(target_key, table_id);
        }
        TableAlteration::RenameColumn {
            column_id,
            new_name,
        } => {
            let target_name = CatalogStore::normalize_identifier(&new_name);
            if table.columns.iter().any(|column| {
                column.column_id != column_id
                    && CatalogStore::normalize_identifier(&column.name) == target_name
            }) {
                let msg = format!(
                    "column \"{new_name}\" already exists in table {}",
                    table.name
                );
                state.tables_by_id.insert(table_id, table.clone());
                return Err(unique_violation(msg));
            }
            let column = table
                .columns
                .iter_mut()
                .find(|column| column.column_id == column_id);
            match column {
                Some(column) => column.name = new_name,
                None => {
                    let msg = format!(
                        "column id {} does not exist in table {}",
                        column_id.get(),
                        table.name
                    );
                    state.tables_by_id.insert(table_id, table.clone());
                    return Err(DbError::Bind(Box::new(ErrorReport::new(
                        SqlState::UndefinedColumn,
                        msg,
                    ))));
                }
            }
        }
        TableAlteration::SetDefault {
            column_id,
            default_expr,
        } => {
            find_column_mut(table, column_id)?.default_value = Some(default_expr);
        }
        TableAlteration::DropDefault { column_id } => {
            find_column_mut(table, column_id)?.default_value = None;
        }
        TableAlteration::SetNotNull { column_id } => {
            find_column_mut(table, column_id)?.nullable = false;
        }
        TableAlteration::DropNotNull { column_id } => {
            find_column_mut(table, column_id)?.nullable = true;
        }
        TableAlteration::AddConstraint {
            constraint_type,
            constraint_name,
            columns,
            check_expr,
            ref_table,
            ref_columns,
            on_delete,
            on_update,
            on_delete_set_columns,
            on_update_set_columns,
            match_type,
        } => {
            apply_add_constraint(
                table,
                &constraint_type,
                constraint_name,
                columns,
                check_expr,
                ref_table,
                ref_columns,
                on_delete,
                on_update,
                on_delete_set_columns,
                on_update_set_columns,
                match_type,
            );
        }
        TableAlteration::DropConstraint { constraint_name } => {
            // Drop matching CHECK constraints.
            table.check_constraints.retain(|c| {
                c.name
                    .as_ref()
                    .map_or(true, |n| !n.eq_ignore_ascii_case(&constraint_name))
            });
            table.foreign_keys.retain(|fk| {
                !fk.effective_name(table.name.object_name())
                    .eq_ignore_ascii_case(&constraint_name)
            });

            // Drop backing index for PRIMARY KEY / UNIQUE constraint.
            // If a unique index with this name exists, drop it.
            let backing_index_id = state
                .index_names
                .iter()
                .find(|((_, name), _)| name.eq_ignore_ascii_case(&constraint_name))
                .map(|(_, id)| *id);
            if let Some(index_id) = backing_index_id {
                let normalized = CatalogStore::normalize_identifier(&constraint_name);
                // Remove the primary key reference if the dropped index is the
                // one currently backing the table primary key.
                if let Some(primary_key) = table.primary_key.clone() {
                    let dropped_index_matches_primary_key =
                        state.indexes_by_id.get(&index_id).is_some_and(|idx| {
                            idx.key_columns
                                .iter()
                                .map(|key| key.column_id)
                                .collect::<Vec<_>>()
                                == primary_key
                        });
                    let pkey_name = format!("{}_pkey", table.name.name);
                    let dropped_default_pkey_name =
                        CatalogStore::normalize_identifier(&pkey_name) == normalized;
                    if dropped_index_matches_primary_key || dropped_default_pkey_name {
                        table.primary_key = None;
                    }
                }

                // Remove the index from the catalog.
                state
                    .index_names
                    .retain(|(_, name), _| !name.eq_ignore_ascii_case(&constraint_name));
                state.indexes_by_id.remove(&index_id);
                // Also clean up indexes_by_table to prevent dangling references.
                if let Some(indexes) = state.indexes_by_table.get_mut(&table_id) {
                    indexes.retain(|existing| *existing != index_id);
                    if indexes.is_empty() {
                        state.indexes_by_table.remove(&table_id);
                    }
                }
            }
        }
        TableAlteration::AlterColumnType {
            column_id,
            new_type,
            raw_type_name,
            text_type_modifier,
        } => {
            let column = find_column_mut(table, column_id)?;
            column.data_type = new_type;
            column.raw_type_name = raw_type_name;
            column.text_type_modifier = text_type_modifier;
        }
    }
    Ok(())
}

fn apply_add_constraint(
    table: &mut TableDescriptor,
    constraint_type: &str,
    constraint_name: Option<String>,
    columns: Vec<String>,
    check_expr: Option<String>,
    ref_table: Option<String>,
    ref_columns: Vec<String>,
    on_delete: aiondb_core::FkAction,
    on_update: aiondb_core::FkAction,
    on_delete_set_columns: Vec<String>,
    on_update_set_columns: Vec<String>,
    match_type: aiondb_core::FkMatchType,
) {
    match constraint_type {
        "PRIMARY KEY" => {
            let mut pk_ids = Vec::with_capacity(columns.len());
            for name in &columns {
                if let Some(column) = table
                    .columns
                    .iter_mut()
                    .find(|column| column.name.eq_ignore_ascii_case(name))
                {
                    pk_ids.push(column.column_id);
                    column.nullable = false;
                }
            }
            table.primary_key = Some(pk_ids);
        }
        "CHECK" => {
            if let Some(expr) = check_expr {
                table
                    .check_constraints
                    .push(aiondb_catalog::CheckConstraint {
                        name: constraint_name,
                        expression: expr,
                    });
            }
        }
        "FOREIGN KEY" => {
            if let Some(rt) = ref_table {
                table
                    .foreign_keys
                    .push(aiondb_catalog::ForeignKeyConstraint {
                        columns,
                        referenced_table: rt,
                        referenced_columns: ref_columns,
                        on_delete,
                        on_update,
                        on_delete_set_columns,
                        on_update_set_columns,
                        match_type,
                        name: constraint_name,
                    });
            }
        }
        _ => {} // UNIQUE: no dedicated catalog list yet
    }
}
