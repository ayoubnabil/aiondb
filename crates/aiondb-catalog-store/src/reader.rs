use aiondb_catalog::{
    AccessPathMetadata, CastDescriptor, CatalogReader, CommentDescriptor, DomainDescriptor,
    EdgeLabelDescriptor, FunctionDescriptor, IndexDescriptor, NodeLabelDescriptor,
    PolicyDescriptor, PrivilegeDescriptor, QualifiedName, RoleDescriptor, RuleDescriptor,
    SchemaDescriptor, SequenceDescriptor, TableDescriptor, TableStatistics, TenantDescriptor,
    TriggerDescriptor, UserTypeDescriptor, ViewDescriptor,
};
use aiondb_core::{DbResult, IndexId, RelationId, SchemaId, TxnId};

use crate::CatalogStore;

impl CatalogReader for CatalogStore {
    fn get_schema(&self, txn: TxnId, name: &QualifiedName) -> DbResult<Option<SchemaDescriptor>> {
        self.read_catalog_state(txn, |state| {
            Ok(CatalogStore::find_schema_id(state, name)
                .and_then(|schema_id| state.schemas_by_id.get(&schema_id).cloned()))
        })
    }

    fn list_schemas(&self, txn: TxnId) -> DbResult<Vec<SchemaDescriptor>> {
        self.read_catalog_state(txn, |state| {
            Ok(state.schemas_by_id.values().cloned().collect())
        })
    }

    fn get_table(&self, txn: TxnId, name: &QualifiedName) -> DbResult<Option<TableDescriptor>> {
        self.read_catalog_state(txn, |state| {
            CatalogStore::lookup_named_object(state, name, &state.table_names, &state.tables_by_id)
        })
    }

    fn get_table_by_id(
        &self,
        txn: TxnId,
        table_id: RelationId,
    ) -> DbResult<Option<TableDescriptor>> {
        self.read_catalog_state(txn, |state| Ok(state.tables_by_id.get(&table_id).cloned()))
    }

    fn get_table_type_name(&self, txn: TxnId, table_id: RelationId) -> DbResult<Option<String>> {
        self.read_catalog_state(txn, |state| {
            Ok(state.typed_table_types_by_id.get(&table_id).cloned())
        })
    }

    fn list_tables(&self, txn: TxnId, schema_id: SchemaId) -> DbResult<Vec<TableDescriptor>> {
        self.read_catalog_state(txn, |state| {
            Ok(state
                .tables_by_id
                .values()
                .filter(|table| table.schema_id == schema_id)
                .cloned()
                .collect())
        })
    }

    fn list_indexes(&self, txn: TxnId, table_id: RelationId) -> DbResult<Vec<IndexDescriptor>> {
        self.read_catalog_state(txn, |state| {
            Ok(state
                .indexes_by_table
                .get(&table_id)
                .into_iter()
                .flat_map(|index_ids| index_ids.iter())
                .filter_map(|index_id| state.indexes_by_id.get(index_id).cloned())
                .collect())
        })
    }

    fn get_index(&self, txn: TxnId, index_id: IndexId) -> DbResult<Option<IndexDescriptor>> {
        self.read_catalog_state(txn, |state| Ok(state.indexes_by_id.get(&index_id).cloned()))
    }

    fn get_sequence(
        &self,
        txn: TxnId,
        name: &QualifiedName,
    ) -> DbResult<Option<SequenceDescriptor>> {
        self.read_catalog_state(txn, |state| {
            CatalogStore::lookup_named_object(
                state,
                name,
                &state.sequence_names,
                &state.sequences_by_id,
            )
        })
    }

    fn list_sequences(&self, txn: TxnId, schema_id: SchemaId) -> DbResult<Vec<SequenceDescriptor>> {
        self.read_catalog_state(txn, |state| {
            Ok(state
                .sequences_by_id
                .values()
                .filter(|sequence| sequence.schema_id == schema_id)
                .cloned()
                .collect())
        })
    }

    fn get_statistics(
        &self,
        txn: TxnId,
        table_id: RelationId,
    ) -> DbResult<Option<TableStatistics>> {
        self.read_catalog_state(txn, |state| Ok(state.statistics.get(&table_id).cloned()))
    }

    fn get_access_path_metadata(
        &self,
        txn: TxnId,
        table_id: RelationId,
    ) -> DbResult<Option<AccessPathMetadata>> {
        // Bundle the three reads (table, indexes, statistics) under one
        // catalog read-lock acquisition. Each individual reader call walks
        // through `read_catalog_state` which acquires the catalog state
        // RwLock; on the OLTP hot path the optimizer pays this lock three
        // times per query, once for each piece of access-path metadata.
        // Folding the reads into one closure cuts that to a single
        // acquisition without changing semantics.
        self.read_catalog_state(txn, |state| {
            let Some(table) = state.tables_by_id.get(&table_id).cloned() else {
                return Ok(None);
            };
            let indexes = state
                .indexes_by_table
                .get(&table_id)
                .into_iter()
                .flat_map(|index_ids| index_ids.iter())
                .filter_map(|index_id| state.indexes_by_id.get(index_id).cloned())
                .collect();
            let stats = state.statistics.get(&table_id).cloned();
            Ok(Some(AccessPathMetadata {
                table,
                indexes,
                stats,
            }))
        })
    }

    fn get_view(&self, txn: TxnId, name: &QualifiedName) -> DbResult<Option<ViewDescriptor>> {
        self.read_catalog_state(txn, |state| {
            CatalogStore::lookup_named_object(state, name, &state.view_names, &state.views_by_id)
        })
    }

    fn list_views(&self, txn: TxnId, schema_id: SchemaId) -> DbResult<Vec<ViewDescriptor>> {
        self.read_catalog_state(txn, |state| {
            Ok(state
                .views_by_id
                .values()
                .filter(|view| view.schema_id == schema_id)
                .cloned()
                .collect())
        })
    }

    fn get_node_label(&self, txn: TxnId, label: &str) -> DbResult<Option<NodeLabelDescriptor>> {
        let lookup = CatalogStore::normalize_identifier(label);
        self.read_catalog_state(txn, |state| Ok(state.node_labels.get(&lookup).cloned()))
    }

    fn get_edge_label(&self, txn: TxnId, label: &str) -> DbResult<Option<EdgeLabelDescriptor>> {
        let lookup = CatalogStore::normalize_identifier(label);
        self.read_catalog_state(txn, |state| Ok(state.edge_labels.get(&lookup).cloned()))
    }

    fn list_node_labels(&self, txn: TxnId) -> DbResult<Vec<NodeLabelDescriptor>> {
        self.read_catalog_state(txn, |state| {
            Ok(state.node_labels.values().cloned().collect())
        })
    }

    fn list_edge_labels(&self, txn: TxnId) -> DbResult<Vec<EdgeLabelDescriptor>> {
        self.read_catalog_state(txn, |state| {
            Ok(state.edge_labels.values().cloned().collect())
        })
    }

    fn get_role(&self, txn: TxnId, name: &str) -> DbResult<Option<RoleDescriptor>> {
        let lookup = CatalogStore::normalize_identifier(name);
        self.read_catalog_state(txn, |state| Ok(state.roles.get(&lookup).cloned()))
    }

    fn list_roles(&self, txn: TxnId) -> DbResult<Vec<RoleDescriptor>> {
        self.read_catalog_state(txn, |state| Ok(state.roles.values().cloned().collect()))
    }

    fn get_privileges(&self, txn: TxnId, role_name: &str) -> DbResult<Vec<PrivilegeDescriptor>> {
        let lookup = CatalogStore::normalize_identifier(role_name);
        self.read_catalog_state(txn, |state| {
            Ok(state
                .privileges
                .iter()
                .filter(|p| CatalogStore::normalize_identifier(&p.role_name) == lookup)
                .cloned()
                .collect())
        })
    }

    fn get_tenant(&self, txn: TxnId, name: &str) -> DbResult<Option<TenantDescriptor>> {
        let lookup = CatalogStore::normalize_identifier(name);
        self.read_catalog_state(txn, |state| Ok(state.tenants_by_name.get(&lookup).cloned()))
    }

    fn list_tenants(&self, txn: TxnId) -> DbResult<Vec<TenantDescriptor>> {
        self.read_catalog_state(txn, |state| {
            Ok(state.tenants_by_name.values().cloned().collect())
        })
    }

    fn get_function(&self, txn: TxnId, name: &str) -> DbResult<Option<FunctionDescriptor>> {
        let lookup = CatalogStore::normalize_identifier(name);
        self.read_catalog_state(txn, |state| {
            Ok(state
                .functions
                .get(&lookup)
                .and_then(|overloads| overloads.first().cloned()))
        })
    }

    fn list_functions(&self, txn: TxnId) -> DbResult<Vec<FunctionDescriptor>> {
        self.read_catalog_state(txn, |state| {
            Ok(state
                .functions
                .values()
                .flat_map(|overloads| overloads.iter().cloned())
                .collect())
        })
    }

    fn get_domain(&self, txn: TxnId, name: &str) -> DbResult<Option<DomainDescriptor>> {
        let lookup = CatalogStore::normalize_identifier(name);
        self.read_catalog_state(txn, |state| Ok(state.domains.get(&lookup).cloned()))
    }

    fn list_domains(&self, txn: TxnId) -> DbResult<Vec<DomainDescriptor>> {
        self.read_catalog_state(txn, |state| Ok(state.domains.values().cloned().collect()))
    }

    fn get_user_type(&self, txn: TxnId, name: &str) -> DbResult<Option<UserTypeDescriptor>> {
        let lookup = CatalogStore::normalize_identifier(name);
        self.read_catalog_state(txn, |state| Ok(state.user_types.get(&lookup).cloned()))
    }

    fn list_user_types(&self, txn: TxnId) -> DbResult<Vec<UserTypeDescriptor>> {
        self.read_catalog_state(txn, |state| {
            Ok(state.user_types.values().cloned().collect())
        })
    }

    fn list_casts(&self, txn: TxnId) -> DbResult<Vec<CastDescriptor>> {
        self.read_catalog_state(txn, |state| Ok(state.casts.values().cloned().collect()))
    }

    fn list_policies(&self, txn: TxnId) -> DbResult<Vec<PolicyDescriptor>> {
        self.read_catalog_state(txn, |state| Ok(state.policies.values().cloned().collect()))
    }

    fn list_rules(&self, txn: TxnId) -> DbResult<Vec<RuleDescriptor>> {
        self.read_catalog_state(txn, |state| Ok(state.rules.values().cloned().collect()))
    }

    fn list_comments(&self, txn: TxnId) -> DbResult<Vec<CommentDescriptor>> {
        self.read_catalog_state(txn, |state| {
            Ok(state
                .comments
                .iter()
                .map(
                    |((object_type, object_identity), comment)| CommentDescriptor {
                        object_type: object_type.clone(),
                        object_identity: object_identity.clone(),
                        comment: comment.clone(),
                    },
                )
                .collect())
        })
    }

    fn list_triggers(&self, txn: TxnId, table_name: &str) -> DbResult<Vec<TriggerDescriptor>> {
        self.read_catalog_state(txn, |state| {
            Ok(state
                .triggers
                .iter()
                .filter(|t| CatalogStore::trigger_table_matches(&t.table_name, table_name))
                .cloned()
                .collect())
        })
    }

    fn catalog_revision(&self, txn: TxnId) -> DbResult<u64> {
        // Autocommit / read-only path: read the lock-free atomic
        // mirror of `state.revision`. This is the OLTP hot path -
        // the plan-cache + search-path machinery hits it once per
        // Execute, and going through the state RwLock for what is
        // effectively a `u64` load was wasted.
        if CatalogStore::is_autocommit_txn(txn) {
            return Ok(self
                .cached_revision
                .load(std::sync::atomic::Ordering::Acquire));
        }
        self.read_catalog_state(txn, |state| Ok(state.revision))
    }
}

#[cfg(test)]
mod tests {
    use aiondb_catalog::{
        CatalogReader, CatalogTxnParticipant, CatalogWriter, ColumnDescriptor, IndexDescriptor,
        IndexKeyColumn, IndexKind, QualifiedName, SchemaDescriptor, SequenceDescriptor, SortOrder,
        TableDescriptor, TableStatistics,
    };
    use aiondb_core::{ColumnId, DataType, IndexId, RelationId, SchemaId, TxnId};

    use crate::CatalogStore;

    /// Autocommit txn id used for operations that take immediate effect.
    fn auto() -> TxnId {
        TxnId::default()
    }

    fn make_table(name: &str) -> TableDescriptor {
        TableDescriptor {
            table_id: Default::default(),
            schema_id: Default::default(),
            name: QualifiedName::qualified("public", name),
            columns: vec![ColumnDescriptor {
                column_id: Default::default(),
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
            foreign_keys: Vec::new(),
            check_constraints: Vec::new(),
            shard_config: None,
            owner: None,
        }
    }

    fn make_index(table_id: RelationId, name: &str) -> IndexDescriptor {
        IndexDescriptor {
            index_id: IndexId::default(),
            schema_id: Default::default(),
            table_id,
            name: QualifiedName::qualified("public", name),
            unique: false,
            kind: IndexKind::BTree,
            key_columns: vec![IndexKeyColumn {
                column_id: ColumnId::new(1),
                sort_order: SortOrder::Ascending,
                nulls_first: false,
            }],
            include_columns: Vec::new(),
            constraint_name: None,
            hnsw_params: None,
            nulls_not_distinct: false,
        }
    }

    fn make_sequence(name: &str) -> SequenceDescriptor {
        SequenceDescriptor {
            sequence_id: Default::default(),
            schema_id: Default::default(),
            name: QualifiedName::qualified("public", name),
            data_type: DataType::BigInt,
            start_value: 1,
            increment_by: 1,
            min_value: 1,
            max_value: i64::MAX,
            cache_size: 1,
            cycle: false,
            owned_by: None,
            owner: None,
        }
    }

    // ===================================================================
    // get_schema
    // ===================================================================

    #[test]
    fn get_schema_public_exists() {
        let store = CatalogStore::new();
        let result = store
            .get_schema(auto(), &QualifiedName::unqualified("public"))
            .unwrap();
        assert!(result.is_some());
        assert_eq!(result.unwrap().name, "public");
    }

    #[test]
    fn get_schema_nonexistent_returns_none() {
        let store = CatalogStore::new();
        let result = store
            .get_schema(auto(), &QualifiedName::unqualified("nonexistent"))
            .unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn get_schema_case_insensitive() {
        let store = CatalogStore::new();
        let result = store
            .get_schema(auto(), &QualifiedName::unqualified("PUBLIC"))
            .unwrap();
        assert!(result.is_some());
    }

    #[test]
    fn get_schema_custom_schema() {
        let store = CatalogStore::new();
        let schema_id = store
            .create_schema(
                auto(),
                SchemaDescriptor {
                    schema_id: Default::default(),
                    name: "analytics".to_owned(),
                },
            )
            .unwrap();
        let result = store
            .get_schema(auto(), &QualifiedName::unqualified("analytics"))
            .unwrap()
            .unwrap();
        assert_eq!(result.schema_id, schema_id);
        assert_eq!(result.name, "analytics");
    }

    // ===================================================================
    // get_table
    // ===================================================================

    #[test]
    fn get_table_returns_none_when_empty() {
        let store = CatalogStore::new();
        let result = store
            .get_table(auto(), &QualifiedName::qualified("public", "users"))
            .unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn get_table_returns_created_table() {
        let store = CatalogStore::new();
        let table_id = store.create_table(auto(), make_table("users")).unwrap();
        let result = store
            .get_table(auto(), &QualifiedName::qualified("public", "users"))
            .unwrap()
            .unwrap();
        assert_eq!(result.table_id, table_id);
        assert_eq!(result.name, QualifiedName::qualified("public", "users"));
    }

    #[test]
    fn get_table_case_insensitive_name() {
        let store = CatalogStore::new();
        store.create_table(auto(), make_table("Users")).unwrap();
        let result = store
            .get_table(auto(), &QualifiedName::qualified("public", "users"))
            .unwrap();
        assert!(result.is_some());
    }

    #[test]
    fn get_table_wrong_schema_returns_none() {
        let store = CatalogStore::new();
        store.create_table(auto(), make_table("users")).unwrap();
        let result = store
            .get_table(
                auto(),
                &QualifiedName::qualified("nonexistent_schema", "users"),
            )
            .unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn get_table_unqualified_resolves_to_public() {
        let store = CatalogStore::new();
        store.create_table(auto(), make_table("orders")).unwrap();
        let result = store
            .get_table(auto(), &QualifiedName::unqualified("orders"))
            .unwrap();
        assert!(result.is_some());
    }

    // ===================================================================
    // get_table_by_id
    // ===================================================================

    #[test]
    fn get_table_by_id_returns_correct_table() {
        let store = CatalogStore::new();
        let table_id = store.create_table(auto(), make_table("users")).unwrap();
        let result = store.get_table_by_id(auto(), table_id).unwrap().unwrap();
        assert_eq!(result.table_id, table_id);
    }

    #[test]
    fn get_table_by_id_nonexistent_returns_none() {
        let store = CatalogStore::new();
        let result = store
            .get_table_by_id(auto(), RelationId::new(9999))
            .unwrap();
        assert!(result.is_none());
    }

    // ===================================================================
    // list_tables
    // ===================================================================

    #[test]
    fn list_tables_empty_schema() {
        let store = CatalogStore::new();
        let tables = store.list_tables(auto(), SchemaId::new(1)).unwrap();
        assert!(tables.is_empty());
    }

    #[test]
    fn list_tables_returns_all_tables_in_schema() {
        let store = CatalogStore::new();
        store.create_table(auto(), make_table("t1")).unwrap();
        store.create_table(auto(), make_table("t2")).unwrap();
        store.create_table(auto(), make_table("t3")).unwrap();
        let tables = store.list_tables(auto(), SchemaId::new(1)).unwrap();
        assert_eq!(tables.len(), 3);
    }

    #[test]
    fn list_tables_does_not_return_tables_from_other_schema() {
        let store = CatalogStore::new();
        store.create_table(auto(), make_table("t1")).unwrap();
        // Schema 999 does not exist but list_tables just filters, returns empty.
        let tables = store.list_tables(auto(), SchemaId::new(999)).unwrap();
        assert!(tables.is_empty());
    }

    // ===================================================================
    // list_indexes
    // ===================================================================

    #[test]
    fn list_indexes_no_indexes() {
        let store = CatalogStore::new();
        let table_id = store.create_table(auto(), make_table("users")).unwrap();
        let indexes = store.list_indexes(auto(), table_id).unwrap();
        assert!(indexes.is_empty());
    }

    #[test]
    fn list_indexes_returns_all_for_table() {
        let store = CatalogStore::new();
        let table_id = store.create_table(auto(), make_table("users")).unwrap();
        store
            .create_index(auto(), make_index(table_id, "idx1"))
            .unwrap();
        store
            .create_index(auto(), make_index(table_id, "idx2"))
            .unwrap();
        let indexes = store.list_indexes(auto(), table_id).unwrap();
        assert_eq!(indexes.len(), 2);
    }

    #[test]
    fn list_indexes_nonexistent_table_returns_empty() {
        let store = CatalogStore::new();
        let indexes = store.list_indexes(auto(), RelationId::new(9999)).unwrap();
        assert!(indexes.is_empty());
    }

    #[test]
    fn list_indexes_does_not_mix_tables() {
        let store = CatalogStore::new();
        let t1 = store.create_table(auto(), make_table("t1")).unwrap();
        let t2 = store.create_table(auto(), make_table("t2")).unwrap();
        store
            .create_index(auto(), make_index(t1, "idx_t1"))
            .unwrap();
        store
            .create_index(auto(), make_index(t2, "idx_t2"))
            .unwrap();
        assert_eq!(store.list_indexes(auto(), t1).unwrap().len(), 1);
        assert_eq!(store.list_indexes(auto(), t2).unwrap().len(), 1);
    }

    // ===================================================================
    // get_index
    // ===================================================================

    #[test]
    fn get_index_returns_created_index() {
        let store = CatalogStore::new();
        let table_id = store.create_table(auto(), make_table("users")).unwrap();
        let index_id = store
            .create_index(auto(), make_index(table_id, "idx1"))
            .unwrap();
        let index = store.get_index(auto(), index_id).unwrap().unwrap();
        assert_eq!(index.index_id, index_id);
        assert_eq!(index.table_id, table_id);
    }

    #[test]
    fn get_index_nonexistent_returns_none() {
        let store = CatalogStore::new();
        let result = store.get_index(auto(), IndexId::new(9999)).unwrap();
        assert!(result.is_none());
    }

    // ===================================================================
    // get_sequence
    // ===================================================================

    #[test]
    fn get_sequence_returns_created_sequence() {
        let store = CatalogStore::new();
        let seq_id = store
            .create_sequence(auto(), make_sequence("my_seq"))
            .unwrap();
        let seq = store
            .get_sequence(auto(), &QualifiedName::qualified("public", "my_seq"))
            .unwrap()
            .unwrap();
        assert_eq!(seq.sequence_id, seq_id);
    }

    #[test]
    fn get_sequence_nonexistent_returns_none() {
        let store = CatalogStore::new();
        let result = store
            .get_sequence(auto(), &QualifiedName::qualified("public", "no_seq"))
            .unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn get_sequence_wrong_schema_returns_none() {
        let store = CatalogStore::new();
        store
            .create_sequence(auto(), make_sequence("my_seq"))
            .unwrap();
        let result = store
            .get_sequence(auto(), &QualifiedName::qualified("nonexistent", "my_seq"))
            .unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn get_sequence_unqualified_resolves_to_public() {
        let store = CatalogStore::new();
        store
            .create_sequence(auto(), make_sequence("my_seq"))
            .unwrap();
        let result = store
            .get_sequence(auto(), &QualifiedName::unqualified("my_seq"))
            .unwrap();
        assert!(result.is_some());
    }

    #[test]
    fn get_sequence_case_insensitive() {
        let store = CatalogStore::new();
        store
            .create_sequence(auto(), make_sequence("MySeq"))
            .unwrap();
        let result = store
            .get_sequence(auto(), &QualifiedName::qualified("public", "myseq"))
            .unwrap();
        assert!(result.is_some());
    }

    // ===================================================================
    // get_statistics
    // ===================================================================

    #[test]
    fn get_statistics_none_when_no_stats() {
        let store = CatalogStore::new();
        let table_id = store.create_table(auto(), make_table("users")).unwrap();
        let result = store.get_statistics(auto(), table_id).unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn get_statistics_returns_updated_stats() {
        let store = CatalogStore::new();
        let table_id = store.create_table(auto(), make_table("users")).unwrap();
        store
            .update_statistics(
                auto(),
                TableStatistics {
                    table_id,
                    row_count: 100,
                    total_bytes: 8192,
                    dead_row_count: 5,
                    last_updated_by: Some(auto()),
                    column_stats: Vec::new(),
                },
            )
            .unwrap();
        let stats = store.get_statistics(auto(), table_id).unwrap().unwrap();
        assert_eq!(stats.row_count, 100);
        assert_eq!(stats.total_bytes, 8192);
        assert_eq!(stats.dead_row_count, 5);
    }

    #[test]
    fn get_statistics_nonexistent_table_returns_none() {
        let store = CatalogStore::new();
        let result = store.get_statistics(auto(), RelationId::new(9999)).unwrap();
        assert!(result.is_none());
    }

    // ===================================================================
    // Transactional reads: reader sees txn-local state
    // ===================================================================

    #[test]
    fn reader_sees_uncommitted_table_in_txn() {
        let store = CatalogStore::new();
        let txn = TxnId::new(100);
        store.begin_txn(txn).unwrap();
        store.create_table(txn, make_table("txn_table")).unwrap();

        // Within txn, table is visible.
        assert!(store
            .get_table(txn, &QualifiedName::qualified("public", "txn_table"))
            .unwrap()
            .is_some());
        // Outside txn, table is not visible.
        assert!(store
            .get_table(auto(), &QualifiedName::qualified("public", "txn_table"))
            .unwrap()
            .is_none());

        store.rollback_txn(txn).unwrap();
    }

    #[test]
    fn reader_sees_uncommitted_index_in_txn() {
        let store = CatalogStore::new();
        let table_id = store.create_table(auto(), make_table("users")).unwrap();
        let txn = TxnId::new(101);
        store.begin_txn(txn).unwrap();
        let index_id = store
            .create_index(txn, make_index(table_id, "txn_idx"))
            .unwrap();

        assert!(store.get_index(txn, index_id).unwrap().is_some());
        assert!(store.get_index(auto(), index_id).unwrap().is_none());
        store.rollback_txn(txn).unwrap();
    }

    #[test]
    fn reader_sees_uncommitted_sequence_in_txn() {
        let store = CatalogStore::new();
        let txn = TxnId::new(102);
        store.begin_txn(txn).unwrap();
        store
            .create_sequence(txn, make_sequence("txn_seq"))
            .unwrap();

        assert!(store
            .get_sequence(txn, &QualifiedName::qualified("public", "txn_seq"))
            .unwrap()
            .is_some());
        assert!(store
            .get_sequence(auto(), &QualifiedName::qualified("public", "txn_seq"))
            .unwrap()
            .is_none());
        store.rollback_txn(txn).unwrap();
    }

    #[test]
    fn list_tables_in_txn_sees_uncommitted() {
        let store = CatalogStore::new();
        let txn = TxnId::new(103);
        store.begin_txn(txn).unwrap();
        store.create_table(txn, make_table("txn_t1")).unwrap();
        store.create_table(txn, make_table("txn_t2")).unwrap();

        let tables = store.list_tables(txn, SchemaId::new(1)).unwrap();
        assert_eq!(tables.len(), 2);
        let tables_auto = store.list_tables(auto(), SchemaId::new(1)).unwrap();
        assert_eq!(tables_auto.len(), 0);
        store.rollback_txn(txn).unwrap();
    }

    // SECURITY: regression for the autocommit-batch path forgetting to
    // mirror the bumped `state.revision` into `cached_revision`. The
    // lock-free reader path uses `cached_revision`, so a stale value
    // makes revision-keyed caches (search-path schema cache, prepared
    // statement re-describe) believe the catalog is unchanged after
    // autocommit DDL or GRANT/REVOKE.
    #[test]
    fn cached_revision_advances_after_autocommit_writes() {
        let store = CatalogStore::new();
        let baseline = store.catalog_revision(auto()).unwrap();

        // Any autocommit write must bump the revision visible to the
        // lock-free reader. `create_table` flows through
        // `write_catalog_state_with_record` -> autocommit batch.
        store.create_table(auto(), make_table("t_rev1")).unwrap();
        let after_create = store.catalog_revision(auto()).unwrap();
        assert!(
            after_create > baseline,
            "cached_revision must advance after autocommit CREATE TABLE \
             (was {baseline}, now {after_create})"
        );

        store.create_table(auto(), make_table("t_rev2")).unwrap();
        let after_second = store.catalog_revision(auto()).unwrap();
        assert!(
            after_second > after_create,
            "cached_revision must advance on every autocommit write \
             (was {after_create}, now {after_second})"
        );
    }
}
