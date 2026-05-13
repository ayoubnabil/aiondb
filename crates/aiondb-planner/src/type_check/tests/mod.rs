use std::sync::Arc;

use aiondb_catalog::{
    CatalogReader, ColumnDescriptor, IndexDescriptor, QualifiedName, SchemaDescriptor,
    SequenceDescriptor, TableDescriptor, TableStatistics, ViewDescriptor,
};
use aiondb_core::{
    ColumnId, DataType, DbResult, IndexId, RelationId, SchemaId, SqlState, TxnId, Value,
};
use aiondb_parser::{parse_prepared_statement, Statement};
use aiondb_plan::{LogicalPlan, TypedExprKind};

use super::*;
use crate::binder::{Binder, BoundStatement};
use crate::{PlanRequest, Planner};

mod dml_and_misc;
mod expressions;
mod functions_and_params;
mod planning;

// ---------------------------------------------------------------
// Mock catalog that returns a single "users" table
// ---------------------------------------------------------------

#[derive(Debug)]
struct MockCatalog {
    tables: Vec<TableDescriptor>,
}

impl MockCatalog {
    fn users_table() -> TableDescriptor {
        TableDescriptor {
            table_id: RelationId::new(1),
            schema_id: SchemaId::new(1),
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
                ColumnDescriptor {
                    column_id: ColumnId::new(3),
                    name: "active".to_owned(),
                    data_type: DataType::Boolean,
                    raw_type_name: None,
                    text_type_modifier: None,
                    nullable: false,
                    ordinal_position: 3,
                    default_value: None,
                },
                ColumnDescriptor {
                    column_id: ColumnId::new(4),
                    name: "score".to_owned(),
                    data_type: DataType::BigInt,
                    raw_type_name: None,
                    text_type_modifier: None,
                    nullable: true,
                    ordinal_position: 4,
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

    fn orders_table() -> TableDescriptor {
        TableDescriptor {
            table_id: RelationId::new(2),
            schema_id: SchemaId::new(1),
            name: QualifiedName::unqualified("orders"),
            columns: vec![
                ColumnDescriptor {
                    column_id: ColumnId::new(5),
                    name: "order_id".to_owned(),
                    data_type: DataType::Int,
                    raw_type_name: None,
                    text_type_modifier: None,
                    nullable: false,
                    ordinal_position: 1,
                    default_value: None,
                },
                ColumnDescriptor {
                    column_id: ColumnId::new(6),
                    name: "user_id".to_owned(),
                    data_type: DataType::Int,
                    raw_type_name: None,
                    text_type_modifier: None,
                    nullable: false,
                    ordinal_position: 2,
                    default_value: None,
                },
                ColumnDescriptor {
                    column_id: ColumnId::new(7),
                    name: "amount".to_owned(),
                    data_type: DataType::Double,
                    raw_type_name: None,
                    text_type_modifier: None,
                    nullable: true,
                    ordinal_position: 3,
                    default_value: None,
                },
            ],
            identity_columns: Vec::new(),
            primary_key: Some(vec![ColumnId::new(5)]),
            foreign_keys: Vec::new(),
            check_constraints: Vec::new(),
            shard_config: None,
            owner: None,
        }
    }

    fn vector_docs_table() -> TableDescriptor {
        TableDescriptor {
            table_id: RelationId::new(3),
            schema_id: SchemaId::new(1),
            name: QualifiedName::unqualified("vector_docs"),
            columns: vec![
                ColumnDescriptor {
                    column_id: ColumnId::new(8),
                    name: "id".to_owned(),
                    data_type: DataType::Int,
                    raw_type_name: None,
                    text_type_modifier: None,
                    nullable: false,
                    ordinal_position: 1,
                    default_value: None,
                },
                ColumnDescriptor {
                    column_id: ColumnId::new(9),
                    name: "embedding".to_owned(),
                    data_type: DataType::Vector {
                        dims: 3,
                        element_type: aiondb_core::VectorElementType::Float32,
                    },
                    raw_type_name: None,
                    text_type_modifier: None,
                    nullable: false,
                    ordinal_position: 2,
                    default_value: None,
                },
                ColumnDescriptor {
                    column_id: ColumnId::new(10),
                    name: "payload".to_owned(),
                    data_type: DataType::Jsonb,
                    raw_type_name: None,
                    text_type_modifier: None,
                    nullable: true,
                    ordinal_position: 3,
                    default_value: None,
                },
            ],
            identity_columns: Vec::new(),
            primary_key: Some(vec![ColumnId::new(8)]),
            foreign_keys: Vec::new(),
            check_constraints: Vec::new(),
            shard_config: None,
            owner: None,
        }
    }

    fn with_users() -> Self {
        Self {
            tables: vec![Self::users_table()],
        }
    }

    fn with_users_and_orders() -> Self {
        Self {
            tables: vec![Self::users_table(), Self::orders_table()],
        }
    }

    fn with_vector_docs() -> Self {
        Self {
            tables: vec![Self::vector_docs_table()],
        }
    }
}

impl CatalogReader for MockCatalog {
    fn get_schema(&self, _txn: TxnId, _name: &QualifiedName) -> DbResult<Option<SchemaDescriptor>> {
        Ok(None)
    }

    fn get_table(&self, _txn: TxnId, name: &QualifiedName) -> DbResult<Option<TableDescriptor>> {
        Ok(self
            .tables
            .iter()
            .find(|t| {
                t.name
                    .object_name()
                    .eq_ignore_ascii_case(name.object_name())
            })
            .cloned())
    }

    fn get_table_by_id(
        &self,
        _txn: TxnId,
        table_id: RelationId,
    ) -> DbResult<Option<TableDescriptor>> {
        Ok(self.tables.iter().find(|t| t.table_id == table_id).cloned())
    }

    fn list_tables(&self, _txn: TxnId, _schema_id: SchemaId) -> DbResult<Vec<TableDescriptor>> {
        Ok(self.tables.clone())
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

// ---------------------------------------------------------------
// Helpers: plan SQL through the full pipeline
// ---------------------------------------------------------------

fn plan_sql(sql: &str) -> DbResult<LogicalPlan> {
    let planner = Planner::default();
    let stmt = parse_prepared_statement(sql).expect("parse");
    planner.plan(PlanRequest {
        statement: &stmt,
        txn_id: TxnId::default(),
        default_schema: None,
        current_user: None,
        session_user: None,
        database_name: None,
        datestyle: None,
        timezone: None,
    })
}

fn plan_sql_with_catalog(sql: &str, catalog: Arc<dyn CatalogReader>) -> DbResult<LogicalPlan> {
    let planner = Planner::new(catalog);
    let stmt = parse_prepared_statement(sql).expect("parse");
    planner.plan(PlanRequest {
        statement: &stmt,
        txn_id: TxnId::default(),
        default_schema: None,
        current_user: None,
        session_user: None,
        database_name: None,
        datestyle: None,
        timezone: None,
    })
}

fn type_check_select_sql(sql: &str) -> DbResult<TypedSelect> {
    let stmt = parse_prepared_statement(sql).expect("parse");
    let binder = Binder::new(Arc::new(crate::EmptyCatalog));
    let BoundStatement::Select(bound) = binder.bind(&stmt, TxnId::default(), None).expect("bind")
    else {
        panic!("expected select");
    };
    TypeChecker::new(Arc::new(crate::EmptyCatalog)).type_check_select(&bound)
}

fn type_check_select_sql_with_catalog(
    sql: &str,
    catalog: Arc<dyn CatalogReader>,
) -> DbResult<TypedSelect> {
    let stmt = parse_prepared_statement(sql).expect("parse");
    let binder = Binder::new(Arc::clone(&catalog));
    let BoundStatement::Select(bound) = binder.bind(&stmt, TxnId::default(), None).expect("bind")
    else {
        panic!("expected select");
    };
    TypeChecker::new(catalog).type_check_select(&bound)
}
