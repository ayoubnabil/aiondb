#![allow(clippy::missing_errors_doc)]

use aiondb_core::{
    ColumnId, DbError, DbResult, IndexId, RelationId, SchemaId, SequenceId, TextTypeModifier, TxnId,
};

use crate::{
    descriptors::{
        CastDescriptor, ColumnDescriptor, CommentDescriptor, DomainDescriptor, FunctionDescriptor,
        IndexDescriptor, PolicyDescriptor, PrivilegeDescriptor, QualifiedName, RoleDescriptor,
        RuleDescriptor, SchemaDescriptor, TableDescriptor, TenantDescriptor, TriggerDescriptor,
        UserTypeDescriptor, ViewDescriptor,
    },
    graph::{EdgeLabelDescriptor, NodeLabelDescriptor},
    sequence::SequenceDescriptor,
    statistics::TableStatistics,
};

/// Bundle of catalog reads needed by the optimizer's access-path selection.
///
/// Backends that hold a single internal lock for catalog reads can implement
/// [`CatalogReader::get_access_path_metadata`] to amortize the lock across
/// the three constituent reads (table, indexes, statistics) instead of paying
/// it three times per query.
#[derive(Clone, Debug)]
pub struct AccessPathMetadata {
    pub table: TableDescriptor,
    pub indexes: Vec<IndexDescriptor>,
    pub stats: Option<TableStatistics>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum TableAlteration {
    AddColumn(ColumnDescriptor),
    DropColumn {
        column_id: ColumnId,
    },
    RenameTable {
        new_name: QualifiedName,
    },
    RenameColumn {
        column_id: ColumnId,
        new_name: String,
    },
    SetDefault {
        column_id: ColumnId,
        default_expr: String,
    },
    DropDefault {
        column_id: ColumnId,
    },
    SetNotNull {
        column_id: ColumnId,
    },
    DropNotNull {
        column_id: ColumnId,
    },
    AddConstraint {
        constraint_type: String,
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
    },
    DropConstraint {
        constraint_name: String,
    },
    AlterColumnType {
        column_id: ColumnId,
        new_type: aiondb_core::DataType,
        raw_type_name: Option<String>,
        text_type_modifier: Option<TextTypeModifier>,
    },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum IndexAlteration {
    Rename { new_name: QualifiedName },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SequenceAlteration {
    Rename {
        new_name: QualifiedName,
    },
    RestartWith {
        value: i64,
    },
    SetOwnedBy {
        table_id: Option<RelationId>,
        column_id: Option<ColumnId>,
    },
}

pub trait CatalogReader: Send + Sync + std::fmt::Debug {
    fn get_schema(&self, txn: TxnId, name: &QualifiedName) -> DbResult<Option<SchemaDescriptor>>;
    fn list_schemas(&self, txn: TxnId) -> DbResult<Vec<SchemaDescriptor>> {
        Ok(self
            .get_schema(txn, &QualifiedName::unqualified("public"))?
            .into_iter()
            .collect())
    }
    fn get_table(&self, txn: TxnId, name: &QualifiedName) -> DbResult<Option<TableDescriptor>>;
    fn get_table_by_id(
        &self,
        txn: TxnId,
        table_id: RelationId,
    ) -> DbResult<Option<TableDescriptor>>;
    fn get_table_type_name(&self, _txn: TxnId, _table_id: RelationId) -> DbResult<Option<String>> {
        Ok(None)
    }
    fn list_tables(&self, txn: TxnId, schema_id: SchemaId) -> DbResult<Vec<TableDescriptor>>;
    fn list_indexes(&self, txn: TxnId, table_id: RelationId) -> DbResult<Vec<IndexDescriptor>>;
    fn get_index(&self, txn: TxnId, index_id: IndexId) -> DbResult<Option<IndexDescriptor>>;
    fn get_sequence(
        &self,
        txn: TxnId,
        name: &QualifiedName,
    ) -> DbResult<Option<SequenceDescriptor>>;
    fn list_sequences(
        &self,
        _txn: TxnId,
        _schema_id: SchemaId,
    ) -> DbResult<Vec<SequenceDescriptor>> {
        Ok(Vec::new())
    }
    fn get_statistics(&self, txn: TxnId, table_id: RelationId)
        -> DbResult<Option<TableStatistics>>;

    /// Bundle the table descriptor + index list + statistics in a single read.
    ///
    /// The default implementation calls each constituent reader, which in
    /// turn acquires the catalog read lock three separate times. Backends
    /// where catalog state lives behind a single lock can override this to
    /// gather all three under one lock acquisition; that's a meaningful
    /// per-query saving on OLTP workloads where every plan touches the
    /// optimizer's access-path selector.
    fn get_access_path_metadata(
        &self,
        txn: TxnId,
        table_id: RelationId,
    ) -> DbResult<Option<AccessPathMetadata>> {
        let Some(table) = self.get_table_by_id(txn, table_id)? else {
            return Ok(None);
        };
        let indexes = self.list_indexes(txn, table_id)?;
        let stats = self.get_statistics(txn, table_id)?;
        Ok(Some(AccessPathMetadata {
            table,
            indexes,
            stats,
        }))
    }
    fn get_view(&self, txn: TxnId, name: &QualifiedName) -> DbResult<Option<ViewDescriptor>>;
    fn list_views(&self, txn: TxnId, schema_id: SchemaId) -> DbResult<Vec<ViewDescriptor>>;

    fn list_node_labels(&self, _txn: TxnId) -> DbResult<Vec<NodeLabelDescriptor>> {
        Ok(Vec::new())
    }
    fn get_node_label(&self, _txn: TxnId, _label: &str) -> DbResult<Option<NodeLabelDescriptor>> {
        Ok(None)
    }
    fn list_edge_labels(&self, _txn: TxnId) -> DbResult<Vec<EdgeLabelDescriptor>> {
        Ok(Vec::new())
    }
    fn get_edge_label(&self, _txn: TxnId, _label: &str) -> DbResult<Option<EdgeLabelDescriptor>> {
        Ok(None)
    }
    fn get_role(&self, _txn: TxnId, _name: &str) -> DbResult<Option<RoleDescriptor>> {
        Ok(None)
    }
    fn list_roles(&self, _txn: TxnId) -> DbResult<Vec<RoleDescriptor>> {
        Ok(Vec::new())
    }
    fn get_privileges(&self, _txn: TxnId, _role_name: &str) -> DbResult<Vec<PrivilegeDescriptor>> {
        Ok(Vec::new())
    }

    fn get_tenant(&self, _txn: TxnId, _name: &str) -> DbResult<Option<TenantDescriptor>> {
        Ok(None)
    }
    fn list_tenants(&self, _txn: TxnId) -> DbResult<Vec<TenantDescriptor>> {
        Ok(Vec::new())
    }

    fn get_function(&self, _txn: TxnId, _name: &str) -> DbResult<Option<FunctionDescriptor>> {
        Ok(None)
    }
    fn list_functions(&self, _txn: TxnId) -> DbResult<Vec<FunctionDescriptor>> {
        Ok(Vec::new())
    }
    fn get_domain(&self, _txn: TxnId, _name: &str) -> DbResult<Option<DomainDescriptor>> {
        Ok(None)
    }
    fn list_domains(&self, _txn: TxnId) -> DbResult<Vec<DomainDescriptor>> {
        Ok(Vec::new())
    }
    fn get_user_type(&self, _txn: TxnId, _name: &str) -> DbResult<Option<UserTypeDescriptor>> {
        Ok(None)
    }
    fn list_user_types(&self, _txn: TxnId) -> DbResult<Vec<UserTypeDescriptor>> {
        Ok(Vec::new())
    }
    fn list_casts(&self, _txn: TxnId) -> DbResult<Vec<CastDescriptor>> {
        Ok(Vec::new())
    }
    fn list_policies(&self, _txn: TxnId) -> DbResult<Vec<PolicyDescriptor>> {
        Ok(Vec::new())
    }
    fn list_rules(&self, _txn: TxnId) -> DbResult<Vec<RuleDescriptor>> {
        Ok(Vec::new())
    }
    fn list_comments(&self, _txn: TxnId) -> DbResult<Vec<CommentDescriptor>> {
        Ok(Vec::new())
    }
    fn list_triggers(&self, _txn: TxnId, _table_name: &str) -> DbResult<Vec<TriggerDescriptor>> {
        Ok(Vec::new())
    }

    /// Monotonic catalog revision visible from the given transaction context.
    ///
    /// Implementations that do not track revisions may return `0`.
    fn catalog_revision(&self, _txn: TxnId) -> DbResult<u64> {
        Ok(0)
    }
}

pub trait CatalogWriter: Send + Sync + std::fmt::Debug {
    fn create_schema(&self, txn: TxnId, schema: SchemaDescriptor) -> DbResult<SchemaId>;
    fn drop_schema(&self, txn: TxnId, schema_id: SchemaId) -> DbResult<()>;
    fn create_table(&self, txn: TxnId, table: TableDescriptor) -> DbResult<RelationId>;
    fn set_table_type_name(
        &self,
        _txn: TxnId,
        _table_id: RelationId,
        _type_name: Option<String>,
    ) -> DbResult<()> {
        Ok(())
    }
    fn create_index(&self, txn: TxnId, index: IndexDescriptor) -> DbResult<IndexId>;
    fn create_sequence(&self, txn: TxnId, sequence: SequenceDescriptor) -> DbResult<SequenceId>;
    fn alter_table(
        &self,
        txn: TxnId,
        table_id: RelationId,
        alteration: TableAlteration,
    ) -> DbResult<()>;
    fn alter_index(
        &self,
        txn: TxnId,
        index_id: IndexId,
        alteration: IndexAlteration,
    ) -> DbResult<()>;
    fn alter_sequence(
        &self,
        txn: TxnId,
        sequence_id: SequenceId,
        alteration: SequenceAlteration,
    ) -> DbResult<()>;
    fn drop_table(&self, txn: TxnId, table_id: RelationId) -> DbResult<()>;
    fn drop_index(&self, txn: TxnId, index_id: IndexId) -> DbResult<()>;
    fn drop_sequence(&self, txn: TxnId, sequence_id: SequenceId) -> DbResult<()>;
    fn create_view(&self, txn: TxnId, view: ViewDescriptor) -> DbResult<RelationId>;
    fn drop_view(&self, txn: TxnId, view_id: RelationId) -> DbResult<()>;
    fn update_statistics(&self, txn: TxnId, stats: TableStatistics) -> DbResult<()>;

    fn create_node_label(&self, _txn: TxnId, _label: NodeLabelDescriptor) -> DbResult<()> {
        Err(DbError::internal(
            "graph labels are not supported by this catalog",
        ))
    }
    fn create_edge_label(&self, _txn: TxnId, _label: EdgeLabelDescriptor) -> DbResult<()> {
        Err(DbError::internal(
            "graph labels are not supported by this catalog",
        ))
    }
    fn drop_node_label(&self, _txn: TxnId, _label: &str) -> DbResult<()> {
        Err(DbError::internal(
            "graph labels are not supported by this catalog",
        ))
    }
    fn drop_edge_label(&self, _txn: TxnId, _label: &str) -> DbResult<()> {
        Err(DbError::internal(
            "graph labels are not supported by this catalog",
        ))
    }
    fn create_role(&self, txn: TxnId, role: RoleDescriptor) -> DbResult<()>;
    fn alter_role(&self, txn: TxnId, name: &str, role: RoleDescriptor) -> DbResult<()>;
    fn drop_role(&self, txn: TxnId, name: &str) -> DbResult<()>;
    fn grant_privilege(&self, txn: TxnId, privilege: PrivilegeDescriptor) -> DbResult<()>;
    fn revoke_privilege(&self, txn: TxnId, privilege: PrivilegeDescriptor) -> DbResult<()>;

    fn create_tenant(&self, txn: TxnId, name: &str) -> DbResult<TenantDescriptor>;
    fn drop_tenant(&self, txn: TxnId, name: &str) -> DbResult<()>;

    fn create_function(&self, txn: TxnId, func: FunctionDescriptor) -> DbResult<()>;
    fn drop_function(&self, txn: TxnId, name: &str) -> DbResult<()>;
    /// Atomic CREATE OR REPLACE FUNCTION: replaces an existing overload
    /// matching `func`'s name and parameter signature in place; appends
    /// the descriptor when no matching overload exists. Avoids the
    /// drop-all-then-recreate-survivors loop that loses surviving
    /// overloads if any recreate fails mid-loop in autocommit mode.
    fn replace_or_create_function(&self, txn: TxnId, func: FunctionDescriptor) -> DbResult<()> {
        // Default impl preserves the previous behaviour for backends that
        // haven't been ported yet (drop all overloads, recreate). The
        // catalog-store impl overrides this with an atomic in-place swap.
        self.drop_function(txn, &func.name).ok();
        self.create_function(txn, func)
    }
    /// Atomic DROP FUNCTION targeting one specific overload by parameter
    /// signature. Preserves siblings without round-tripping through
    /// drop-all-then-recreate-survivors. Returns Ok(false) if no
    /// matching overload exists.
    fn drop_function_overload(
        &self,
        txn: TxnId,
        name: &str,
        param_types: &[aiondb_core::DataType],
    ) -> DbResult<bool> {
        let _ = (txn, name, param_types);
        // Default impl: backends that haven't been ported still go
        // through the drop-all path; engine-side caller falls back to
        // the loop on `Ok(false)`. Catalog-store overrides.
        Ok(false)
    }
    fn create_domain(&self, txn: TxnId, domain: DomainDescriptor) -> DbResult<()>;
    fn drop_domain(&self, txn: TxnId, name: &str) -> DbResult<()>;
    fn alter_domain(&self, txn: TxnId, domain: DomainDescriptor) -> DbResult<()>;
    fn create_user_type(&self, txn: TxnId, user_type: UserTypeDescriptor) -> DbResult<()>;
    fn drop_user_type(&self, txn: TxnId, name: &str) -> DbResult<()>;
    fn alter_user_type(&self, txn: TxnId, user_type: UserTypeDescriptor) -> DbResult<()>;
    fn create_cast(&self, txn: TxnId, cast: CastDescriptor) -> DbResult<()>;
    /// Drop the cast with the given (source, target) pair. Both names
    /// are normalised by the catalog implementation.
    fn drop_cast(&self, txn: TxnId, source_type: &str, target_type: &str) -> DbResult<()>;
    fn create_policy(&self, txn: TxnId, policy: PolicyDescriptor) -> DbResult<()>;
    /// Drop the policy identified by `(name, table_name)`. The catalog
    /// normalises both identifiers before lookup.
    fn drop_policy(&self, txn: TxnId, name: &str, table_name: &str) -> DbResult<()>;
    fn alter_policy(&self, txn: TxnId, policy: PolicyDescriptor) -> DbResult<()>;
    fn create_rule(&self, txn: TxnId, rule: RuleDescriptor) -> DbResult<()>;
    /// Drop the rule identified by `(name, table_name)`.
    fn drop_rule(&self, txn: TxnId, name: &str, table_name: &str) -> DbResult<()>;
    fn set_comment(&self, _txn: TxnId, _comment: CommentDescriptor) -> DbResult<()> {
        Err(DbError::internal(
            "comments are not supported by this catalog",
        ))
    }
    fn drop_comment(
        &self,
        _txn: TxnId,
        _object_type: &str,
        _object_identity: &str,
    ) -> DbResult<()> {
        Err(DbError::internal(
            "comments are not supported by this catalog",
        ))
    }
    fn create_trigger(&self, txn: TxnId, trigger: TriggerDescriptor) -> DbResult<()>;
    fn drop_trigger(&self, txn: TxnId, name: &str, table_name: &str) -> DbResult<()>;
    fn rename_trigger(
        &self,
        txn: TxnId,
        name: &str,
        table_name: &str,
        new_name: &str,
    ) -> DbResult<()>;
}

pub trait SequenceManager: Send + Sync + std::fmt::Debug {
    fn next_value(&self, txn: TxnId, sequence_id: SequenceId) -> DbResult<i64>;
    fn next_values(&self, txn: TxnId, sequence_id: SequenceId, count: usize) -> DbResult<Vec<i64>> {
        let mut values = Vec::with_capacity(count);
        for _ in 0..count {
            values.push(self.next_value(txn, sequence_id)?);
        }
        Ok(values)
    }
    fn set_value(
        &self,
        txn: TxnId,
        sequence_id: SequenceId,
        value: i64,
        is_called: bool,
    ) -> DbResult<()>;
}

pub trait CatalogTxnParticipant: Send + Sync + std::fmt::Debug {
    fn begin_txn(&self, txn: TxnId) -> DbResult<()>;
    /// Return whether the transaction currently has pending catalog writes.
    ///
    /// Engines can use this as a contention hint to avoid taking heavyweight
    /// global commit coordination locks for pure DML transactions.
    fn txn_writes_catalog(&self, _txn: TxnId) -> DbResult<bool> {
        Ok(true)
    }
    /// Validate that the transaction can commit without publishing changes.
    ///
    /// Implementations should use this to surface serialization/conflict
    /// failures before other subsystems publish durable state.
    fn validate_commit_txn(&self, _txn: TxnId) -> DbResult<()> {
        Ok(())
    }
    fn commit_txn(&self, txn: TxnId) -> DbResult<()>;
    fn rollback_txn(&self, txn: TxnId) -> DbResult<()>;

    /// Create a savepoint for the given transaction, returning an opaque id.
    fn create_savepoint(&self, _txn: TxnId) -> DbResult<u64> {
        Err(DbError::internal(
            "savepoints are not supported by this catalog",
        ))
    }

    /// Rollback the catalog to the given savepoint.
    fn rollback_to_savepoint(&self, _txn: TxnId, _savepoint_id: u64) -> DbResult<()> {
        Err(DbError::internal(
            "savepoints are not supported by this catalog",
        ))
    }

    /// Release the given savepoint.
    fn release_savepoint(&self, _txn: TxnId, _savepoint_id: u64) -> DbResult<()> {
        Err(DbError::internal(
            "savepoints are not supported by this catalog",
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aiondb_core::DataType;

    // ===================================================================
    // TableAlteration
    // ===================================================================

    // --- AddColumn variant construction and equality ---
    #[test]
    fn table_alteration_add_column() {
        let col = ColumnDescriptor {
            column_id: ColumnId::new(5),
            name: "new_col".to_owned(),
            data_type: DataType::Text,
            raw_type_name: None,
            text_type_modifier: None,
            nullable: true,
            ordinal_position: 3,
            default_value: Some("hello".to_owned()),
        };
        let alt = TableAlteration::AddColumn(col.clone());
        if let TableAlteration::AddColumn(ref c) = alt {
            assert_eq!(c, &col);
        } else {
            unreachable!("expected AddColumn");
        }
    }

    // --- DropColumn variant ---
    #[test]
    fn table_alteration_drop_column() {
        let alt = TableAlteration::DropColumn {
            column_id: ColumnId::new(3),
        };
        if let TableAlteration::DropColumn { column_id } = alt {
            assert_eq!(column_id, ColumnId::new(3));
        } else {
            unreachable!("expected DropColumn");
        }
    }

    // --- RenameTable variant ---
    #[test]
    fn table_alteration_rename_table() {
        let new_name = QualifiedName::qualified("public", "new_table_name");
        let alt = TableAlteration::RenameTable {
            new_name: new_name.clone(),
        };
        if let TableAlteration::RenameTable { new_name: n } = alt {
            assert_eq!(n, new_name);
        } else {
            unreachable!("expected RenameTable");
        }
    }

    // --- RenameColumn variant ---
    #[test]
    fn table_alteration_rename_column() {
        let alt = TableAlteration::RenameColumn {
            column_id: ColumnId::new(2),
            new_name: "renamed".to_owned(),
        };
        if let TableAlteration::RenameColumn {
            column_id,
            new_name,
        } = alt
        {
            assert_eq!(column_id, ColumnId::new(2));
            assert_eq!(new_name, "renamed");
        } else {
            unreachable!("expected RenameColumn");
        }
    }

    // --- TableAlteration variants are distinct ---
    #[test]
    fn table_alteration_variants_not_equal() {
        let col = ColumnDescriptor {
            column_id: ColumnId::new(1),
            name: "c".to_owned(),
            data_type: DataType::Int,
            raw_type_name: None,
            text_type_modifier: None,
            nullable: false,
            ordinal_position: 0,
            default_value: None,
        };
        let add = TableAlteration::AddColumn(col);
        let drop = TableAlteration::DropColumn {
            column_id: ColumnId::new(1),
        };
        let rename_table = TableAlteration::RenameTable {
            new_name: QualifiedName::unqualified("x"),
        };
        let rename_col = TableAlteration::RenameColumn {
            column_id: ColumnId::new(1),
            new_name: "y".to_owned(),
        };
        assert_ne!(add, drop);
        assert_ne!(drop, rename_table);
        assert_ne!(rename_table, rename_col);
        assert_ne!(add, rename_col);
    }

    // --- TableAlteration Clone produces equal ---
    #[test]
    fn table_alteration_clone_eq() {
        let alt = TableAlteration::DropColumn {
            column_id: ColumnId::new(7),
        };
        assert_eq!(alt, alt.clone());
    }

    // --- TableAlteration Debug format ---
    #[test]
    fn table_alteration_debug() {
        let alt = TableAlteration::RenameTable {
            new_name: QualifiedName::unqualified("t"),
        };
        let dbg = format!("{alt:?}");
        assert!(dbg.contains("RenameTable"));
    }

    // --- AddColumn with no default ---
    #[test]
    fn table_alteration_add_column_no_default() {
        let col = ColumnDescriptor {
            column_id: ColumnId::new(1),
            name: "x".to_owned(),
            data_type: DataType::Boolean,
            raw_type_name: None,
            text_type_modifier: None,
            nullable: false,
            ordinal_position: 0,
            default_value: None,
        };
        let alt = TableAlteration::AddColumn(col);
        if let TableAlteration::AddColumn(c) = alt {
            assert!(c.default_value.is_none());
        }
    }

    // --- RenameTable with unqualified name ---
    #[test]
    fn table_alteration_rename_table_unqualified() {
        let alt = TableAlteration::RenameTable {
            new_name: QualifiedName::unqualified("simple"),
        };
        if let TableAlteration::RenameTable { new_name } = alt {
            assert!(new_name.schema.is_none());
            assert_eq!(new_name.name, "simple");
        }
    }

    // --- RenameColumn with empty string name ---
    #[test]
    fn table_alteration_rename_column_empty_name() {
        let alt = TableAlteration::RenameColumn {
            column_id: ColumnId::new(1),
            new_name: String::new(),
        };
        if let TableAlteration::RenameColumn { new_name, .. } = alt {
            assert!(new_name.is_empty());
        }
    }

    // ===================================================================
    // IndexAlteration
    // ===================================================================

    // --- Rename variant ---
    #[test]
    fn index_alteration_rename() {
        let alt = IndexAlteration::Rename {
            new_name: QualifiedName::qualified("myschema", "new_idx_name"),
        };
        let IndexAlteration::Rename { new_name } = alt;
        assert_eq!(
            new_name,
            QualifiedName::qualified("myschema", "new_idx_name")
        );
    }

    // --- IndexAlteration Clone produces equal ---
    #[test]
    fn index_alteration_clone_eq() {
        let alt = IndexAlteration::Rename {
            new_name: QualifiedName::unqualified("idx"),
        };
        assert_eq!(alt, alt.clone());
    }

    // --- IndexAlteration Debug format ---
    #[test]
    fn index_alteration_debug() {
        let alt = IndexAlteration::Rename {
            new_name: QualifiedName::unqualified("idx"),
        };
        let dbg = format!("{alt:?}");
        assert!(dbg.contains("Rename"));
    }

    // --- Two IndexAlteration::Rename with different names are not equal ---
    #[test]
    fn index_alteration_rename_different_names_ne() {
        let a = IndexAlteration::Rename {
            new_name: QualifiedName::unqualified("a"),
        };
        let b = IndexAlteration::Rename {
            new_name: QualifiedName::unqualified("b"),
        };
        assert_ne!(a, b);
    }

    // --- IndexAlteration Rename with qualified name ---
    #[test]
    fn index_alteration_rename_with_qualified_name() {
        let alt = IndexAlteration::Rename {
            new_name: QualifiedName::qualified("schema", "idx_name"),
        };
        let IndexAlteration::Rename { ref new_name } = alt;
        assert_eq!(new_name.schema_name(), Some("schema"));
        assert_eq!(new_name.object_name(), "idx_name");
    }

    // ===================================================================
    // SequenceAlteration
    // ===================================================================

    // --- Rename variant ---
    #[test]
    fn sequence_alteration_rename() {
        let alt = SequenceAlteration::Rename {
            new_name: QualifiedName::qualified("public", "renamed_seq"),
        };
        if let SequenceAlteration::Rename { new_name } = alt {
            assert_eq!(new_name, QualifiedName::qualified("public", "renamed_seq"));
        } else {
            unreachable!("expected Rename");
        }
    }

    // --- RestartWith variant ---
    #[test]
    fn sequence_alteration_restart_with() {
        let alt = SequenceAlteration::RestartWith { value: 100 };
        if let SequenceAlteration::RestartWith { value } = alt {
            assert_eq!(value, 100);
        } else {
            unreachable!("expected RestartWith");
        }
    }

    // --- RestartWith with negative value ---
    #[test]
    fn sequence_alteration_restart_with_negative() {
        let alt = SequenceAlteration::RestartWith { value: -50 };
        if let SequenceAlteration::RestartWith { value } = alt {
            assert_eq!(value, -50);
        } else {
            unreachable!("expected RestartWith");
        }
    }

    // --- RestartWith with zero ---
    #[test]
    fn sequence_alteration_restart_with_zero() {
        let alt = SequenceAlteration::RestartWith { value: 0 };
        if let SequenceAlteration::RestartWith { value } = alt {
            assert_eq!(value, 0);
        }
    }

    // --- RestartWith with i64::MAX ---
    #[test]
    fn sequence_alteration_restart_with_max() {
        let alt = SequenceAlteration::RestartWith { value: i64::MAX };
        if let SequenceAlteration::RestartWith { value } = alt {
            assert_eq!(value, i64::MAX);
        }
    }

    // --- RestartWith with i64::MIN ---
    #[test]
    fn sequence_alteration_restart_with_min() {
        let alt = SequenceAlteration::RestartWith { value: i64::MIN };
        if let SequenceAlteration::RestartWith { value } = alt {
            assert_eq!(value, i64::MIN);
        }
    }

    // --- SetOwnedBy with both Some ---
    #[test]
    fn sequence_alteration_set_owned_by_both_some() {
        let alt = SequenceAlteration::SetOwnedBy {
            table_id: Some(RelationId::new(10)),
            column_id: Some(ColumnId::new(3)),
        };
        if let SequenceAlteration::SetOwnedBy {
            table_id,
            column_id,
        } = alt
        {
            assert_eq!(table_id, Some(RelationId::new(10)));
            assert_eq!(column_id, Some(ColumnId::new(3)));
        } else {
            unreachable!("expected SetOwnedBy");
        }
    }

    // --- SetOwnedBy with both None (disown) ---
    #[test]
    fn sequence_alteration_set_owned_by_both_none() {
        let alt = SequenceAlteration::SetOwnedBy {
            table_id: None,
            column_id: None,
        };
        if let SequenceAlteration::SetOwnedBy {
            table_id,
            column_id,
        } = alt
        {
            assert!(table_id.is_none());
            assert!(column_id.is_none());
        }
    }

    // --- SetOwnedBy with mixed Some/None ---
    #[test]
    fn sequence_alteration_set_owned_by_mixed() {
        let alt = SequenceAlteration::SetOwnedBy {
            table_id: Some(RelationId::new(5)),
            column_id: None,
        };
        if let SequenceAlteration::SetOwnedBy {
            table_id,
            column_id,
        } = alt
        {
            assert!(table_id.is_some());
            assert!(column_id.is_none());
        }
    }

    // --- SequenceAlteration variants are distinct ---
    #[test]
    fn sequence_alteration_variants_not_equal() {
        let rename = SequenceAlteration::Rename {
            new_name: QualifiedName::unqualified("x"),
        };
        let restart = SequenceAlteration::RestartWith { value: 1 };
        let set_owned = SequenceAlteration::SetOwnedBy {
            table_id: None,
            column_id: None,
        };
        assert_ne!(rename, restart);
        assert_ne!(restart, set_owned);
        assert_ne!(rename, set_owned);
    }

    // --- SequenceAlteration Clone produces equal ---
    #[test]
    fn sequence_alteration_clone_eq() {
        let alt = SequenceAlteration::RestartWith { value: 42 };
        assert_eq!(alt, alt.clone());
    }

    // --- SequenceAlteration Debug format ---
    #[test]
    fn sequence_alteration_debug() {
        let alt = SequenceAlteration::SetOwnedBy {
            table_id: Some(RelationId::new(1)),
            column_id: Some(ColumnId::new(2)),
        };
        let dbg = format!("{alt:?}");
        assert!(dbg.contains("SetOwnedBy"));
    }

    // --- Same variant, different values are not equal ---
    #[test]
    fn sequence_alteration_same_variant_different_values_ne() {
        let a = SequenceAlteration::RestartWith { value: 1 };
        let b = SequenceAlteration::RestartWith { value: 2 };
        assert_ne!(a, b);
    }
}
