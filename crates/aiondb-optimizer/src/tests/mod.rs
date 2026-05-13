use std::{cmp::Ordering, ops::Bound, sync::Arc};

use super::*;
use aiondb_catalog::{
    CatalogReader, ColumnDescriptor, IndexDescriptor, IndexKeyColumn, QualifiedName, SortOrder,
    TableDescriptor, TableStatistics,
};
use aiondb_core::{ColumnId, DataType, DbResult, IndexId, RelationId, SchemaId, TxnId, Value};
use aiondb_plan::{
    ColumnPlan, LogicalPlan, PhysicalPlan, ProjectionExpr, ResultField, ScanAccessPath, TypedExpr,
};

fn make_projection(name: &str, dt: DataType) -> ProjectionExpr {
    ProjectionExpr {
        field: ResultField {
            name: name.to_owned(),
            data_type: dt.clone(),
            text_type_modifier: None,
            nullable: false,
        },
        expr: TypedExpr::literal(Value::Int(1), dt, false),
    }
}

fn make_table_descriptor() -> TableDescriptor {
    TableDescriptor {
        table_id: RelationId::new(1),
        schema_id: SchemaId::new(1),
        name: QualifiedName::qualified("public", "users"),
        columns: vec![
            ColumnDescriptor {
                column_id: ColumnId::new(10),
                name: "id".to_owned(),
                data_type: DataType::Int,
                raw_type_name: None,
                text_type_modifier: None,
                nullable: false,
                ordinal_position: 0,
                default_value: None,
            },
            ColumnDescriptor {
                column_id: ColumnId::new(20),
                name: "name".to_owned(),
                data_type: DataType::Text,
                raw_type_name: None,
                text_type_modifier: None,
                nullable: true,
                ordinal_position: 1,
                default_value: None,
            },
        ],
        identity_columns: Vec::new(),
        primary_key: Some(vec![ColumnId::new(10)]),
        foreign_keys: Vec::new(),
        check_constraints: Vec::new(),
        shard_config: None,
        owner: None,
    }
}

#[derive(Debug)]
struct TestCatalog {
    table: TableDescriptor,
    indexes: Vec<IndexDescriptor>,
    statistics: Option<TableStatistics>,
}

impl CatalogReader for TestCatalog {
    fn get_schema(
        &self,
        _txn: TxnId,
        _name: &QualifiedName,
    ) -> DbResult<Option<aiondb_catalog::SchemaDescriptor>> {
        Ok(None)
    }

    fn get_table(&self, _txn: TxnId, _name: &QualifiedName) -> DbResult<Option<TableDescriptor>> {
        Ok(Some(self.table.clone()))
    }

    fn get_table_by_id(
        &self,
        _txn: TxnId,
        _table_id: RelationId,
    ) -> DbResult<Option<TableDescriptor>> {
        Ok(Some(self.table.clone()))
    }

    fn list_tables(&self, _txn: TxnId, _schema_id: SchemaId) -> DbResult<Vec<TableDescriptor>> {
        Ok(vec![self.table.clone()])
    }

    fn list_indexes(&self, _txn: TxnId, _table_id: RelationId) -> DbResult<Vec<IndexDescriptor>> {
        Ok(self.indexes.clone())
    }

    fn get_index(&self, _txn: TxnId, _index_id: IndexId) -> DbResult<Option<IndexDescriptor>> {
        Ok(self.indexes.first().cloned())
    }

    fn get_sequence(
        &self,
        _txn: TxnId,
        _name: &QualifiedName,
    ) -> DbResult<Option<aiondb_catalog::SequenceDescriptor>> {
        Ok(None)
    }

    fn get_statistics(
        &self,
        _txn: TxnId,
        _table_id: RelationId,
    ) -> DbResult<Option<aiondb_catalog::TableStatistics>> {
        Ok(self.statistics.clone())
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
}

fn make_single_column_index(index_id: u64, column_id: u64) -> IndexDescriptor {
    IndexDescriptor {
        index_id: IndexId::new(index_id),
        schema_id: SchemaId::new(1),
        table_id: RelationId::new(1),
        name: QualifiedName::qualified("public", "idx_test"),
        unique: true,
        nulls_not_distinct: false,
        kind: aiondb_catalog::IndexKind::BTree,
        key_columns: vec![IndexKeyColumn {
            column_id: ColumnId::new(column_id),
            sort_order: SortOrder::Ascending,
            nulls_first: false,
        }],
        include_columns: vec![],
        constraint_name: None,
        hnsw_params: None,
    }
}

/// Table with three columns: a (ordinal 0, col_id 10), b (ordinal 1, col_id 20), c (ordinal 2, col_id 30).
fn make_three_column_table() -> TableDescriptor {
    TableDescriptor {
        table_id: RelationId::new(1),
        schema_id: SchemaId::new(1),
        name: QualifiedName::qualified("public", "orders"),
        columns: vec![
            ColumnDescriptor {
                column_id: ColumnId::new(10),
                name: "a".to_owned(),
                data_type: DataType::Int,
                raw_type_name: None,
                text_type_modifier: None,
                nullable: false,
                ordinal_position: 0,
                default_value: None,
            },
            ColumnDescriptor {
                column_id: ColumnId::new(20),
                name: "b".to_owned(),
                data_type: DataType::Int,
                raw_type_name: None,
                text_type_modifier: None,
                nullable: false,
                ordinal_position: 1,
                default_value: None,
            },
            ColumnDescriptor {
                column_id: ColumnId::new(30),
                name: "c".to_owned(),
                data_type: DataType::Int,
                raw_type_name: None,
                text_type_modifier: None,
                nullable: false,
                ordinal_position: 2,
                default_value: None,
            },
        ],
        identity_columns: Vec::new(),
        primary_key: Some(vec![ColumnId::new(10)]),
        foreign_keys: Vec::new(),
        check_constraints: Vec::new(),
        shard_config: None,
        owner: None,
    }
}

/// Composite BTree index on columns (a, b, c) with the given column IDs.
fn make_composite_index(index_id: u64, column_ids: &[u64]) -> IndexDescriptor {
    IndexDescriptor {
        index_id: IndexId::new(index_id),
        schema_id: SchemaId::new(1),
        table_id: RelationId::new(1),
        name: QualifiedName::qualified("public", "idx_composite"),
        unique: true,
        nulls_not_distinct: false,
        kind: aiondb_catalog::IndexKind::BTree,
        key_columns: column_ids
            .iter()
            .map(|&id| IndexKeyColumn {
                column_id: ColumnId::new(id),
                sort_order: SortOrder::Ascending,
                nulls_first: false,
            })
            .collect(),
        include_columns: vec![],
        constraint_name: None,
        hnsw_params: None,
    }
}

/// Build a ProjectTable leaf with named columns (nullable:true).
/// Shared by multiple test modules.
fn make_nullable_scan_leaf(table_id: RelationId, col_names: &[&str]) -> LogicalPlan {
    LogicalPlan::ProjectTable {
        table_id,
        outputs: col_names
            .iter()
            .enumerate()
            .map(|(i, name)| ProjectionExpr {
                field: ResultField {
                    name: name.to_string(),
                    data_type: DataType::Int,
                    text_type_modifier: None,
                    nullable: true,
                },
                expr: TypedExpr::column_ref(*name, i, DataType::Int, true),
            })
            .collect(),
        filter: None,
        order_by: Vec::new(),
        limit: None,
        offset: None,
        distinct: false,
        distinct_on: Vec::new(),
    }
}

mod access_path_and_cost;
#[path = "advanced_optimizations.rs"]
mod advanced_optimizations;
mod catalog_and_ranges;
mod join_optimization;
mod lookup_extraction;
mod optimize_basic;
#[path = "outer_join_simplify.rs"]
mod outer_join_simplify;
