use super::*;
use aiondb_storage_api::Bound;
use std::fs;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use aiondb_catalog::{
    CatalogReader, CatalogTxnParticipant, CatalogWriter, IndexDescriptor, QualifiedName,
    SchemaDescriptor, SequenceManager, TableDescriptor, TableStatistics,
};
use aiondb_core::{
    ColumnId, DataType, DbError, DbResult, IndexId, RelationId, Row, SchemaId, SequenceId, TupleId,
    TxnId, Value,
};
use aiondb_graph::{NeighborCursor, OwnedCursor};
use aiondb_plan::{
    ColumnPlan, PhysicalPlan, ProjectionExpr, ResultField, ScanAccessPath, TypedExpr,
    UpdateAssignment,
};
use aiondb_storage_api::{
    CheckpointInfo, KeyRange, StorageDDL, StorageDML, StorageTxnParticipant,
    TableStorageDescriptor, TupleRecord, TupleStream, VecTupleStream,
};
use aiondb_tx::{IsolationLevel, Snapshot};

use crate::{ExecutionContext, ExecutionResult};

// =========================================================================
// Mock implementations
// =========================================================================

/// In-memory catalog that supports creating, reading, and dropping tables.
#[derive(Debug)]
struct MockCatalog {
    schemas: Mutex<Vec<SchemaDescriptor>>,
    tables: Mutex<Vec<TableDescriptor>>,
    indexes: Mutex<Vec<IndexDescriptor>>,
    sequences: Mutex<Vec<aiondb_catalog::SequenceDescriptor>>,
    node_labels: Mutex<Vec<aiondb_catalog::NodeLabelDescriptor>>,
    edge_labels: Mutex<Vec<aiondb_catalog::EdgeLabelDescriptor>>,
    next_schema_id: Mutex<u64>,
    next_table_id: Mutex<u64>,
    next_column_id: Mutex<u64>,
    next_index_id: Mutex<u64>,
    next_sequence_id: Mutex<u64>,
    rollback_savepoint_error: Mutex<Option<DbError>>,
    release_savepoint_error: Mutex<Option<DbError>>,
}

impl MockCatalog {
    fn new() -> Self {
        Self {
            schemas: Mutex::new(vec![SchemaDescriptor {
                schema_id: SchemaId::new(1),
                name: "public".to_owned(),
            }]),
            tables: Mutex::new(Vec::new()),
            indexes: Mutex::new(Vec::new()),
            sequences: Mutex::new(Vec::new()),
            node_labels: Mutex::new(Vec::new()),
            edge_labels: Mutex::new(Vec::new()),
            next_schema_id: Mutex::new(2),
            next_table_id: Mutex::new(1),
            next_column_id: Mutex::new(1),
            next_index_id: Mutex::new(1),
            next_sequence_id: Mutex::new(1),
            rollback_savepoint_error: Mutex::new(None),
            release_savepoint_error: Mutex::new(None),
        }
    }

    fn fail_next_release_savepoint(&self, error: DbError) {
        *self.release_savepoint_error.lock().unwrap() = Some(error);
    }
}

impl CatalogReader for MockCatalog {
    fn get_schema(
        &self,
        _txn: TxnId,
        name: &QualifiedName,
    ) -> DbResult<Option<aiondb_catalog::SchemaDescriptor>> {
        let schemas = self.schemas.lock().unwrap();
        Ok(schemas
            .iter()
            .find(|schema| schema.name.eq_ignore_ascii_case(name.object_name()))
            .cloned())
    }

    fn get_table(&self, _txn: TxnId, name: &QualifiedName) -> DbResult<Option<TableDescriptor>> {
        let tables = self.tables.lock().unwrap();
        Ok(tables.iter().find(|t| t.name == *name).cloned())
    }

    fn get_table_by_id(
        &self,
        _txn: TxnId,
        table_id: RelationId,
    ) -> DbResult<Option<TableDescriptor>> {
        let tables = self.tables.lock().unwrap();
        Ok(tables.iter().find(|t| t.table_id == table_id).cloned())
    }

    fn list_tables(&self, _txn: TxnId, schema_id: SchemaId) -> DbResult<Vec<TableDescriptor>> {
        let tables = self.tables.lock().unwrap();
        Ok(tables
            .iter()
            .filter(|t| t.schema_id == schema_id)
            .cloned()
            .collect())
    }

    fn list_indexes(&self, _txn: TxnId, table_id: RelationId) -> DbResult<Vec<IndexDescriptor>> {
        let indexes = self.indexes.lock().unwrap();
        Ok(indexes
            .iter()
            .filter(|i| i.table_id == table_id)
            .cloned()
            .collect())
    }

    fn get_index(&self, _txn: TxnId, index_id: IndexId) -> DbResult<Option<IndexDescriptor>> {
        let indexes = self.indexes.lock().unwrap();
        Ok(indexes.iter().find(|i| i.index_id == index_id).cloned())
    }

    fn get_sequence(
        &self,
        _txn: TxnId,
        name: &QualifiedName,
    ) -> DbResult<Option<aiondb_catalog::SequenceDescriptor>> {
        let sequences = self.sequences.lock().unwrap();
        Ok(sequences.iter().find(|s| s.name == *name).cloned())
    }

    fn get_statistics(
        &self,
        _txn: TxnId,
        _table_id: RelationId,
    ) -> DbResult<Option<TableStatistics>> {
        Ok(None)
    }

    fn get_view(
        &self,
        _txn: TxnId,
        _name: &QualifiedName,
    ) -> DbResult<Option<aiondb_catalog::ViewDescriptor>> {
        Ok(None)
    }

    fn list_views(
        &self,
        _txn: TxnId,
        _schema_id: SchemaId,
    ) -> DbResult<Vec<aiondb_catalog::ViewDescriptor>> {
        Ok(Vec::new())
    }

    fn list_node_labels(&self, _txn: TxnId) -> DbResult<Vec<aiondb_catalog::NodeLabelDescriptor>> {
        Ok(self.node_labels.lock().unwrap().clone())
    }

    fn get_node_label(
        &self,
        _txn: TxnId,
        label: &str,
    ) -> DbResult<Option<aiondb_catalog::NodeLabelDescriptor>> {
        Ok(self
            .node_labels
            .lock()
            .unwrap()
            .iter()
            .find(|entry| entry.label.eq_ignore_ascii_case(label))
            .cloned())
    }

    fn list_edge_labels(&self, _txn: TxnId) -> DbResult<Vec<aiondb_catalog::EdgeLabelDescriptor>> {
        Ok(self.edge_labels.lock().unwrap().clone())
    }

    fn get_edge_label(
        &self,
        _txn: TxnId,
        label: &str,
    ) -> DbResult<Option<aiondb_catalog::EdgeLabelDescriptor>> {
        Ok(self
            .edge_labels
            .lock()
            .unwrap()
            .iter()
            .find(|entry| entry.label.eq_ignore_ascii_case(label))
            .cloned())
    }
}

impl CatalogWriter for MockCatalog {
    fn create_schema(
        &self,
        _txn: TxnId,
        mut schema: aiondb_catalog::SchemaDescriptor,
    ) -> DbResult<SchemaId> {
        let mut next_id = self.next_schema_id.lock().unwrap();
        let schema_id = SchemaId::new(*next_id);
        *next_id += 1;
        schema.schema_id = schema_id;
        self.schemas.lock().unwrap().push(schema);
        Ok(schema_id)
    }

    fn drop_schema(&self, _txn: TxnId, schema_id: SchemaId) -> DbResult<()> {
        let mut schemas = self.schemas.lock().unwrap();
        let initial_len = schemas.len();
        schemas.retain(|schema| schema.schema_id != schema_id);
        if schemas.len() == initial_len {
            return Err(DbError::internal("schema not found"));
        }
        Ok(())
    }

    fn create_table(&self, _txn: TxnId, mut table: TableDescriptor) -> DbResult<RelationId> {
        let mut next_id = self.next_table_id.lock().unwrap();
        let table_id = RelationId::new(*next_id);
        *next_id += 1;
        table.table_id = table_id;

        // Assign column IDs
        let mut col_id_gen = self.next_column_id.lock().unwrap();
        for col in &mut table.columns {
            col.column_id = ColumnId::new(*col_id_gen);
            *col_id_gen += 1;
        }

        self.tables.lock().unwrap().push(table);
        Ok(table_id)
    }

    fn create_index(&self, _txn: TxnId, mut index: IndexDescriptor) -> DbResult<IndexId> {
        let mut next_id = self.next_index_id.lock().unwrap();
        let index_id = IndexId::new(*next_id);
        *next_id += 1;
        index.index_id = index_id;
        self.indexes.lock().unwrap().push(index);
        Ok(index_id)
    }

    fn create_sequence(
        &self,
        _txn: TxnId,
        mut sequence: aiondb_catalog::SequenceDescriptor,
    ) -> DbResult<SequenceId> {
        let mut next_id = self.next_sequence_id.lock().unwrap();
        let sequence_id = SequenceId::new(*next_id);
        *next_id += 1;
        sequence.sequence_id = sequence_id;
        self.sequences.lock().unwrap().push(sequence);
        Ok(sequence_id)
    }

    fn alter_table(
        &self,
        _txn: TxnId,
        table_id: RelationId,
        alteration: aiondb_catalog::TableAlteration,
    ) -> DbResult<()> {
        let mut tables = self.tables.lock().unwrap();
        let table = tables
            .iter_mut()
            .find(|t| t.table_id == table_id)
            .ok_or_else(|| DbError::internal("table not found"))?;
        match alteration {
            aiondb_catalog::TableAlteration::AddColumn(mut col) => {
                let mut col_id_gen = self.next_column_id.lock().unwrap();
                col.column_id = ColumnId::new(*col_id_gen);
                *col_id_gen += 1;
                col.ordinal_position =
                    u32::try_from(table.columns.len().saturating_add(1)).unwrap_or(u32::MAX);
                table.columns.push(col);
            }
            aiondb_catalog::TableAlteration::DropColumn { column_id } => {
                table.columns.retain(|c| c.column_id != column_id);
            }
            aiondb_catalog::TableAlteration::RenameTable { new_name } => {
                table.name = new_name;
            }
            aiondb_catalog::TableAlteration::RenameColumn {
                column_id,
                new_name,
            } => {
                if let Some(col) = table.columns.iter_mut().find(|c| c.column_id == column_id) {
                    col.name = new_name;
                }
            }
            aiondb_catalog::TableAlteration::SetDefault {
                column_id,
                default_expr,
            } => {
                if let Some(col) = table.columns.iter_mut().find(|c| c.column_id == column_id) {
                    col.default_value = Some(default_expr);
                }
            }
            aiondb_catalog::TableAlteration::DropDefault { column_id } => {
                if let Some(col) = table.columns.iter_mut().find(|c| c.column_id == column_id) {
                    col.default_value = None;
                }
            }
            aiondb_catalog::TableAlteration::SetNotNull { column_id } => {
                if let Some(col) = table.columns.iter_mut().find(|c| c.column_id == column_id) {
                    col.nullable = false;
                }
            }
            aiondb_catalog::TableAlteration::DropNotNull { column_id } => {
                if let Some(col) = table.columns.iter_mut().find(|c| c.column_id == column_id) {
                    col.nullable = true;
                }
            }
            aiondb_catalog::TableAlteration::AddConstraint {
                constraint_type,
                constraint_name,
                columns,
                check_expr,
                ref_table,
                ref_columns,
                ..
            } => match constraint_type.as_str() {
                "PRIMARY KEY" => {
                    let pk_ids: Vec<ColumnId> = columns
                        .iter()
                        .filter_map(|name| {
                            table
                                .columns
                                .iter()
                                .find(|c| c.name.eq_ignore_ascii_case(name))
                                .map(|c| c.column_id)
                        })
                        .collect();
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
                    if let Some(ref_table_name) = ref_table {
                        table
                            .foreign_keys
                            .push(aiondb_catalog::ForeignKeyConstraint {
                                columns,
                                referenced_table: ref_table_name,
                                referenced_columns: ref_columns,
                                on_delete: aiondb_core::FkAction::NoAction,
                                on_update: aiondb_core::FkAction::NoAction,
                                on_delete_set_columns: Vec::new(),
                                on_update_set_columns: Vec::new(),
                                match_type: aiondb_core::FkMatchType::Simple,
                                name: constraint_name,
                            });
                    }
                }
                _ => {}
            },
            aiondb_catalog::TableAlteration::DropConstraint { constraint_name } => {
                table.check_constraints.retain(|c| {
                    c.name
                        .as_ref()
                        .map_or(true, |n| !n.eq_ignore_ascii_case(&constraint_name))
                });
            }
            aiondb_catalog::TableAlteration::AlterColumnType {
                column_id,
                new_type,
                raw_type_name,
                text_type_modifier,
            } => {
                if let Some(col) = table.columns.iter_mut().find(|c| c.column_id == column_id) {
                    col.data_type = new_type;
                    col.raw_type_name = raw_type_name;
                    col.text_type_modifier = text_type_modifier;
                }
            }
        }
        Ok(())
    }

    fn alter_index(
        &self,
        _txn: TxnId,
        _index_id: IndexId,
        _alteration: aiondb_catalog::IndexAlteration,
    ) -> DbResult<()> {
        Ok(())
    }

    fn alter_sequence(
        &self,
        _txn: TxnId,
        _sequence_id: SequenceId,
        _alteration: aiondb_catalog::SequenceAlteration,
    ) -> DbResult<()> {
        Ok(())
    }

    fn drop_table(&self, _txn: TxnId, table_id: RelationId) -> DbResult<()> {
        let mut tables = self.tables.lock().unwrap();
        tables.retain(|t| t.table_id != table_id);
        Ok(())
    }

    fn drop_index(&self, _txn: TxnId, index_id: IndexId) -> DbResult<()> {
        let mut indexes = self.indexes.lock().unwrap();
        indexes.retain(|i| i.index_id != index_id);
        Ok(())
    }

    fn drop_sequence(&self, _txn: TxnId, sequence_id: SequenceId) -> DbResult<()> {
        let mut sequences = self.sequences.lock().unwrap();
        sequences.retain(|s| s.sequence_id != sequence_id);
        Ok(())
    }

    fn update_statistics(&self, _txn: TxnId, _stats: TableStatistics) -> DbResult<()> {
        Ok(())
    }

    fn create_node_label(
        &self,
        _txn: TxnId,
        label: aiondb_catalog::NodeLabelDescriptor,
    ) -> DbResult<()> {
        self.node_labels.lock().unwrap().push(label);
        Ok(())
    }

    fn create_edge_label(
        &self,
        _txn: TxnId,
        label: aiondb_catalog::EdgeLabelDescriptor,
    ) -> DbResult<()> {
        self.edge_labels.lock().unwrap().push(label);
        Ok(())
    }

    fn drop_node_label(&self, _txn: TxnId, label: &str) -> DbResult<()> {
        self.node_labels
            .lock()
            .unwrap()
            .retain(|entry| !entry.label.eq_ignore_ascii_case(label));
        Ok(())
    }

    fn drop_edge_label(&self, _txn: TxnId, label: &str) -> DbResult<()> {
        self.edge_labels
            .lock()
            .unwrap()
            .retain(|entry| !entry.label.eq_ignore_ascii_case(label));
        Ok(())
    }

    fn create_view(
        &self,
        _txn: TxnId,
        _view: aiondb_catalog::ViewDescriptor,
    ) -> DbResult<RelationId> {
        Ok(RelationId::new(100))
    }

    fn drop_view(&self, _txn: TxnId, _view_id: RelationId) -> DbResult<()> {
        Ok(())
    }

    fn create_role(&self, _txn: TxnId, _role: aiondb_catalog::RoleDescriptor) -> DbResult<()> {
        Err(DbError::internal("mock catalog does not implement roles"))
    }

    fn alter_role(
        &self,
        _txn: TxnId,
        _name: &str,
        _role: aiondb_catalog::RoleDescriptor,
    ) -> DbResult<()> {
        Err(DbError::internal("mock catalog does not implement roles"))
    }

    fn drop_role(&self, _txn: TxnId, _name: &str) -> DbResult<()> {
        Err(DbError::internal("mock catalog does not implement roles"))
    }

    fn grant_privilege(
        &self,
        _txn: TxnId,
        _privilege: aiondb_catalog::PrivilegeDescriptor,
    ) -> DbResult<()> {
        Err(DbError::internal(
            "mock catalog does not implement privileges",
        ))
    }

    fn revoke_privilege(
        &self,
        _txn: TxnId,
        _privilege: aiondb_catalog::PrivilegeDescriptor,
    ) -> DbResult<()> {
        Err(DbError::internal(
            "mock catalog does not implement privileges",
        ))
    }

    fn create_tenant(
        &self,
        _txn: TxnId,
        _name: &str,
    ) -> DbResult<aiondb_catalog::TenantDescriptor> {
        Err(DbError::internal("mock catalog does not implement tenants"))
    }

    fn drop_tenant(&self, _txn: TxnId, _name: &str) -> DbResult<()> {
        Err(DbError::internal("mock catalog does not implement tenants"))
    }

    fn create_function(
        &self,
        _txn: TxnId,
        _func: aiondb_catalog::FunctionDescriptor,
    ) -> DbResult<()> {
        Err(DbError::internal(
            "mock catalog does not implement functions",
        ))
    }

    fn drop_function(&self, _txn: TxnId, _name: &str) -> DbResult<()> {
        Err(DbError::internal(
            "mock catalog does not implement functions",
        ))
    }

    fn create_domain(
        &self,
        _txn: TxnId,
        _domain: aiondb_catalog::DomainDescriptor,
    ) -> DbResult<()> {
        Err(DbError::internal("mock catalog does not implement domains"))
    }

    fn drop_domain(&self, _txn: TxnId, _name: &str) -> DbResult<()> {
        Err(DbError::internal("mock catalog does not implement domains"))
    }

    fn alter_domain(&self, _txn: TxnId, _domain: aiondb_catalog::DomainDescriptor) -> DbResult<()> {
        Err(DbError::internal("mock catalog does not implement domains"))
    }

    fn create_user_type(
        &self,
        _txn: TxnId,
        _user_type: aiondb_catalog::UserTypeDescriptor,
    ) -> DbResult<()> {
        Err(DbError::internal(
            "mock catalog does not implement user types",
        ))
    }

    fn drop_user_type(&self, _txn: TxnId, _name: &str) -> DbResult<()> {
        Err(DbError::internal(
            "mock catalog does not implement user types",
        ))
    }

    fn alter_user_type(
        &self,
        _txn: TxnId,
        _user_type: aiondb_catalog::UserTypeDescriptor,
    ) -> DbResult<()> {
        Err(DbError::internal(
            "mock catalog does not implement user types",
        ))
    }

    fn create_cast(&self, _txn: TxnId, _cast: aiondb_catalog::CastDescriptor) -> DbResult<()> {
        Err(DbError::internal("mock catalog does not implement casts"))
    }

    fn drop_cast(&self, _txn: TxnId, _source_type: &str, _target_type: &str) -> DbResult<()> {
        Err(DbError::internal("mock catalog does not implement casts"))
    }

    fn create_policy(
        &self,
        _txn: TxnId,
        _policy: aiondb_catalog::PolicyDescriptor,
    ) -> DbResult<()> {
        Err(DbError::internal(
            "mock catalog does not implement policies",
        ))
    }

    fn drop_policy(&self, _txn: TxnId, _name: &str, _table_name: &str) -> DbResult<()> {
        Err(DbError::internal(
            "mock catalog does not implement policies",
        ))
    }

    fn alter_policy(&self, _txn: TxnId, _policy: aiondb_catalog::PolicyDescriptor) -> DbResult<()> {
        Err(DbError::internal(
            "mock catalog does not implement policies",
        ))
    }

    fn create_rule(&self, _txn: TxnId, _rule: aiondb_catalog::RuleDescriptor) -> DbResult<()> {
        Err(DbError::internal("mock catalog does not implement rules"))
    }

    fn drop_rule(&self, _txn: TxnId, _name: &str, _table_name: &str) -> DbResult<()> {
        Err(DbError::internal("mock catalog does not implement rules"))
    }

    fn create_trigger(
        &self,
        _txn: TxnId,
        _trigger: aiondb_catalog::TriggerDescriptor,
    ) -> DbResult<()> {
        Err(DbError::internal(
            "mock catalog does not implement triggers",
        ))
    }

    fn drop_trigger(&self, _txn: TxnId, _name: &str, _table_name: &str) -> DbResult<()> {
        Err(DbError::internal(
            "mock catalog does not implement triggers",
        ))
    }

    fn rename_trigger(
        &self,
        _txn: TxnId,
        _name: &str,
        _table_name: &str,
        _new_name: &str,
    ) -> DbResult<()> {
        Err(DbError::internal(
            "mock catalog does not implement triggers",
        ))
    }
}

impl SequenceManager for MockCatalog {
    fn next_value(&self, _txn: TxnId, _sequence_id: SequenceId) -> DbResult<i64> {
        Ok(1)
    }

    fn set_value(
        &self,
        _txn: TxnId,
        _sequence_id: SequenceId,
        _value: i64,
        _is_called: bool,
    ) -> DbResult<()> {
        Ok(())
    }
}

impl CatalogTxnParticipant for MockCatalog {
    fn begin_txn(&self, _txn: TxnId) -> DbResult<()> {
        Ok(())
    }

    fn commit_txn(&self, _txn: TxnId) -> DbResult<()> {
        Ok(())
    }

    fn rollback_txn(&self, _txn: TxnId) -> DbResult<()> {
        Ok(())
    }

    fn create_savepoint(&self, _txn: TxnId) -> DbResult<u64> {
        Ok(1)
    }

    fn rollback_to_savepoint(&self, _txn: TxnId, _savepoint_id: u64) -> DbResult<()> {
        if let Some(error) = self.rollback_savepoint_error.lock().unwrap().take() {
            Err(error)
        } else {
            Ok(())
        }
    }

    fn release_savepoint(&self, _txn: TxnId, _savepoint_id: u64) -> DbResult<()> {
        if let Some(error) = self.release_savepoint_error.lock().unwrap().take() {
            Err(error)
        } else {
            Ok(())
        }
    }
}

/// In-memory storage engine that stores rows per table.
struct MockStorage {
    tables: Mutex<std::collections::HashMap<RelationId, Vec<(TupleId, Row)>>>,
    table_descriptors:
        Mutex<std::collections::HashMap<RelationId, aiondb_storage_api::TableStorageDescriptor>>,
    created_index_ids: Mutex<Vec<IndexId>>,
    registered_edge_tables: Mutex<Vec<(RelationId, usize, usize)>>,
    table_scan_counts: Mutex<std::collections::HashMap<RelationId, usize>>,
    fetch_projection_widths: Mutex<std::collections::HashMap<RelationId, Vec<usize>>>,
    adjacency_edge_cursor_counts: Mutex<std::collections::HashMap<RelationId, usize>>,
    adjacency_neighbor_cursor_counts: Mutex<std::collections::HashMap<RelationId, usize>>,
    adjacency_edge_counts: Mutex<std::collections::HashMap<RelationId, usize>>,
    adjacency_weighted_edge_counts: Mutex<std::collections::HashMap<RelationId, usize>>,
    graph_projection_cache: Mutex<std::collections::HashMap<(String, String, u64), Vec<u8>>>,
    cache_generation: Mutex<Option<u64>>,
    next_tuple_id: Mutex<u64>,
    rollback_savepoint_error: Mutex<Option<DbError>>,
    release_savepoint_error: Mutex<Option<DbError>>,
}

impl MockStorage {
    fn new() -> Self {
        Self {
            tables: Mutex::new(std::collections::HashMap::new()),
            table_descriptors: Mutex::new(std::collections::HashMap::new()),
            created_index_ids: Mutex::new(Vec::new()),
            registered_edge_tables: Mutex::new(Vec::new()),
            table_scan_counts: Mutex::new(std::collections::HashMap::new()),
            fetch_projection_widths: Mutex::new(std::collections::HashMap::new()),
            adjacency_edge_cursor_counts: Mutex::new(std::collections::HashMap::new()),
            adjacency_neighbor_cursor_counts: Mutex::new(std::collections::HashMap::new()),
            adjacency_edge_counts: Mutex::new(std::collections::HashMap::new()),
            adjacency_weighted_edge_counts: Mutex::new(std::collections::HashMap::new()),
            graph_projection_cache: Mutex::new(std::collections::HashMap::new()),
            cache_generation: Mutex::new(None),
            next_tuple_id: Mutex::new(1),
            rollback_savepoint_error: Mutex::new(None),
            release_savepoint_error: Mutex::new(None),
        }
    }

    fn fail_next_rollback_savepoint(&self, error: DbError) {
        *self.rollback_savepoint_error.lock().unwrap() = Some(error);
    }

    fn set_cache_generation(&self, generation: Option<u64>) {
        *self.cache_generation.lock().unwrap() = generation;
    }

    fn table_scan_count(&self, table_id: RelationId) -> usize {
        self.table_scan_counts
            .lock()
            .unwrap()
            .get(&table_id)
            .copied()
            .unwrap_or(0)
    }

    fn adjacency_edge_count(&self, table_id: RelationId) -> usize {
        self.adjacency_edge_counts
            .lock()
            .unwrap()
            .get(&table_id)
            .copied()
            .unwrap_or(0)
    }

    fn adjacency_edge_cursor_count(&self, table_id: RelationId) -> usize {
        self.adjacency_edge_cursor_counts
            .lock()
            .unwrap()
            .get(&table_id)
            .copied()
            .unwrap_or(0)
    }

    fn fetch_projection_widths(&self, table_id: RelationId) -> Vec<usize> {
        self.fetch_projection_widths
            .lock()
            .unwrap()
            .get(&table_id)
            .cloned()
            .unwrap_or_default()
    }

    fn adjacency_weighted_edge_count(&self, table_id: RelationId) -> usize {
        self.adjacency_weighted_edge_counts
            .lock()
            .unwrap()
            .get(&table_id)
            .copied()
            .unwrap_or(0)
    }

    fn reset_graph_access_counts(&self) {
        self.table_scan_counts.lock().unwrap().clear();
        self.fetch_projection_widths.lock().unwrap().clear();
        self.adjacency_edge_cursor_counts.lock().unwrap().clear();
        self.adjacency_neighbor_cursor_counts
            .lock()
            .unwrap()
            .clear();
        self.adjacency_edge_counts.lock().unwrap().clear();
        self.adjacency_weighted_edge_counts.lock().unwrap().clear();
    }

    fn overwrite_graph_projection_cache_payloads(
        &self,
        namespace: &str,
        generation: u64,
        payload: &[u8],
    ) -> usize {
        let mut cache = self.graph_projection_cache.lock().unwrap();
        let mut replaced = 0;
        for ((entry_namespace, _cache_key, entry_generation), entry_payload) in cache.iter_mut() {
            if entry_namespace == namespace && *entry_generation == generation {
                *entry_payload = payload.to_vec();
                replaced += 1;
            }
        }
        replaced
    }

    fn graph_projection_cache_payloads(&self, namespace: &str, generation: u64) -> Vec<Vec<u8>> {
        self.graph_projection_cache
            .lock()
            .unwrap()
            .iter()
            .filter_map(
                |((entry_namespace, _cache_key, entry_generation), payload)| {
                    if entry_namespace == namespace && *entry_generation == generation {
                        Some(payload.clone())
                    } else {
                        None
                    }
                },
            )
            .collect()
    }
}

impl StorageDDL for MockStorage {
    fn create_table_storage(&self, _txn: TxnId, table: &TableStorageDescriptor) -> DbResult<()> {
        self.tables
            .lock()
            .unwrap()
            .insert(table.table_id, Vec::new());
        self.table_descriptors
            .lock()
            .unwrap()
            .insert(table.table_id, table.clone());
        Ok(())
    }

    fn create_index_storage(
        &self,
        _txn: TxnId,
        index: &aiondb_storage_api::IndexStorageDescriptor,
    ) -> DbResult<()> {
        self.created_index_ids.lock().unwrap().push(index.index_id);
        Ok(())
    }

    fn alter_table_storage(&self, _txn: TxnId, _table: &TableStorageDescriptor) -> DbResult<()> {
        Ok(())
    }

    fn drop_table_storage(&self, _txn: TxnId, table_id: RelationId) -> DbResult<()> {
        self.tables.lock().unwrap().remove(&table_id);
        self.table_descriptors.lock().unwrap().remove(&table_id);
        Ok(())
    }

    fn drop_index_storage(&self, _txn: TxnId, _index_id: IndexId) -> DbResult<()> {
        Ok(())
    }
}

impl StorageDML for MockStorage {
    fn cache_generation(&self) -> Option<u64> {
        *self.cache_generation.lock().unwrap()
    }

    fn graph_projection_cache_get(
        &self,
        namespace: &str,
        cache_key: &str,
        generation: u64,
    ) -> DbResult<Option<Vec<u8>>> {
        Ok(self
            .graph_projection_cache
            .lock()
            .unwrap()
            .get(&(namespace.to_owned(), cache_key.to_owned(), generation))
            .cloned())
    }

    fn graph_projection_cache_put(
        &self,
        namespace: &str,
        cache_key: &str,
        generation: u64,
        payload: &[u8],
    ) -> DbResult<()> {
        self.graph_projection_cache.lock().unwrap().insert(
            (namespace.to_owned(), cache_key.to_owned(), generation),
            payload.to_vec(),
        );
        Ok(())
    }

    fn scan_table(
        &self,
        _txn: TxnId,
        _snapshot: &Snapshot,
        table_id: RelationId,
        _projected_columns: Option<Vec<ColumnId>>,
    ) -> DbResult<Box<dyn TupleStream>> {
        *self
            .table_scan_counts
            .lock()
            .unwrap()
            .entry(table_id)
            .or_default() += 1;
        let tables = self.tables.lock().unwrap();
        let records: Vec<TupleRecord> = tables
            .get(&table_id)
            .unwrap_or(&Vec::new())
            .iter()
            .map(|(tid, row)| TupleRecord {
                tuple_id: *tid,
                heap_position: tid.get(),
                row: row.clone(),
            })
            .collect();
        Ok(Box::new(VecTupleStream::new(records)))
    }

    fn scan_index(
        &self,
        _txn: TxnId,
        _snapshot: &Snapshot,
        _index_id: IndexId,
        _key_range: KeyRange,
        _projected_columns: Option<Vec<ColumnId>>,
    ) -> DbResult<Box<dyn TupleStream>> {
        Ok(Box::new(VecTupleStream::new(Vec::new())))
    }

    fn fetch(
        &self,
        _txn: TxnId,
        _snapshot: &Snapshot,
        table_id: RelationId,
        tuple_id: TupleId,
        projected_columns: Option<Vec<ColumnId>>,
    ) -> DbResult<Option<Row>> {
        self.fetch_projection_widths
            .lock()
            .unwrap()
            .entry(table_id)
            .or_default()
            .push(projected_columns.as_ref().map_or(0, Vec::len));
        let tables = self.tables.lock().unwrap();
        let row = tables
            .get(&table_id)
            .and_then(|rows| rows.iter().find(|(tid, _)| *tid == tuple_id))
            .map(|(_, row)| row.clone());
        let Some(row) = row else {
            return Ok(None);
        };
        let Some(projected_columns) = projected_columns else {
            return Ok(Some(row));
        };
        let descriptors = self.table_descriptors.lock().unwrap();
        let Some(descriptor) = descriptors.get(&table_id) else {
            return Ok(Some(row));
        };
        let projected_values = projected_columns
            .into_iter()
            .filter_map(|column_id| {
                descriptor
                    .columns
                    .iter()
                    .position(|column| column.column_id == column_id)
                    .and_then(|ordinal| row.values.get(ordinal).cloned())
            })
            .collect();
        Ok(Some(Row::new(projected_values)))
    }

    fn insert(&self, _txn: TxnId, table_id: RelationId, row: Row) -> DbResult<TupleId> {
        let mut next_id = self.next_tuple_id.lock().unwrap();
        let tuple_id = TupleId::new(*next_id);
        *next_id += 1;
        self.tables
            .lock()
            .unwrap()
            .entry(table_id)
            .or_default()
            .push((tuple_id, row));
        Ok(tuple_id)
    }

    fn update(
        &self,
        _txn: TxnId,
        table_id: RelationId,
        tuple_id: TupleId,
        row: Row,
    ) -> DbResult<TupleId> {
        let mut tables = self.tables.lock().unwrap();
        if let Some(rows) = tables.get_mut(&table_id) {
            if let Some(entry) = rows.iter_mut().find(|(tid, _)| *tid == tuple_id) {
                entry.1 = row;
            }
        }
        Ok(tuple_id)
    }

    fn delete(&self, _txn: TxnId, table_id: RelationId, tuple_id: TupleId) -> DbResult<()> {
        let mut tables = self.tables.lock().unwrap();
        if let Some(rows) = tables.get_mut(&table_id) {
            rows.retain(|(tid, _)| *tid != tuple_id);
        }
        Ok(())
    }

    fn vacuum_table(&self, _table_id: RelationId) -> DbResult<u64> {
        Ok(0)
    }

    fn register_edge_table(
        &self,
        table_id: RelationId,
        source_col_idx: usize,
        target_col_idx: usize,
    ) {
        self.registered_edge_tables.lock().unwrap().push((
            table_id,
            source_col_idx,
            target_col_idx,
        ));
    }

    fn adjacency_edges(
        &self,
        _txn: TxnId,
        _snapshot: &Snapshot,
        edge_table_id: RelationId,
    ) -> DbResult<Vec<(TupleId, Value, Value)>> {
        *self
            .adjacency_edge_counts
            .lock()
            .unwrap()
            .entry(edge_table_id)
            .or_default() += 1;
        let registrations = self.registered_edge_tables.lock().unwrap();
        let Some((_, source_col_idx, target_col_idx)) = registrations
            .iter()
            .find(|(table_id, _, _)| *table_id == edge_table_id)
            .copied()
        else {
            return Err(DbError::feature_not_supported(
                "adjacency edge enumeration is not available for this mock edge table",
            ));
        };
        drop(registrations);

        let tables = self.tables.lock().unwrap();
        Ok(tables
            .get(&edge_table_id)
            .into_iter()
            .flat_map(|rows| rows.iter())
            .map(|(tuple_id, row)| {
                let source = row
                    .values
                    .get(source_col_idx)
                    .cloned()
                    .unwrap_or(Value::Null);
                let target = row
                    .values
                    .get(target_col_idx)
                    .cloned()
                    .unwrap_or(Value::Null);
                (*tuple_id, source, target)
            })
            .collect())
    }

    fn adjacency_edge_cursor(
        &self,
        _txn: TxnId,
        _snapshot: &Snapshot,
        edge_table_id: RelationId,
        node_id: &Value,
        outgoing: bool,
    ) -> DbResult<Box<dyn NeighborCursor<TupleId> + '_>> {
        *self
            .adjacency_edge_cursor_counts
            .lock()
            .unwrap()
            .entry(edge_table_id)
            .or_default() += 1;
        let registrations = self.registered_edge_tables.lock().unwrap();
        let Some((_, source_col_idx, target_col_idx)) = registrations
            .iter()
            .find(|(table_id, _, _)| *table_id == edge_table_id)
            .copied()
        else {
            return Err(DbError::feature_not_supported(
                "adjacency edge cursor is not available for this mock edge table",
            ));
        };
        drop(registrations);

        let tables = self.tables.lock().unwrap();
        let tuple_ids = tables
            .get(&edge_table_id)
            .into_iter()
            .flat_map(|rows| rows.iter())
            .filter_map(|(tuple_id, row)| {
                let source = row.values.get(source_col_idx)?;
                let target = row.values.get(target_col_idx)?;
                if outgoing {
                    (source == node_id).then_some(*tuple_id)
                } else {
                    (target == node_id).then_some(*tuple_id)
                }
            })
            .collect();
        Ok(Box::new(OwnedCursor::new(tuple_ids)))
    }

    fn adjacency_neighbor_cursor(
        &self,
        _txn: TxnId,
        _snapshot: &Snapshot,
        edge_table_id: RelationId,
        node_id: &Value,
        outgoing: bool,
    ) -> DbResult<Box<dyn NeighborCursor<Value> + '_>> {
        *self
            .adjacency_neighbor_cursor_counts
            .lock()
            .unwrap()
            .entry(edge_table_id)
            .or_default() += 1;
        let registrations = self.registered_edge_tables.lock().unwrap();
        let Some((_, source_col_idx, target_col_idx)) = registrations
            .iter()
            .find(|(table_id, _, _)| *table_id == edge_table_id)
            .copied()
        else {
            return Err(DbError::feature_not_supported(
                "adjacency neighbor cursor is not available for this mock edge table",
            ));
        };
        drop(registrations);

        let tables = self.tables.lock().unwrap();
        let neighbors = tables
            .get(&edge_table_id)
            .into_iter()
            .flat_map(|rows| rows.iter())
            .filter_map(|(_, row)| {
                let source = row.values.get(source_col_idx)?;
                let target = row.values.get(target_col_idx)?;
                if outgoing {
                    (source == node_id).then(|| target.clone())
                } else {
                    (target == node_id).then(|| source.clone())
                }
            })
            .collect();
        Ok(Box::new(OwnedCursor::new(neighbors)))
    }

    fn adjacency_weighted_edges(
        &self,
        _txn: TxnId,
        _snapshot: &Snapshot,
        edge_table_id: RelationId,
        weight_column: ColumnId,
    ) -> DbResult<Vec<(TupleId, Value, Value, Value)>> {
        *self
            .adjacency_weighted_edge_counts
            .lock()
            .unwrap()
            .entry(edge_table_id)
            .or_default() += 1;
        let registrations = self.registered_edge_tables.lock().unwrap();
        let Some((_, source_col_idx, target_col_idx)) = registrations
            .iter()
            .find(|(table_id, _, _)| *table_id == edge_table_id)
            .copied()
        else {
            return Err(DbError::feature_not_supported(
                "adjacency weighted edge enumeration is not available for this mock edge table",
            ));
        };
        drop(registrations);

        let descriptors = self.table_descriptors.lock().unwrap();
        let weight_col_idx = descriptors
            .get(&edge_table_id)
            .and_then(|descriptor| {
                descriptor
                    .columns
                    .iter()
                    .position(|column| column.column_id == weight_column)
            })
            .ok_or_else(|| DbError::internal("mock weighted edge column id out of bounds"))?;
        drop(descriptors);
        let tables = self.tables.lock().unwrap();
        Ok(tables
            .get(&edge_table_id)
            .into_iter()
            .flat_map(|rows| rows.iter())
            .map(|(tuple_id, row)| {
                let source = row
                    .values
                    .get(source_col_idx)
                    .cloned()
                    .unwrap_or(Value::Null);
                let target = row
                    .values
                    .get(target_col_idx)
                    .cloned()
                    .unwrap_or(Value::Null);
                let weight = row
                    .values
                    .get(weight_col_idx)
                    .cloned()
                    .unwrap_or(Value::Null);
                (*tuple_id, source, target, weight)
            })
            .collect())
    }

    fn adjacency_edge_endpoints(
        &self,
        _txn: TxnId,
        _snapshot: &Snapshot,
        edge_table_id: RelationId,
        edge_tuple_id: TupleId,
    ) -> DbResult<Option<(Value, Value)>> {
        let registrations = self.registered_edge_tables.lock().unwrap();
        let Some((_, source_col_idx, target_col_idx)) = registrations
            .iter()
            .find(|(table_id, _, _)| *table_id == edge_table_id)
            .copied()
        else {
            return Err(DbError::feature_not_supported(
                "adjacency edge endpoint lookup is not available for this mock edge table",
            ));
        };
        drop(registrations);
        let tables = self.tables.lock().unwrap();
        Ok(tables
            .get(&edge_table_id)
            .and_then(|rows| rows.iter().find(|(tid, _)| *tid == edge_tuple_id))
            .map(|(_, row)| {
                (
                    row.values
                        .get(source_col_idx)
                        .cloned()
                        .unwrap_or(Value::Null),
                    row.values
                        .get(target_col_idx)
                        .cloned()
                        .unwrap_or(Value::Null),
                )
            }))
    }

    fn adjacency_index_available(&self, _txn: TxnId, edge_table_id: RelationId) -> bool {
        self.registered_edge_tables
            .lock()
            .unwrap()
            .iter()
            .any(|(table_id, _, _)| *table_id == edge_table_id)
    }
}

impl StorageTxnParticipant for MockStorage {
    fn begin_txn(&self, _txn: TxnId, _isolation: IsolationLevel) -> DbResult<()> {
        Ok(())
    }

    fn commit_txn(&self, _txn: TxnId, _commit_ts: u64) -> DbResult<()> {
        Ok(())
    }

    fn rollback_txn(&self, _txn: TxnId) -> DbResult<()> {
        Ok(())
    }

    fn checkpoint(&self) -> DbResult<CheckpointInfo> {
        Ok(CheckpointInfo {
            checkpoint_lsn: 0,
            dirty_pages_flushed: 0,
        })
    }

    fn create_savepoint(&self, _txn: TxnId) -> DbResult<u64> {
        Ok(1)
    }

    fn rollback_to_savepoint(&self, _txn: TxnId, _savepoint_id: u64) -> DbResult<()> {
        if let Some(error) = self.rollback_savepoint_error.lock().unwrap().take() {
            Err(error)
        } else {
            Ok(())
        }
    }

    fn release_savepoint(&self, _txn: TxnId, _savepoint_id: u64) -> DbResult<()> {
        if let Some(error) = self.release_savepoint_error.lock().unwrap().take() {
            Err(error)
        } else {
            Ok(())
        }
    }
}

// =========================================================================
// Helper functions
// =========================================================================

fn default_context() -> ExecutionContext {
    ExecutionContext::default()
}

fn make_executor() -> (Executor, Arc<MockCatalog>, Arc<MockStorage>) {
    let catalog = Arc::new(MockCatalog::new());
    let storage = Arc::new(MockStorage::new());
    let compiler: Arc<super::LogicalPlanCompiler> =
        Arc::new(|_plan, _ctx| Err(aiondb_core::DbError::internal("no compiler in test")));
    let executor = Executor::new(
        catalog.clone(),
        catalog.clone(),
        catalog.clone(),
        catalog.clone(),
        storage.clone(),
        storage.clone(),
        storage.clone(),
        compiler,
    );
    (executor, catalog, storage)
}

fn make_projection_expr(
    name: &str,
    dt: DataType,
    nullable: bool,
    expr: TypedExpr,
) -> ProjectionExpr {
    ProjectionExpr {
        field: ResultField {
            name: name.to_string(),
            data_type: dt,
            text_type_modifier: None,
            nullable,
        },
        expr,
    }
}

/// Execute a `CreateTable` plan and return the `table_id` assigned by the catalog.
fn create_test_table(
    executor: &Executor,
    catalog: &MockCatalog,
    name: &str,
    columns: Vec<ColumnPlan>,
) -> RelationId {
    let ctx = default_context();
    let plan = PhysicalPlan::CreateTable {
        relation_name: name.to_string(),
        columns,
        defaults: Vec::new(),
        identities: Vec::new(),
        typed_table_of: None,
        primary_key_columns: Vec::new(),
        unique_constraints: Vec::new(),
        foreign_keys: Vec::new(),
        check_constraints: Vec::new(),
        shard_key_columns: Vec::new(),
        shard_count: None,
    };
    executor.execute(&plan, &ctx).expect("create table");

    // Find the table in the catalog
    let tables = catalog.tables.lock().unwrap();
    tables
        .iter()
        .find(|t| t.name.object_name() == name)
        .expect("table must exist after creation")
        .table_id
}

#[test]
fn rewrite_add_column_surfaces_storage_rollback_savepoint_cleanup_failure() {
    let (executor, catalog, storage) = make_executor();
    let table_id = create_test_table(
        &executor,
        &catalog,
        "t_rewrite_add_col_storage_cleanup",
        vec![ColumnPlan {
            name: "id".to_string(),
            data_type: DataType::Int,
            raw_type_name: None,
            text_type_modifier: None,
            nullable: false,
            has_default: false,
        }],
    );
    storage.fail_next_rollback_savepoint(DbError::internal("injected storage rollback failure"));

    let past = Instant::now()
        .checked_sub(Duration::from_millis(1))
        .expect("subtract 1ms");
    let ctx = ExecutionContext {
        txn_id: TxnId::new(42),
        statement_deadline: Some(past),
        ..default_context()
    };
    let plan = PhysicalPlan::AlterTableAddColumn {
        table_id,
        column: ColumnPlan {
            name: "new_col".to_string(),
            data_type: DataType::Int,
            raw_type_name: None,
            text_type_modifier: None,
            nullable: true,
            has_default: false,
        },
        default: None,
    };

    let error = executor
        .execute(&plan, &ctx)
        .expect_err("expired deadline should fail rewrite");
    assert_eq!(error.sqlstate(), aiondb_core::SqlState::QueryCanceled);
    let detail = error
        .report()
        .internal_detail
        .as_deref()
        .expect("cleanup failure detail should be attached");
    assert!(detail.contains("ALTER TABLE ADD COLUMN rewrite: internal savepoint cleanup failed"));
    assert!(detail.contains("rollback storage savepoint: injected storage rollback failure"));
}

#[test]
fn rewrite_add_column_surfaces_catalog_release_savepoint_cleanup_failure() {
    let (executor, catalog, _storage) = make_executor();
    let table_id = create_test_table(
        &executor,
        &catalog,
        "t_rewrite_add_col_catalog_cleanup",
        vec![ColumnPlan {
            name: "id".to_string(),
            data_type: DataType::Int,
            raw_type_name: None,
            text_type_modifier: None,
            nullable: false,
            has_default: false,
        }],
    );
    catalog.fail_next_release_savepoint(DbError::internal("injected catalog release failure"));

    let ctx = ExecutionContext {
        txn_id: TxnId::new(43),
        ..default_context()
    };
    let plan = PhysicalPlan::AlterTableAddColumn {
        table_id,
        column: ColumnPlan {
            name: "new_col".to_string(),
            data_type: DataType::Int,
            raw_type_name: None,
            text_type_modifier: None,
            nullable: true,
            has_default: false,
        },
        default: None,
    };

    let error = executor
        .execute(&plan, &ctx)
        .expect_err("savepoint release cleanup failure should be surfaced");
    assert_eq!(error.sqlstate(), aiondb_core::SqlState::InternalError);
    assert_eq!(
        error.report().message,
        "ALTER TABLE ADD COLUMN rewrite: failed to release internal rewrite savepoint"
    );
    let detail = error
        .report()
        .internal_detail
        .as_deref()
        .expect("cleanup failure detail should be attached");
    assert!(detail.contains("release catalog savepoint: injected catalog release failure"));
}

#[test]
fn pg_ls_logdir_uses_execution_context_server_data_dir() {
    let (executor, _, _) = make_executor();
    let data_dir = unique_test_dir("pg-ls-logdir");
    fs::create_dir_all(data_dir.join("log")).expect("create log dir");
    fs::write(data_dir.join("log").join("server.log"), b"log").expect("write log file");

    let ctx = ExecutionContext {
        server_data_dir: Some(data_dir.clone()),
        ..ExecutionContext::default()
    };
    let plan = PhysicalPlan::ProjectOnce {
        outputs: vec![make_projection_expr(
            "entry",
            DataType::Text,
            true,
            TypedExpr::scalar_function(
                aiondb_plan::ScalarFunction::Generic("pg_ls_logdir".to_owned()),
                vec![],
                DataType::Text,
                true,
            ),
        )],
        filter: None,
        order_by: Vec::new(),
        limit: None,
        offset: None,
        distinct: false,
        distinct_on: Vec::new(),
    };

    let result = executor.execute(&plan, &ctx).expect("pg_ls_logdir");
    match result {
        ExecutionResult::Query { rows, .. } => {
            assert_eq!(rows.len(), 1);
            assert_eq!(rows[0].values, vec![Value::Text("server.log".to_owned())]);
        }
        other => panic!("expected Query result, got {other:?}"),
    }

    let _ = fs::remove_dir_all(&data_dir);
}

#[test]
fn format_index_definition_quotes_identifiers() {
    let table = TableDescriptor {
        table_id: RelationId::new(1),
        schema_id: SchemaId::new(7),
        name: QualifiedName::qualified("Mixed Schema", "Order Items"),
        columns: vec![
            ColumnDescriptor {
                column_id: ColumnId::new(1),
                name: "Select".to_owned(),
                data_type: DataType::Int,
                raw_type_name: None,
                text_type_modifier: None,
                nullable: false,
                ordinal_position: 1,
                default_value: None,
            },
            ColumnDescriptor {
                column_id: ColumnId::new(2),
                name: "CamelCase".to_owned(),
                data_type: DataType::Text,
                raw_type_name: None,
                text_type_modifier: None,
                nullable: true,
                ordinal_position: 2,
                default_value: None,
            },
        ],
        primary_key: None,
        foreign_keys: Vec::new(),
        check_constraints: Vec::new(),
        shard_config: None,
        identity_columns: Vec::new(),
        owner: None,
    };
    let index = IndexDescriptor {
        index_id: IndexId::new(3),
        schema_id: SchemaId::new(7),
        table_id: table.table_id,
        name: QualifiedName::qualified("Mixed Schema", "Idx Order Items"),
        unique: true,
        nulls_not_distinct: false,
        kind: IndexKind::BTree,
        key_columns: vec![IndexKeyColumn {
            column_id: ColumnId::new(1),
            sort_order: SortOrder::Descending,
            nulls_first: false,
        }],
        include_columns: vec![ColumnId::new(2)],
        constraint_name: None,
        hnsw_params: None,
    };

    let ddl = format_index_definition(&index, &table);
    assert_eq!(
        ddl,
        "CREATE UNIQUE INDEX \"Idx Order Items\" ON \"Mixed Schema\".\"Order Items\" USING btree (\"Select\" DESC NULLS LAST) INCLUDE (\"CamelCase\")"
    );
}

fn unique_test_dir(name: &str) -> std::path::PathBuf {
    static TEST_DIR_SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let seq = TEST_DIR_SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or(0);
    std::env::temp_dir().join(format!(
        "aiondb-executor-{name}-{}-{:?}-{seq}-{nanos}",
        std::process::id(),
        std::thread::current().id()
    ))
}

mod context_and_limits;
mod cypher_graph;
mod ddl;
mod deadline_helpers;
mod delete_update;
mod distributed_exec;
mod index_scans;
mod insert_and_project;
mod joins_and_aggregates;
mod merge;
mod project_once;
mod returning;
mod sequences_deadlines_edge;
mod window_eval;
