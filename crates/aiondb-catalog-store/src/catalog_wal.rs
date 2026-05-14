//! WAL integration for durable catalog persistence.
//!
//! Each catalog mutation is logged as a typed `WalRecord` variant with its
//! descriptor serialized to JSON. During recovery the WAL entries are replayed
//! to rebuild the `CatalogState`.

use aiondb_catalog::{
    CastDescriptor, CommentDescriptor, DomainDescriptor, EdgeLabelDescriptor, FunctionDescriptor,
    IndexDescriptor, NodeLabelDescriptor, PolicyDescriptor, PrivilegeDescriptor, RoleDescriptor,
    RuleDescriptor, SchemaDescriptor, SequenceDescriptor, TableDescriptor, TableStatistics,
    TenantDescriptor, TriggerDescriptor, UserTypeDescriptor, ViewDescriptor,
};
use aiondb_core::{DbError, DbResult, IndexId, RelationId, SchemaId, SequenceId, TxnId};
use aiondb_wal::WalRecord;

use crate::CatalogState;

// ---------------------------------------------------------------------------
// Helpers: serialize descriptor to JSON bytes
// ---------------------------------------------------------------------------

fn to_json<T: serde::Serialize>(value: &T) -> DbResult<Vec<u8>> {
    serde_json::to_vec(value)
        .map_err(|e| DbError::internal(format!("catalog WAL: serialize failed: {e}")))
}

pub(crate) fn from_json<T: serde::de::DeserializeOwned>(data: &[u8]) -> DbResult<T> {
    serde_json::from_slice(data)
        .map_err(|e| DbError::internal(format!("catalog WAL: deserialize failed: {e}")))
}

// ---------------------------------------------------------------------------
// Build WalRecord variants for each catalog operation
// ---------------------------------------------------------------------------

pub(crate) fn create_schema_record(txn_id: TxnId, desc: &SchemaDescriptor) -> DbResult<WalRecord> {
    Ok(WalRecord::CatalogCreateSchema {
        txn_id,
        descriptor_json: to_json(desc)?,
    })
}

pub(crate) fn drop_schema_record(txn_id: TxnId, schema_id: SchemaId) -> WalRecord {
    WalRecord::CatalogDropSchema {
        txn_id,
        schema_id_raw: schema_id.get(),
    }
}

pub(crate) fn set_table_descriptor_record(
    txn_id: TxnId,
    desc: &TableDescriptor,
) -> DbResult<WalRecord> {
    Ok(WalRecord::CatalogSetTableDescriptor {
        txn_id,
        descriptor_json: to_json(desc)?,
    })
}

pub(crate) fn set_index_descriptor_record(
    txn_id: TxnId,
    desc: &IndexDescriptor,
) -> DbResult<WalRecord> {
    Ok(WalRecord::CatalogSetIndexDescriptor {
        txn_id,
        descriptor_json: to_json(desc)?,
    })
}

pub(crate) fn create_tenant_record(txn_id: TxnId, desc: &TenantDescriptor) -> DbResult<WalRecord> {
    Ok(WalRecord::CatalogCreateTenant {
        txn_id,
        descriptor_json: to_json(desc)?,
    })
}

pub(crate) fn drop_tenant_record(txn_id: TxnId, name: &str) -> WalRecord {
    WalRecord::CatalogDropTenant {
        txn_id,
        tenant_name: name.to_owned(),
    }
}

pub(crate) fn drop_table_record(txn_id: TxnId, table_id: RelationId) -> WalRecord {
    WalRecord::CatalogDropTable {
        txn_id,
        table_id_raw: table_id.get(),
    }
}

pub(crate) fn drop_index_record(txn_id: TxnId, index_id: IndexId) -> WalRecord {
    WalRecord::CatalogDropIndex {
        txn_id,
        index_id_raw: index_id.get(),
    }
}

pub(crate) fn update_statistics_record(
    txn_id: TxnId,
    stats: &TableStatistics,
) -> DbResult<WalRecord> {
    Ok(WalRecord::CatalogUpdateStatistics {
        txn_id,
        descriptor_json: to_json(stats)?,
    })
}

pub(crate) fn create_node_label_record(
    txn_id: TxnId,
    desc: &NodeLabelDescriptor,
) -> DbResult<WalRecord> {
    Ok(WalRecord::CatalogCreateNodeLabel {
        txn_id,
        descriptor_json: to_json(desc)?,
    })
}

pub(crate) fn create_edge_label_record(
    txn_id: TxnId,
    desc: &EdgeLabelDescriptor,
) -> DbResult<WalRecord> {
    Ok(WalRecord::CatalogCreateEdgeLabel {
        txn_id,
        descriptor_json: to_json(desc)?,
    })
}

pub(crate) fn drop_node_label_record(txn_id: TxnId, name: &str) -> WalRecord {
    WalRecord::CatalogDropNodeLabel {
        txn_id,
        label_name: name.to_owned(),
    }
}

pub(crate) fn drop_edge_label_record(txn_id: TxnId, name: &str) -> WalRecord {
    WalRecord::CatalogDropEdgeLabel {
        txn_id,
        label_name: name.to_owned(),
    }
}

pub(crate) fn create_role_record(txn_id: TxnId, desc: &RoleDescriptor) -> DbResult<WalRecord> {
    Ok(WalRecord::CatalogCreateRole {
        txn_id,
        descriptor_json: to_json(desc)?,
    })
}

pub(crate) fn alter_role_record(txn_id: TxnId, desc: &RoleDescriptor) -> DbResult<WalRecord> {
    Ok(WalRecord::CatalogAlterRole {
        txn_id,
        descriptor_json: to_json(desc)?,
    })
}

pub(crate) fn drop_role_record(txn_id: TxnId, name: &str) -> WalRecord {
    WalRecord::CatalogDropRole {
        txn_id,
        role_name: name.to_owned(),
    }
}

pub(crate) fn create_view_record(txn_id: TxnId, desc: &ViewDescriptor) -> DbResult<WalRecord> {
    Ok(WalRecord::CatalogCreateView {
        txn_id,
        descriptor_json: to_json(desc)?,
    })
}

pub(crate) fn drop_view_record(txn_id: TxnId, view_id: aiondb_core::RelationId) -> WalRecord {
    WalRecord::CatalogDropView {
        txn_id,
        view_id_raw: view_id.get(),
    }
}

pub(crate) fn create_sequence_record(
    txn_id: TxnId,
    desc: &SequenceDescriptor,
) -> DbResult<WalRecord> {
    Ok(WalRecord::CatalogCreateSequence {
        txn_id,
        descriptor_json: to_json(desc)?,
    })
}

pub(crate) fn alter_sequence_record(
    txn_id: TxnId,
    desc: &SequenceDescriptor,
) -> DbResult<WalRecord> {
    Ok(WalRecord::CatalogAlterSequence {
        txn_id,
        descriptor_json: to_json(desc)?,
    })
}

pub(crate) fn drop_sequence_record(txn_id: TxnId, sequence_id: SequenceId) -> WalRecord {
    WalRecord::CatalogDropSequence {
        txn_id,
        sequence_id_raw: sequence_id.get(),
    }
}

pub(crate) fn set_sequence_value_record(
    txn_id: TxnId,
    sequence_id: SequenceId,
    runtime: &crate::SequenceValueState,
) -> WalRecord {
    WalRecord::CatalogSetSequenceValue {
        txn_id,
        sequence_id_raw: sequence_id.get(),
        current_value: runtime.current_value,
        is_called: runtime.is_called,
    }
}

pub(crate) fn create_function_record(
    txn_id: TxnId,
    desc: &FunctionDescriptor,
) -> DbResult<WalRecord> {
    Ok(WalRecord::CatalogCreateFunction {
        txn_id,
        descriptor_json: to_json(desc)?,
    })
}

pub(crate) fn drop_function_record(txn_id: TxnId, name: &str) -> WalRecord {
    WalRecord::CatalogDropFunction {
        txn_id,
        function_name: name.to_owned(),
    }
}

pub(crate) fn create_domain_record(txn_id: TxnId, desc: &DomainDescriptor) -> DbResult<WalRecord> {
    Ok(WalRecord::CatalogCreateDomain {
        txn_id,
        descriptor_json: to_json(desc)?,
    })
}

pub(crate) fn drop_domain_record(txn_id: TxnId, name: &str) -> WalRecord {
    WalRecord::CatalogDropDomain {
        txn_id,
        domain_name: name.to_owned(),
    }
}

pub(crate) fn alter_domain_record(txn_id: TxnId, desc: &DomainDescriptor) -> DbResult<WalRecord> {
    Ok(WalRecord::CatalogAlterDomain {
        txn_id,
        descriptor_json: to_json(desc)?,
    })
}

pub(crate) fn create_user_type_record(
    txn_id: TxnId,
    desc: &UserTypeDescriptor,
) -> DbResult<WalRecord> {
    Ok(WalRecord::CatalogCreateUserType {
        txn_id,
        descriptor_json: to_json(desc)?,
    })
}

pub(crate) fn drop_user_type_record(txn_id: TxnId, name: &str) -> WalRecord {
    WalRecord::CatalogDropUserType {
        txn_id,
        type_name: name.to_owned(),
    }
}

pub(crate) fn alter_user_type_record(
    txn_id: TxnId,
    desc: &UserTypeDescriptor,
) -> DbResult<WalRecord> {
    Ok(WalRecord::CatalogAlterUserType {
        txn_id,
        descriptor_json: to_json(desc)?,
    })
}

pub(crate) fn create_cast_record(txn_id: TxnId, desc: &CastDescriptor) -> DbResult<WalRecord> {
    Ok(WalRecord::CatalogCreateCast {
        txn_id,
        descriptor_json: to_json(desc)?,
    })
}

pub(crate) fn drop_cast_record(txn_id: TxnId, source_type: &str, target_type: &str) -> WalRecord {
    WalRecord::CatalogDropCast {
        txn_id,
        source_type: source_type.to_owned(),
        target_type: target_type.to_owned(),
    }
}

pub(crate) fn create_policy_record(txn_id: TxnId, desc: &PolicyDescriptor) -> DbResult<WalRecord> {
    Ok(WalRecord::CatalogCreatePolicy {
        txn_id,
        descriptor_json: to_json(desc)?,
    })
}

pub(crate) fn drop_policy_record(txn_id: TxnId, policy_name: &str, table_name: &str) -> WalRecord {
    WalRecord::CatalogDropPolicy {
        txn_id,
        policy_name: policy_name.to_owned(),
        table_name: table_name.to_owned(),
    }
}

pub(crate) fn alter_policy_record(txn_id: TxnId, desc: &PolicyDescriptor) -> DbResult<WalRecord> {
    Ok(WalRecord::CatalogAlterPolicy {
        txn_id,
        descriptor_json: to_json(desc)?,
    })
}

pub(crate) fn create_rule_record(txn_id: TxnId, desc: &RuleDescriptor) -> DbResult<WalRecord> {
    Ok(WalRecord::CatalogCreateRule {
        txn_id,
        descriptor_json: to_json(desc)?,
    })
}

pub(crate) fn drop_rule_record(txn_id: TxnId, rule_name: &str, table_name: &str) -> WalRecord {
    WalRecord::CatalogDropRule {
        txn_id,
        rule_name: rule_name.to_owned(),
        table_name: table_name.to_owned(),
    }
}

pub(crate) fn set_comment_record(txn_id: TxnId, desc: &CommentDescriptor) -> DbResult<WalRecord> {
    Ok(WalRecord::CatalogSetComment {
        txn_id,
        descriptor_json: to_json(desc)?,
    })
}

pub(crate) fn drop_comment_record(
    txn_id: TxnId,
    object_type: &str,
    object_identity: &str,
) -> WalRecord {
    WalRecord::CatalogDropComment {
        txn_id,
        object_type: object_type.to_owned(),
        object_identity: object_identity.to_owned(),
    }
}

pub(crate) fn create_trigger_record(
    txn_id: TxnId,
    desc: &TriggerDescriptor,
) -> DbResult<WalRecord> {
    Ok(WalRecord::CatalogCreateTrigger {
        txn_id,
        descriptor_json: to_json(desc)?,
    })
}

pub(crate) fn drop_trigger_record(txn_id: TxnId, name: &str, table_name: &str) -> WalRecord {
    WalRecord::CatalogDropTrigger {
        txn_id,
        trigger_name: name.to_owned(),
        table_name: table_name.to_owned(),
    }
}

pub(crate) fn grant_privilege_record(
    txn_id: TxnId,
    desc: &PrivilegeDescriptor,
) -> DbResult<WalRecord> {
    Ok(WalRecord::CatalogGrantPrivilege {
        txn_id,
        descriptor_json: to_json(desc)?,
    })
}

pub(crate) fn revoke_privilege_record(
    txn_id: TxnId,
    desc: &PrivilegeDescriptor,
) -> DbResult<WalRecord> {
    Ok(WalRecord::CatalogRevokePrivilege {
        txn_id,
        descriptor_json: to_json(desc)?,
    })
}

// ---------------------------------------------------------------------------
// Replay a single catalog WAL record into a CatalogState
// ---------------------------------------------------------------------------

/// Apply a single catalog WAL record to the given state.
///
/// This is used during recovery to rebuild the `CatalogState` from WAL entries.
/// It applies the operation unconditionally (no conflict detection) since WAL
/// entries represent already-committed operations.
pub fn replay_catalog_record(state: &mut CatalogState, record: &WalRecord) -> DbResult<()> {
    match record {
        WalRecord::CatalogCreateSchema {
            descriptor_json, ..
        } => {
            let desc: SchemaDescriptor = from_json(descriptor_json)?;
            let schema_id = crate::CatalogStore::next_schema_id(state, desc.schema_id);
            let lookup = crate::CatalogStore::normalize_identifier(&desc.name);
            let mut descriptor = desc;
            descriptor.schema_id = schema_id;
            state.schemas_by_id.insert(schema_id, descriptor);
            state.schema_names.insert(lookup, schema_id);
        }
        WalRecord::CatalogDropSchema { schema_id_raw, .. } => {
            let schema_id = SchemaId::new(*schema_id_raw);
            if let Some(schema) = state.schemas_by_id.remove(&schema_id) {
                let lookup = crate::CatalogStore::normalize_identifier(&schema.name);
                state.schema_names.remove(&lookup);
            }
        }
        WalRecord::CatalogSetTableDescriptor {
            descriptor_json, ..
        } => {
            let desc: TableDescriptor = from_json(descriptor_json)?;
            let table_id = desc.table_id;
            // Update ID counters
            crate::CatalogStore::next_table_id(state, table_id);
            for col in &desc.columns {
                crate::CatalogStore::next_column_id(state, col.column_id);
            }
            let name_key = (
                desc.schema_id,
                crate::CatalogStore::normalize_identifier(&desc.name.name),
            );
            // Remove old name entry if the table already existed
            if let Some(old) = state.tables_by_id.get(&table_id) {
                let old_key = (
                    old.schema_id,
                    crate::CatalogStore::normalize_identifier(&old.name.name),
                );
                state.table_names.remove(&old_key);
            }
            state.table_names.insert(name_key, table_id);
            state.tables_by_id.insert(table_id, desc);
        }
        WalRecord::CatalogSetIndexDescriptor {
            descriptor_json, ..
        } => {
            let desc: IndexDescriptor = from_json(descriptor_json)?;
            let index_id = desc.index_id;
            crate::CatalogStore::next_index_id(state, index_id);

            if let Some(old) = state.indexes_by_id.get(&index_id).cloned() {
                let old_key = (
                    old.schema_id,
                    crate::CatalogStore::normalize_identifier(&old.name.name),
                );
                state.index_names.remove(&old_key);
                let mut remove_table_entry = false;
                if let Some(index_ids) = state.indexes_by_table.get_mut(&old.table_id) {
                    index_ids.retain(|existing| *existing != index_id);
                    remove_table_entry = index_ids.is_empty();
                }
                if remove_table_entry {
                    state.indexes_by_table.remove(&old.table_id);
                }
            }

            let name_key = (
                desc.schema_id,
                crate::CatalogStore::normalize_identifier(&desc.name.name),
            );
            state.index_names.insert(name_key, index_id);
            state.indexes_by_id.insert(index_id, desc.clone());
            let index_ids = state.indexes_by_table.entry(desc.table_id).or_default();
            if !index_ids.contains(&index_id) {
                index_ids.push(index_id);
            }
        }
        WalRecord::CatalogCreateTenant {
            descriptor_json, ..
        } => {
            let desc: TenantDescriptor = from_json(descriptor_json)?;
            let tenant_id = crate::CatalogStore::next_tenant_id(state, desc.tenant_id);
            let schema_id = crate::CatalogStore::next_schema_id(state, desc.schema_id);
            let schema_name = format!("tenant_{}", desc.name);
            state.schemas_by_id.insert(
                schema_id,
                SchemaDescriptor {
                    schema_id,
                    name: schema_name.clone(),
                },
            );
            state.schema_names.insert(schema_name, schema_id);
            state.tenants_by_name.insert(
                crate::CatalogStore::normalize_identifier(&desc.name),
                TenantDescriptor {
                    tenant_id,
                    name: desc.name,
                    schema_id,
                },
            );
        }
        WalRecord::CatalogDropTenant { tenant_name, .. } => {
            let lookup = crate::CatalogStore::normalize_identifier(tenant_name);
            if let Some(descriptor) = state.tenants_by_name.remove(&lookup) {
                let schema_id = descriptor.schema_id;

                let table_ids: Vec<RelationId> = state
                    .tables_by_id
                    .values()
                    .filter(|table| table.schema_id == schema_id)
                    .map(|table| table.table_id)
                    .collect();
                for table_id in table_ids {
                    if let Some(table) = state.tables_by_id.remove(&table_id) {
                        crate::CatalogStore::remove_privileges_for_dropped_relation(
                            state,
                            &table.name,
                        );
                        let key = (
                            table.schema_id,
                            crate::CatalogStore::normalize_identifier(&table.name.name),
                        );
                        state.table_names.remove(&key);
                        state.statistics.remove(&table_id);
                        if let Some(index_ids) = state.indexes_by_table.remove(&table_id) {
                            for index_id in index_ids {
                                if let Some(index) = state.indexes_by_id.remove(&index_id) {
                                    let index_key = (
                                        index.schema_id,
                                        crate::CatalogStore::normalize_identifier(&index.name.name),
                                    );
                                    state.index_names.remove(&index_key);
                                }
                            }
                        }
                    }
                }

                let seq_ids: Vec<SequenceId> = state
                    .sequences_by_id
                    .values()
                    .filter(|sequence| sequence.schema_id == schema_id)
                    .map(|sequence| sequence.sequence_id)
                    .collect();
                for seq_id in seq_ids {
                    if let Some(seq) = state.sequences_by_id.remove(&seq_id) {
                        crate::CatalogStore::remove_privileges_for_dropped_relation(
                            state, &seq.name,
                        );
                        let key = (
                            seq.schema_id,
                            crate::CatalogStore::normalize_identifier(&seq.name.name),
                        );
                        state.sequence_names.remove(&key);
                        state.sequence_values.remove(&seq_id);
                    }
                }

                let view_ids: Vec<RelationId> = state
                    .views_by_id
                    .values()
                    .filter(|view| view.schema_id == schema_id)
                    .map(|view| view.view_id)
                    .collect();
                for view_id in view_ids {
                    if let Some(view) = state.views_by_id.remove(&view_id) {
                        crate::CatalogStore::remove_privileges_for_dropped_relation(
                            state, &view.name,
                        );
                        let key = (
                            view.schema_id,
                            crate::CatalogStore::normalize_identifier(&view.name.name),
                        );
                        state.view_names.remove(&key);
                    }
                }

                if let Some(schema) = state.schemas_by_id.remove(&schema_id) {
                    crate::CatalogStore::remove_privileges_for_dropped_schema(state, &schema.name);
                    state
                        .schema_names
                        .remove(&crate::CatalogStore::normalize_identifier(&schema.name));
                }
            }
        }
        WalRecord::CatalogDropTable { table_id_raw, .. } => {
            let table_id = RelationId::new(*table_id_raw);
            if let Some(table) = state.tables_by_id.remove(&table_id) {
                crate::CatalogStore::remove_privileges_for_dropped_relation(state, &table.name);
                let table_key = (
                    table.schema_id,
                    crate::CatalogStore::normalize_identifier(&table.name.name),
                );
                state.table_names.remove(&table_key);
                state.statistics.remove(&table_id);

                if let Some(index_ids) = state.indexes_by_table.remove(&table_id) {
                    for index_id in index_ids {
                        if let Some(index) = state.indexes_by_id.remove(&index_id) {
                            let key = (
                                index.schema_id,
                                crate::CatalogStore::normalize_identifier(&index.name.name),
                            );
                            state.index_names.remove(&key);
                        }
                    }
                }

                let table_name = crate::CatalogStore::normalize_identifier(&table.name.name);
                // Use the same qualified-vs-bare matcher the runtime DROP
                // path uses; comparing normalized names directly leaks
                // triggers stored as `schema.foo` when the table was
                // dropped as bare `foo` (or vice versa).
                state.triggers.retain(|trigger| {
                    !crate::CatalogStore::trigger_table_matches(&trigger.table_name, &table_name)
                });

                let owned_seq_ids: Vec<SequenceId> = state
                    .sequences_by_id
                    .iter()
                    .filter(|(_, seq)| {
                        seq.owned_by
                            .as_ref()
                            .is_some_and(|(owned_table_id, _)| *owned_table_id == table_id)
                    })
                    .map(|(seq_id, _)| *seq_id)
                    .collect();
                for seq_id in owned_seq_ids {
                    if let Some(seq) = state.sequences_by_id.remove(&seq_id) {
                        crate::CatalogStore::remove_privileges_for_dropped_relation(
                            state, &seq.name,
                        );
                        let key = (
                            seq.schema_id,
                            crate::CatalogStore::normalize_identifier(&seq.name.name),
                        );
                        state.sequence_names.remove(&key);
                        state.sequence_values.remove(&seq_id);
                    }
                }
            }
        }
        WalRecord::CatalogDropIndex { index_id_raw, .. } => {
            let index_id = IndexId::new(*index_id_raw);
            if let Some(index) = state.indexes_by_id.remove(&index_id) {
                let key = (
                    index.schema_id,
                    crate::CatalogStore::normalize_identifier(&index.name.name),
                );
                state.index_names.remove(&key);
                let mut remove_table_entry = false;
                if let Some(index_ids) = state.indexes_by_table.get_mut(&index.table_id) {
                    index_ids.retain(|existing| *existing != index_id);
                    remove_table_entry = index_ids.is_empty();
                }
                if remove_table_entry {
                    state.indexes_by_table.remove(&index.table_id);
                }
            }
        }
        WalRecord::CatalogUpdateStatistics {
            descriptor_json, ..
        } => {
            let stats: TableStatistics = from_json(descriptor_json)?;
            // Match the writer guard: don't re-install stats for a table
            // that's no longer present (DROP TABLE replayed first).
            if state.tables_by_id.contains_key(&stats.table_id) {
                state.statistics.insert(stats.table_id, stats);
            }
        }
        WalRecord::CatalogCreateNodeLabel {
            descriptor_json, ..
        } => {
            let desc: NodeLabelDescriptor = from_json(descriptor_json)?;
            let lookup = crate::CatalogStore::normalize_identifier(&desc.label);
            state.node_labels.insert(lookup, desc);
        }
        WalRecord::CatalogCreateEdgeLabel {
            descriptor_json, ..
        } => {
            let desc: EdgeLabelDescriptor = from_json(descriptor_json)?;
            let lookup = crate::CatalogStore::normalize_identifier(&desc.label);
            state.edge_labels.insert(lookup, desc);
        }
        WalRecord::CatalogDropNodeLabel { label_name, .. } => {
            let lookup = crate::CatalogStore::normalize_identifier(label_name);
            state.node_labels.remove(&lookup);
        }
        WalRecord::CatalogDropEdgeLabel { label_name, .. } => {
            let lookup = crate::CatalogStore::normalize_identifier(label_name);
            state.edge_labels.remove(&lookup);
        }
        WalRecord::CatalogCreateRole {
            descriptor_json, ..
        } => {
            let desc: RoleDescriptor = from_json(descriptor_json)?;
            let lookup = crate::CatalogStore::normalize_identifier(&desc.name);
            state.roles.insert(lookup, desc);
        }
        WalRecord::CatalogAlterRole {
            descriptor_json, ..
        } => {
            let desc: RoleDescriptor = from_json(descriptor_json)?;
            let lookup = crate::CatalogStore::normalize_identifier(&desc.name);
            state.roles.insert(lookup, desc);
        }
        WalRecord::CatalogDropRole { role_name, .. } => {
            let lookup = crate::CatalogStore::normalize_identifier(role_name);
            state.roles.remove(&lookup);
            state.privileges.retain(|privilege| {
                if crate::CatalogStore::normalize_identifier(&privilege.role_name) == lookup {
                    return false;
                }
                !crate::CatalogStore::privilege_target_references_role(&privilege.target, &lookup)
            });
        }
        WalRecord::CatalogCreateView {
            descriptor_json, ..
        } => {
            let desc: ViewDescriptor = from_json(descriptor_json)?;
            let view_id = desc.view_id;
            crate::CatalogStore::next_table_id(state, view_id);
            let name_key = (
                desc.schema_id,
                crate::CatalogStore::normalize_identifier(&desc.name.name),
            );
            // Replace existing view with same name if present
            if let Some(&existing_id) = state.view_names.get(&name_key) {
                state.views_by_id.remove(&existing_id);
            }
            state.view_names.insert(name_key, view_id);
            state.views_by_id.insert(view_id, desc);
        }
        WalRecord::CatalogDropView { view_id_raw, .. } => {
            let view_id = aiondb_core::RelationId::new(*view_id_raw);
            if let Some(view) = state.views_by_id.remove(&view_id) {
                crate::CatalogStore::remove_privileges_for_dropped_relation(state, &view.name);
                let key = (
                    view.schema_id,
                    crate::CatalogStore::normalize_identifier(&view.name.name),
                );
                state.view_names.remove(&key);
            }
        }
        WalRecord::CatalogCreateSequence {
            descriptor_json, ..
        } => {
            let desc: SequenceDescriptor = from_json(descriptor_json)?;
            let seq_id = desc.sequence_id;
            crate::CatalogStore::next_sequence_id(state, seq_id);
            let name_key = (
                desc.schema_id,
                crate::CatalogStore::normalize_identifier(&desc.name.name),
            );
            let seq_val = crate::CatalogStore::default_sequence_state(&desc);
            state.sequence_names.insert(name_key, seq_id);
            state.sequence_values.insert(seq_id, seq_val);
            state.sequences_by_id.insert(seq_id, desc);
        }
        WalRecord::CatalogAlterSequence {
            descriptor_json, ..
        } => {
            let desc: SequenceDescriptor = from_json(descriptor_json)?;
            let seq_id = desc.sequence_id;
            // Remove old name key if exists
            if let Some(old) = state.sequences_by_id.get(&seq_id) {
                let old_key = (
                    old.schema_id,
                    crate::CatalogStore::normalize_identifier(&old.name.name),
                );
                state.sequence_names.remove(&old_key);
            }
            let name_key = (
                desc.schema_id,
                crate::CatalogStore::normalize_identifier(&desc.name.name),
            );
            state.sequence_names.insert(name_key, seq_id);
            state.sequences_by_id.insert(seq_id, desc);
        }
        WalRecord::CatalogDropSequence {
            sequence_id_raw, ..
        } => {
            let seq_id = SequenceId::new(*sequence_id_raw);
            if let Some(seq) = state.sequences_by_id.remove(&seq_id) {
                crate::CatalogStore::remove_privileges_for_dropped_relation(state, &seq.name);
                let key = (
                    seq.schema_id,
                    crate::CatalogStore::normalize_identifier(&seq.name.name),
                );
                state.sequence_names.remove(&key);
                state.sequence_values.remove(&seq_id);
            }
        }
        WalRecord::CatalogSetSequenceValue {
            sequence_id_raw,
            current_value,
            is_called,
            ..
        } => {
            let seq_id = SequenceId::new(*sequence_id_raw);
            if !state.sequences_by_id.contains_key(&seq_id) {
                return Err(DbError::internal(format!(
                    "catalog WAL: sequence runtime update references missing sequence {}",
                    seq_id.get()
                )));
            }
            state.sequence_values.insert(
                seq_id,
                crate::SequenceValueState {
                    current_value: *current_value,
                    is_called: *is_called,
                },
            );
        }
        WalRecord::CatalogCreateFunction {
            descriptor_json, ..
        } => {
            let desc: FunctionDescriptor = from_json(descriptor_json)?;
            let lookup = crate::CatalogStore::normalize_identifier(&desc.name);
            let overloads = state.functions.entry(lookup).or_default();
            if let Some(existing) = overloads
                .iter_mut()
                .find(|existing| crate::CatalogStore::same_function_signature(existing, &desc))
            {
                *existing = desc;
            } else {
                overloads.push(desc);
            }
        }
        WalRecord::CatalogDropFunction { function_name, .. } => {
            let lookup = crate::CatalogStore::normalize_identifier(function_name);
            state.functions.remove(&lookup);
            crate::CatalogStore::remove_privileges_for_dropped_function(state, function_name);
        }
        WalRecord::CatalogCreateDomain {
            descriptor_json, ..
        }
        | WalRecord::CatalogAlterDomain {
            descriptor_json, ..
        } => {
            let desc: DomainDescriptor = from_json(descriptor_json)?;
            let lookup = crate::CatalogStore::normalize_identifier(&desc.name);
            state.domains.insert(lookup, desc);
        }
        WalRecord::CatalogDropDomain { domain_name, .. } => {
            let lookup = crate::CatalogStore::normalize_identifier(domain_name);
            state.domains.remove(&lookup);
        }
        WalRecord::CatalogCreateUserType {
            descriptor_json, ..
        }
        | WalRecord::CatalogAlterUserType {
            descriptor_json, ..
        } => {
            let desc: UserTypeDescriptor = from_json(descriptor_json)?;
            let lookup = crate::CatalogStore::normalize_identifier(&desc.name);
            state.user_types.insert(lookup, desc);
        }
        WalRecord::CatalogDropUserType { type_name, .. } => {
            let lookup = crate::CatalogStore::normalize_identifier(type_name);
            state.user_types.remove(&lookup);
        }
        WalRecord::CatalogCreateCast {
            descriptor_json, ..
        } => {
            let desc: CastDescriptor = from_json(descriptor_json)?;
            let key = (
                crate::CatalogStore::normalize_identifier(&desc.source_type),
                crate::CatalogStore::normalize_identifier(&desc.target_type),
            );
            state.casts.insert(key, desc);
        }
        WalRecord::CatalogDropCast {
            source_type,
            target_type,
            ..
        } => {
            let key = (
                crate::CatalogStore::normalize_identifier(source_type),
                crate::CatalogStore::normalize_identifier(target_type),
            );
            state.casts.remove(&key);
        }
        WalRecord::CatalogCreatePolicy {
            descriptor_json, ..
        }
        | WalRecord::CatalogAlterPolicy {
            descriptor_json, ..
        } => {
            let desc: PolicyDescriptor = from_json(descriptor_json)?;
            let key = (
                crate::CatalogStore::normalize_identifier(&desc.name),
                crate::CatalogStore::normalize_identifier(&desc.table_name),
            );
            state.policies.insert(key, desc);
        }
        WalRecord::CatalogDropPolicy {
            policy_name,
            table_name,
            ..
        } => {
            let key = (
                crate::CatalogStore::normalize_identifier(policy_name),
                crate::CatalogStore::normalize_identifier(table_name),
            );
            state.policies.remove(&key);
        }
        WalRecord::CatalogCreateRule {
            descriptor_json, ..
        } => {
            let desc: RuleDescriptor = from_json(descriptor_json)?;
            let key = (
                crate::CatalogStore::normalize_identifier(&desc.name),
                crate::CatalogStore::normalize_identifier(&desc.table_name),
            );
            state.rules.insert(key, desc);
        }
        WalRecord::CatalogDropRule {
            rule_name,
            table_name,
            ..
        } => {
            let key = (
                crate::CatalogStore::normalize_identifier(rule_name),
                crate::CatalogStore::normalize_identifier(table_name),
            );
            state.rules.remove(&key);
        }
        WalRecord::CatalogSetComment {
            descriptor_json, ..
        } => {
            let desc: CommentDescriptor = from_json(descriptor_json)?;
            state
                .comments
                .insert((desc.object_type, desc.object_identity), desc.comment);
        }
        WalRecord::CatalogDropComment {
            object_type,
            object_identity,
            ..
        } => {
            state
                .comments
                .remove(&(object_type.clone(), object_identity.clone()));
        }
        WalRecord::CatalogCreateTrigger {
            descriptor_json, ..
        } => {
            let desc: TriggerDescriptor = from_json(descriptor_json)?;
            // Remove existing trigger with same name+table if present
            let trig_name = crate::CatalogStore::normalize_identifier(&desc.name);
            state.triggers.retain(|t| {
                !(crate::CatalogStore::normalize_identifier(&t.name) == trig_name
                    && crate::CatalogStore::trigger_table_matches(&t.table_name, &desc.table_name))
            });
            state.triggers.push(desc);
        }
        WalRecord::CatalogDropTrigger {
            trigger_name,
            table_name,
            ..
        } => {
            let trig_name = crate::CatalogStore::normalize_identifier(trigger_name);
            state.triggers.retain(|t| {
                !(crate::CatalogStore::normalize_identifier(&t.name) == trig_name
                    && crate::CatalogStore::trigger_table_matches(&t.table_name, table_name))
            });
        }
        WalRecord::CatalogGrantPrivilege {
            descriptor_json, ..
        } => {
            let desc = crate::CatalogStore::canonicalize_function_privilege(from_json::<
                PrivilegeDescriptor,
            >(
                descriptor_json
            )?);
            if !state.privileges.contains(&desc) {
                state.privileges.push(desc);
            }
        }
        WalRecord::CatalogRevokePrivilege {
            descriptor_json, ..
        } => {
            let desc = crate::CatalogStore::canonicalize_function_privilege(from_json::<
                PrivilegeDescriptor,
            >(
                descriptor_json
            )?);
            state
                .privileges
                .retain(|p| !crate::CatalogStore::privilege_matches_revoke(p, &desc));
        }
        // Non-catalog records are ignored
        _ => {}
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use aiondb_catalog::{
        CatalogPrivilege, FunctionPrivilegeTarget, PrivilegeDescriptor, PrivilegeTarget,
        QualifiedName, SchemaDescriptor,
    };
    use aiondb_core::SchemaId;

    #[test]
    fn roundtrip_schema_descriptor() {
        let desc = SchemaDescriptor {
            schema_id: SchemaId::new(42),
            name: "test_schema".to_owned(),
        };
        let json = to_json(&desc).unwrap();
        let recovered: SchemaDescriptor = from_json(&json).unwrap();
        assert_eq!(desc, recovered);
    }

    #[test]
    fn roundtrip_table_descriptor() {
        use aiondb_catalog::{ColumnDescriptor, TableDescriptor};
        use aiondb_core::{ColumnId, DataType, RelationId};

        let desc = TableDescriptor {
            table_id: RelationId::new(1),
            schema_id: SchemaId::new(1),
            name: QualifiedName::qualified("public", "users"),
            columns: vec![ColumnDescriptor {
                column_id: ColumnId::new(1),
                name: "id".to_owned(),
                data_type: DataType::Int,
                raw_type_name: None,
                text_type_modifier: None,
                nullable: false,
                ordinal_position: 1,
                default_value: None,
            }],
            identity_columns: Vec::new(),
            primary_key: None,
            foreign_keys: vec![],
            check_constraints: vec![],
            shard_config: None,
            owner: None,
        };
        let json = to_json(&desc).unwrap();
        let recovered: TableDescriptor = from_json(&json).unwrap();
        assert_eq!(desc, recovered);
    }

    #[test]
    fn roundtrip_role_descriptor() {
        let desc = RoleDescriptor {
            name: "admin".to_owned(),
            login: true,
            superuser: true,
            password_hash: Some("hash123".to_owned()),
            ..RoleDescriptor::default()
        };
        let json = to_json(&desc).unwrap();
        let recovered: RoleDescriptor = from_json(&json).unwrap();
        assert_eq!(desc, recovered);
    }

    #[test]
    fn replay_create_schema() {
        let mut state = CatalogState::default();
        let desc = SchemaDescriptor {
            schema_id: SchemaId::new(10),
            name: "myschema".to_owned(),
        };
        let record = create_schema_record(TxnId::new(1), &desc).unwrap();
        replay_catalog_record(&mut state, &record).unwrap();

        assert!(state.schema_names.contains_key("myschema"));
        let sid = state.schema_names["myschema"];
        assert_eq!(state.schemas_by_id[&sid].name, "myschema");
    }

    #[test]
    fn replay_create_then_drop_schema() {
        let mut state = CatalogState::default();
        let desc = SchemaDescriptor {
            schema_id: SchemaId::new(5),
            name: "dropme".to_owned(),
        };
        let create = create_schema_record(TxnId::new(1), &desc).unwrap();
        replay_catalog_record(&mut state, &create).unwrap();
        assert!(state.schema_names.contains_key("dropme"));

        let drop_rec = WalRecord::CatalogDropSchema {
            txn_id: TxnId::new(2),
            schema_id_raw: SchemaId::new(5).get(),
        };
        replay_catalog_record(&mut state, &drop_rec).unwrap();
        assert!(!state.schema_names.contains_key("dropme"));
        assert!(!state.schemas_by_id.contains_key(&SchemaId::new(5)));
    }

    #[test]
    fn replay_create_role() {
        let mut state = CatalogState::default();
        let desc = RoleDescriptor {
            name: "user1".to_owned(),
            login: true,
            superuser: false,
            password_hash: None,
            ..RoleDescriptor::default()
        };
        let record = create_role_record(TxnId::new(1), &desc).unwrap();
        replay_catalog_record(&mut state, &record).unwrap();
        assert!(state.roles.contains_key("user1"));
    }

    #[test]
    fn replay_drop_role() {
        let mut state = CatalogState::default();
        let desc = RoleDescriptor {
            name: "user1".to_owned(),
            login: true,
            superuser: false,
            password_hash: None,
            ..RoleDescriptor::default()
        };
        let create = create_role_record(TxnId::new(1), &desc).unwrap();
        replay_catalog_record(&mut state, &create).unwrap();

        let drop_rec = drop_role_record(TxnId::new(2), "user1");
        replay_catalog_record(&mut state, &drop_rec).unwrap();
        assert!(!state.roles.contains_key("user1"));
    }

    #[test]
    fn replay_ignores_non_catalog_records() {
        let mut state = CatalogState::default();
        let record = WalRecord::Checkpoint {
            last_committed_lsn: aiondb_wal::Lsn::ZERO,
        };
        replay_catalog_record(&mut state, &record).unwrap();
        // State should be unchanged
        assert!(state.schemas_by_id.is_empty());
    }

    #[test]
    fn replay_migrates_table_form_function_grant_and_modern_revoke_removes_it() {
        let mut state = CatalogState::default();

        let table_form_grant = PrivilegeDescriptor {
            role_name: "reader".to_owned(),
            privilege: CatalogPrivilege::Execute,
            target: PrivilegeTarget::Table(QualifiedName::qualified("public", "function_v1")),
        };
        let grant_record = grant_privilege_record(TxnId::new(10), &table_form_grant).unwrap();
        replay_catalog_record(&mut state, &grant_record).unwrap();
        assert_eq!(
            state.privileges,
            vec![PrivilegeDescriptor {
                role_name: "reader".to_owned(),
                privilege: CatalogPrivilege::Execute,
                target: PrivilegeTarget::Function(FunctionPrivilegeTarget {
                    name: QualifiedName::qualified("public", "function_v1"),
                    arg_types: None,
                }),
            }]
        );

        let modern_revoke = PrivilegeDescriptor {
            role_name: "reader".to_owned(),
            privilege: CatalogPrivilege::Execute,
            target: PrivilegeTarget::Function(FunctionPrivilegeTarget {
                name: QualifiedName::qualified("public", "function_v1"),
                arg_types: None,
            }),
        };
        let revoke_record = revoke_privilege_record(TxnId::new(11), &modern_revoke).unwrap();
        replay_catalog_record(&mut state, &revoke_record).unwrap();
        assert!(state.privileges.is_empty());
    }

    #[test]
    fn replay_signature_revoke_removes_migrated_table_form_function_grant() {
        let mut state = CatalogState::default();

        let table_form_grant = PrivilegeDescriptor {
            role_name: "reader".to_owned(),
            privilege: CatalogPrivilege::Execute,
            target: PrivilegeTarget::Table(QualifiedName::qualified("public", "function_v1")),
        };
        let grant_record = grant_privilege_record(TxnId::new(20), &table_form_grant).unwrap();
        replay_catalog_record(&mut state, &grant_record).unwrap();

        let signature_revoke = PrivilegeDescriptor {
            role_name: "reader".to_owned(),
            privilege: CatalogPrivilege::Execute,
            target: PrivilegeTarget::Function(FunctionPrivilegeTarget {
                name: QualifiedName::qualified("public", "function_v1"),
                arg_types: Some(vec![aiondb_core::DataType::Int]),
            }),
        };
        let revoke_record = revoke_privilege_record(TxnId::new(21), &signature_revoke).unwrap();
        replay_catalog_record(&mut state, &revoke_record).unwrap();
        assert!(state.privileges.is_empty());
    }
}
