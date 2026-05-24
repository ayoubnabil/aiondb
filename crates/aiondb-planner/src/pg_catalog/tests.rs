use std::sync::Arc;

use aiondb_catalog::{
    CatalogReader, ColumnDescriptor, IndexDescriptor, IndexKeyColumn, IndexKind, QualifiedName,
    SchemaDescriptor, SequenceDescriptor, SortOrder, TableDescriptor, TableStatistics,
    ViewDescriptor,
};
use aiondb_core::{
    ColumnId, DataType, DbResult, IndexId, RelationId, SchemaId, TextTypeModifier, TxnId, Value,
};
use aiondb_parser::parse_prepared_statement;
use aiondb_plan::{LogicalPlan, TypedExprKind};

use super::*;

#[path = "tests_build_plan.rs"]
mod build_plan_tests;
#[path = "tests_lookup_and_output_fields.rs"]
mod lookup_and_output_fields;
#[path = "tests_output_field_aliases.rs"]
mod output_field_aliases;

// ---------------------------------------------------------------
// Mock catalog with a "public" schema and one table + one index
// ---------------------------------------------------------------

#[derive(Debug)]
struct MockCatalog;

const MOCK_SCHEMA_ID: SchemaId = SchemaId::new(1);

impl CatalogReader for MockCatalog {
    fn get_schema(&self, _txn: TxnId, name: &QualifiedName) -> DbResult<Option<SchemaDescriptor>> {
        if name.object_name().eq_ignore_ascii_case("public") {
            return Ok(Some(SchemaDescriptor {
                schema_id: MOCK_SCHEMA_ID,
                name: "public".to_owned(),
            }));
        }
        Ok(None)
    }

    fn get_table(&self, _txn: TxnId, name: &QualifiedName) -> DbResult<Option<TableDescriptor>> {
        if name.object_name().eq_ignore_ascii_case("users") {
            return Ok(Some(users_table()));
        }
        Ok(None)
    }

    fn get_table_by_id(
        &self,
        _txn: TxnId,
        _table_id: RelationId,
    ) -> DbResult<Option<TableDescriptor>> {
        Ok(None)
    }

    fn list_tables(&self, _txn: TxnId, schema_id: SchemaId) -> DbResult<Vec<TableDescriptor>> {
        if schema_id == MOCK_SCHEMA_ID {
            return Ok(vec![users_table()]);
        }
        Ok(Vec::new())
    }

    fn list_indexes(&self, _txn: TxnId, table_id: RelationId) -> DbResult<Vec<IndexDescriptor>> {
        if table_id == RelationId::new(1) {
            return Ok(vec![users_pk_index()]);
        }
        Ok(Vec::new())
    }

    fn get_index(&self, _txn: TxnId, _index_id: IndexId) -> DbResult<Option<IndexDescriptor>> {
        Ok(None)
    }

    fn get_sequence(
        &self,
        _txn: TxnId,
        name: &QualifiedName,
    ) -> DbResult<Option<SequenceDescriptor>> {
        if name.object_name().eq_ignore_ascii_case("user_ids_seq") {
            return Ok(Some(user_ids_sequence()));
        }
        Ok(None)
    }

    fn list_sequences(
        &self,
        _txn: TxnId,
        schema_id: SchemaId,
    ) -> DbResult<Vec<SequenceDescriptor>> {
        if schema_id == MOCK_SCHEMA_ID {
            return Ok(vec![user_ids_sequence()]);
        }
        Ok(Vec::new())
    }

    fn get_statistics(
        &self,
        _txn: TxnId,
        _table_id: RelationId,
    ) -> DbResult<Option<TableStatistics>> {
        Ok(None)
    }

    fn get_view(&self, _txn: TxnId, _name: &QualifiedName) -> DbResult<Option<ViewDescriptor>> {
        Ok(None)
    }

    fn list_views(&self, _txn: TxnId, _schema_id: SchemaId) -> DbResult<Vec<ViewDescriptor>> {
        Ok(Vec::new())
    }
}

fn users_table() -> TableDescriptor {
    TableDescriptor {
        table_id: RelationId::new(1),
        schema_id: MOCK_SCHEMA_ID,
        name: QualifiedName::unqualified("users"),
        columns: vec![
            ColumnDescriptor {
                column_id: ColumnId::new(1),
                name: "id".to_owned(),
                data_type: DataType::Int,
                raw_type_name: None,
                text_type_modifier: None,
                nullable: false,
                ordinal_position: 1,
                default_value: None,
            },
            ColumnDescriptor {
                column_id: ColumnId::new(2),
                name: "name".to_owned(),
                data_type: DataType::Text,
                raw_type_name: None,
                text_type_modifier: None,
                nullable: true,
                ordinal_position: 2,
                default_value: None,
            },
        ],
        identity_columns: Vec::new(),
        primary_key: Some(vec![ColumnId::new(1)]),
        foreign_keys: Vec::new(),
        check_constraints: Vec::new(),
        shard_config: None,
        owner: None,
    }
}

fn users_pk_index() -> IndexDescriptor {
    IndexDescriptor {
        index_id: IndexId::new(1),
        schema_id: MOCK_SCHEMA_ID,
        table_id: RelationId::new(1),
        name: QualifiedName::unqualified("users_pkey"),
        unique: true,
        nulls_not_distinct: false,
        kind: IndexKind::BTree,
        key_columns: vec![IndexKeyColumn {
            column_id: ColumnId::new(1),
            sort_order: SortOrder::Ascending,
            nulls_first: false,
        }],
        include_columns: Vec::new(),
        constraint_name: None,
        hnsw_params: None,
    }
}

fn user_ids_sequence() -> SequenceDescriptor {
    SequenceDescriptor {
        sequence_id: aiondb_core::SequenceId::new(1),
        schema_id: MOCK_SCHEMA_ID,
        name: QualifiedName::unqualified("user_ids_seq"),
        data_type: DataType::BigInt,
        start_value: 1,
        increment_by: 1,
        min_value: 1,
        max_value: i64::MAX,
        cache_size: 1,
        cycle: false,
        owned_by: None,
        owner: Some("aiondb".to_owned()),
    }
}

fn mock_catalog() -> Arc<dyn CatalogReader> {
    Arc::new(MockCatalog)
}

#[derive(Debug)]
struct MatviewCatalog;

impl CatalogReader for MatviewCatalog {
    fn get_schema(&self, _txn: TxnId, name: &QualifiedName) -> DbResult<Option<SchemaDescriptor>> {
        if name.object_name().eq_ignore_ascii_case("public") {
            return Ok(Some(SchemaDescriptor {
                schema_id: MOCK_SCHEMA_ID,
                name: "public".to_owned(),
            }));
        }
        Ok(None)
    }

    fn get_table(&self, _txn: TxnId, name: &QualifiedName) -> DbResult<Option<TableDescriptor>> {
        if name.object_name().eq_ignore_ascii_case("sales_snapshot") {
            return Ok(Some(sales_snapshot_table()));
        }
        Ok(None)
    }

    fn get_table_by_id(
        &self,
        _txn: TxnId,
        _table_id: RelationId,
    ) -> DbResult<Option<TableDescriptor>> {
        Ok(None)
    }

    fn list_tables(&self, _txn: TxnId, schema_id: SchemaId) -> DbResult<Vec<TableDescriptor>> {
        if schema_id == MOCK_SCHEMA_ID {
            return Ok(vec![sales_snapshot_table()]);
        }
        Ok(Vec::new())
    }

    fn list_indexes(&self, _txn: TxnId, _table_id: RelationId) -> DbResult<Vec<IndexDescriptor>> {
        Ok(Vec::new())
    }

    fn get_index(&self, _txn: TxnId, _index_id: IndexId) -> DbResult<Option<IndexDescriptor>> {
        Ok(None)
    }

    fn get_sequence(
        &self,
        _txn: TxnId,
        _name: &QualifiedName,
    ) -> DbResult<Option<SequenceDescriptor>> {
        Ok(None)
    }

    fn list_sequences(
        &self,
        _txn: TxnId,
        _schema_id: SchemaId,
    ) -> DbResult<Vec<SequenceDescriptor>> {
        Ok(Vec::new())
    }

    fn get_statistics(
        &self,
        _txn: TxnId,
        _table_id: RelationId,
    ) -> DbResult<Option<TableStatistics>> {
        Ok(None)
    }

    fn get_view(&self, _txn: TxnId, name: &QualifiedName) -> DbResult<Option<ViewDescriptor>> {
        if name
            .object_name()
            .eq_ignore_ascii_case("__aiondb_matview_sales_snapshot")
        {
            return Ok(Some(sales_snapshot_sidecar()));
        }
        Ok(None)
    }

    fn list_views(&self, _txn: TxnId, schema_id: SchemaId) -> DbResult<Vec<ViewDescriptor>> {
        if schema_id == MOCK_SCHEMA_ID {
            return Ok(vec![sales_snapshot_sidecar()]);
        }
        Ok(Vec::new())
    }
}

fn matview_catalog() -> Arc<dyn CatalogReader> {
    Arc::new(MatviewCatalog)
}

fn txn() -> TxnId {
    TxnId::default()
}

fn sales_snapshot_table() -> TableDescriptor {
    TableDescriptor {
        table_id: RelationId::new(2),
        schema_id: MOCK_SCHEMA_ID,
        name: QualifiedName::unqualified("sales_snapshot"),
        columns: vec![
            ColumnDescriptor {
                column_id: ColumnId::new(1),
                name: "region".to_owned(),
                data_type: DataType::Text,
                raw_type_name: None,
                text_type_modifier: None,
                nullable: false,
                ordinal_position: 1,
                default_value: None,
            },
            ColumnDescriptor {
                column_id: ColumnId::new(2),
                name: "amount".to_owned(),
                data_type: DataType::Int,
                raw_type_name: None,
                text_type_modifier: None,
                nullable: false,
                ordinal_position: 2,
                default_value: None,
            },
        ],
        identity_columns: Vec::new(),
        primary_key: None,
        foreign_keys: Vec::new(),
        check_constraints: Vec::new(),
        shard_config: None,
        owner: None,
    }
}

fn sales_snapshot_sidecar() -> ViewDescriptor {
    ViewDescriptor {
        view_id: RelationId::new(3),
        schema_id: MOCK_SCHEMA_ID,
        name: QualifiedName::unqualified("__aiondb_matview_sales_snapshot"),
        query_sql: "/* aiondb:matview table=sales_snapshot populated=false */ SELECT region, amount FROM sales"
            .to_owned(),
        creation_search_path_schemas: Vec::new(),
        check_option: None,
        columns: vec![
            ColumnDescriptor {
                column_id: ColumnId::new(1),
                name: "region".to_owned(),
                data_type: DataType::Text,
                raw_type_name: None,
                text_type_modifier: None,
                nullable: false,
                ordinal_position: 1,
                default_value: None,
            },
            ColumnDescriptor {
                column_id: ColumnId::new(2),
                name: "amount".to_owned(),
                data_type: DataType::Int,
                raw_type_name: None,
                text_type_modifier: None,
                nullable: false,
                ordinal_position: 2,
                default_value: None,
            },
        ],
        owner: String::new(),
    }
}

/// Extract values from a row of `TypedExpr` literals as strings.
fn extract_values(row: &[TypedExpr]) -> Vec<String> {
    row.iter()
        .map(|expr| match &expr.kind {
            TypedExprKind::Literal(Value::Text(s)) => s.clone(),
            TypedExprKind::Literal(Value::Int(n)) => n.to_string(),
            TypedExprKind::Literal(Value::BigInt(n)) => n.to_string(),
            TypedExprKind::Literal(Value::Boolean(b)) => b.to_string(),
            TypedExprKind::Literal(Value::Double(d)) => d.to_string(),
            TypedExprKind::Literal(Value::Null) => "NULL".to_owned(),
            TypedExprKind::Literal(Value::Array(arr)) => format!(
                "{{{}}}",
                arr.iter()
                    .map(|v| match v {
                        Value::Int(n) => n.to_string(),
                        Value::Text(s) => s.clone(),
                        other => format!("{other:?}"),
                    })
                    .collect::<Vec<_>>()
                    .join(",")
            ),
            other => panic!("expected literal, got {other:?}"),
        })
        .collect()
}

fn unwrap_rows(plan: LogicalPlan) -> (Vec<ResultField>, Vec<Vec<TypedExpr>>) {
    match plan {
        LogicalPlan::ProjectValues {
            output_fields,
            rows,
            ..
        } => (output_fields, rows),
        other => panic!("expected ProjectValues, got {other:?}"),
    }
}
