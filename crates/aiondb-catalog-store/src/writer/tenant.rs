use super::*;

pub(super) fn create_tenant(
    store: &CatalogStore,
    txn: TxnId,
    name: &str,
) -> DbResult<TenantDescriptor> {
    store.write_catalog_state_with_record(
        txn,
        true,
        CatalogTxnChange::ComplexWrite,
        |state| {
            let lookup = CatalogStore::normalize_identifier(name);
            if state.tenants_by_name.contains_key(&lookup) {
                return Err(unique_violation(format!(
                    "tenant \"{name}\" already exists"
                )));
            }

            // Create the tenant schema named "tenant_<name>"
            let schema_name = format!("tenant_{lookup}");
            if state.schema_names.contains_key(&schema_name) {
                return Err(unique_violation(format!(
                    "schema \"{schema_name}\" already exists"
                )));
            }

            let schema_id = CatalogStore::next_schema_id(state, SchemaId::default());
            let schema = SchemaDescriptor {
                schema_id,
                name: schema_name.clone(),
            };
            state.schemas_by_id.insert(schema_id, schema);
            state.schema_names.insert(schema_name, schema_id);

            let tenant_id = CatalogStore::next_tenant_id(state, Default::default());
            let descriptor = TenantDescriptor {
                tenant_id,
                name: lookup.clone(),
                schema_id,
            };
            state.tenants_by_name.insert(lookup, descriptor.clone());
            Ok(descriptor)
        },
        |descriptor| crate::catalog_wal::create_tenant_record(txn, descriptor),
    )
}

pub(super) fn drop_tenant(store: &CatalogStore, txn: TxnId, name: &str) -> DbResult<()> {
    store.write_catalog_state_with_record(
        txn,
        true,
        CatalogTxnChange::ComplexWrite,
        |state| {
            let lookup = CatalogStore::normalize_identifier(name);
            let descriptor = state
                .tenants_by_name
                .remove(&lookup)
                .ok_or_else(|| crate::undefined_tenant(name))?;

            let schema_id = descriptor.schema_id;

            // Cascade: drop all tables in the tenant schema
            let table_ids: Vec<RelationId> = state
                .tables_by_id
                .values()
                .filter(|t| t.schema_id == schema_id)
                .map(|t| t.table_id)
                .collect();
            for table_id in table_ids {
                if let Some(table) = state.tables_by_id.remove(&table_id) {
                    let key = (
                        table.schema_id,
                        CatalogStore::normalize_identifier(&table.name.name),
                    );
                    state.table_names.remove(&key);
                    state.statistics.remove(&table_id);
                    if let Some(index_ids) = state.indexes_by_table.remove(&table_id) {
                        for index_id in index_ids {
                            if let Some(index) = state.indexes_by_id.remove(&index_id) {
                                let ikey = (
                                    index.schema_id,
                                    CatalogStore::normalize_identifier(&index.name.name),
                                );
                                state.index_names.remove(&ikey);
                            }
                        }
                    }
                }
            }

            // Cascade: drop all sequences in the tenant schema
            let seq_ids: Vec<_> = state
                .sequences_by_id
                .values()
                .filter(|s| s.schema_id == schema_id)
                .map(|s| s.sequence_id)
                .collect();
            for seq_id in seq_ids {
                if let Some(seq) = state.sequences_by_id.remove(&seq_id) {
                    let key = (
                        seq.schema_id,
                        CatalogStore::normalize_identifier(&seq.name.name),
                    );
                    state.sequence_names.remove(&key);
                    state.sequence_values.remove(&seq_id);
                }
            }

            // Cascade: drop all views in the tenant schema
            let view_ids: Vec<_> = state
                .views_by_id
                .values()
                .filter(|v| v.schema_id == schema_id)
                .map(|v| v.view_id)
                .collect();
            for view_id in view_ids {
                if let Some(view) = state.views_by_id.remove(&view_id) {
                    let key = (
                        view.schema_id,
                        CatalogStore::normalize_identifier(&view.name.name),
                    );
                    state.view_names.remove(&key);
                }
            }

            // Drop the tenant schema itself
            if let Some(schema) = state.schemas_by_id.remove(&schema_id) {
                state
                    .schema_names
                    .remove(&CatalogStore::normalize_identifier(&schema.name));
            }

            Ok(())
        },
        |()| Ok(crate::catalog_wal::drop_tenant_record(txn, name)),
    )
}
