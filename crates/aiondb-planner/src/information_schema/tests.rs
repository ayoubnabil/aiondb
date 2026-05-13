#![allow(clippy::used_underscore_binding)]

use std::sync::Arc;

use aiondb_catalog::{
    CatalogReader, ColumnDescriptor, IndexDescriptor, QualifiedName, SchemaDescriptor,
    SequenceDescriptor, TableDescriptor, TableStatistics, ViewDescriptor,
};
use aiondb_core::{
    ColumnId, DataType, DbResult, IndexId, RelationId, SchemaId, SequenceId, TxnId, Value,
};
use aiondb_plan::{LogicalPlan, TypedExprKind};

use super::*;

// ---------------------------------------------------------------
// Mock catalog with a "public" schema and one table
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
        if _name.object_name().eq_ignore_ascii_case("public_seq") {
            return Ok(Some(public_seq()));
        }
        Ok(None)
    }

    fn list_sequences(
        &self,
        _txn: TxnId,
        schema_id: SchemaId,
    ) -> DbResult<Vec<SequenceDescriptor>> {
        if schema_id == MOCK_SCHEMA_ID {
            return Ok(vec![public_seq(), owned_seq()]);
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
        if _name.object_name().eq_ignore_ascii_case("active_users") {
            return Ok(Some(active_users_view()));
        }
        if _name
            .object_name()
            .eq_ignore_ascii_case("__aiondb_matview_active_users_snapshot")
        {
            return Ok(Some(active_users_matview_sidecar()));
        }
        Ok(None)
    }

    fn list_views(&self, _txn: TxnId, _schema_id: SchemaId) -> DbResult<Vec<ViewDescriptor>> {
        if _schema_id == MOCK_SCHEMA_ID {
            return Ok(vec![active_users_view(), active_users_matview_sidecar()]);
        }
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
                default_value: Some("nextval('users_id_seq')".to_owned()),
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
        identity_columns: vec![aiondb_catalog::IdentityColumnDescriptor {
            ordinal_position: 1,
            generation: aiondb_core::IdentityGeneration::ByDefault,
            implicit_serial: false,
        }],
        primary_key: Some(vec![ColumnId::new(1)]),
        foreign_keys: Vec::new(),
        check_constraints: Vec::new(),
        shard_config: None,
        owner: None,
    }
}

fn active_users_view() -> ViewDescriptor {
    ViewDescriptor {
        view_id: RelationId::new(2),
        schema_id: MOCK_SCHEMA_ID,
        name: QualifiedName::unqualified("active_users"),
        query_sql: "SELECT id, name FROM users WHERE id > 0".to_owned(),
        creation_search_path_schemas: Vec::new(),
        check_option: None,
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
    }
}

fn active_users_matview_sidecar() -> ViewDescriptor {
    ViewDescriptor {
        view_id: RelationId::new(3),
        schema_id: MOCK_SCHEMA_ID,
        name: QualifiedName::unqualified("__aiondb_matview_active_users_snapshot"),
        query_sql:
            "/* aiondb:matview table=active_users_snapshot populated=true */ SELECT id, name FROM users WHERE id > 0"
                .to_owned(),
        creation_search_path_schemas: Vec::new(),
        check_option: None,
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
    }
}

fn public_seq() -> SequenceDescriptor {
    SequenceDescriptor {
        sequence_id: SequenceId::new(1),
        schema_id: MOCK_SCHEMA_ID,
        name: QualifiedName::unqualified("public_seq"),
        data_type: DataType::BigInt,
        start_value: 10,
        increment_by: 5,
        min_value: 10,
        max_value: 1000,
        cache_size: 1,
        cycle: false,
        owned_by: None,
        owner: None,
    }
}

fn owned_seq() -> SequenceDescriptor {
    SequenceDescriptor {
        sequence_id: SequenceId::new(2),
        schema_id: MOCK_SCHEMA_ID,
        name: QualifiedName::unqualified("users_id_seq"),
        data_type: DataType::BigInt,
        start_value: 1,
        increment_by: 1,
        min_value: 1,
        max_value: i64::MAX,
        cache_size: 1,
        cycle: false,
        owned_by: Some((RelationId::new(1), ColumnId::new(1))),
        owner: None,
    }
}

fn mock_catalog() -> Arc<dyn CatalogReader> {
    Arc::new(MockCatalog)
}

fn txn() -> TxnId {
    TxnId::default()
}

/// Extract text values from a row of `TypedExpr` literals.
fn extract_text_values(row: &[TypedExpr]) -> Vec<Option<String>> {
    row.iter()
        .map(|expr| match &expr.kind {
            TypedExprKind::Literal(Value::Text(s)) => Some(s.clone()),
            TypedExprKind::Literal(Value::Null) => None,
            TypedExprKind::Literal(Value::Int(n)) => Some(n.to_string()),
            _ => panic!("expected literal, got {:?}", expr.kind),
        })
        .collect()
}

// ---------------------------------------------------------------
// is_information_schema
// ---------------------------------------------------------------

#[test]
fn is_information_schema_lowercase() {
    assert!(is_information_schema("information_schema"));
}

#[test]
fn is_information_schema_mixed_case() {
    assert!(is_information_schema("INFORMATION_SCHEMA"));
    assert!(is_information_schema("Information_Schema"));
}

#[test]
fn is_information_schema_rejects_other() {
    assert!(!is_information_schema("public"));
    assert!(!is_information_schema("info_schema"));
}

// ---------------------------------------------------------------
// output_fields_for
// ---------------------------------------------------------------

#[test]
fn output_fields_for_tables() {
    let fields = output_fields_for("tables").expect("should be Some");
    assert_eq!(fields.len(), 5);
    assert_eq!(fields[0].name, "table_catalog");
    assert_eq!(fields[1].name, "table_schema");
    assert_eq!(fields[2].name, "table_name");
    assert_eq!(fields[3].name, "table_type");
    assert_eq!(fields[4].name, "is_insertable_into");
}

#[test]
fn output_fields_for_columns() {
    let fields = output_fields_for("columns").expect("should be Some");
    assert_eq!(fields.len(), 30);
    assert_eq!(fields[0].name, "table_catalog");
    assert_eq!(fields[3].name, "column_name");
    assert_eq!(fields[4].name, "ordinal_position");
    assert_eq!(fields[4].data_type, DataType::Int);
    assert_eq!(fields[7].name, "data_type");
    assert_eq!(fields[14].name, "identity_maximum");
    assert_eq!(fields[16].name, "identity_cycle");
    assert_eq!(fields[20].name, "numeric_precision");
    assert_eq!(fields[21].name, "numeric_precision_radix");
    assert_eq!(fields[22].name, "numeric_scale");
    assert_eq!(fields[23].name, "datetime_precision");
    assert_eq!(fields[26].name, "udt_catalog");
    assert_eq!(fields[27].name, "udt_schema");
    assert_eq!(fields[28].name, "udt_name");
    assert_eq!(fields[29].name, "dtd_identifier");
}

#[test]
fn output_fields_for_schemata() {
    let fields = output_fields_for("schemata").expect("should be Some");
    assert_eq!(fields.len(), 3);
    assert_eq!(fields[0].name, "catalog_name");
    assert_eq!(fields[1].name, "schema_name");
    assert_eq!(fields[2].name, "schema_owner");
    assert!(fields[2].nullable);
}

#[test]
fn output_fields_for_case_insensitive() {
    assert!(output_fields_for("TABLES").is_some());
    assert!(output_fields_for("Columns").is_some());
    assert!(output_fields_for("SCHEMATA").is_some());
    assert!(output_fields_for("Views").is_some());
    assert!(output_fields_for("FOREIGN_DATA_WRAPPER_OPTIONS").is_some());
}

#[test]
fn output_fields_for_views() {
    let fields = output_fields_for("views").expect("should be Some");
    assert_eq!(fields.len(), 10);
    assert_eq!(fields[0].name, "table_catalog");
    assert_eq!(fields[2].name, "table_name");
    assert_eq!(fields[3].name, "view_definition");
    assert_eq!(fields[5].name, "is_updatable");
}

#[test]
fn output_fields_for_foreign_data_wrapper_options() {
    let fields = output_fields_for("foreign_data_wrapper_options").expect("should be Some");
    assert_eq!(fields.len(), 4);
    assert_eq!(fields[0].name, "foreign_data_wrapper_catalog");
    assert_eq!(fields[1].name, "foreign_data_wrapper_name");
    assert_eq!(fields[2].name, "option_name");
    assert_eq!(fields[3].name, "option_value");
}

#[test]
fn output_fields_for_unknown_returns_none() {
    assert!(output_fields_for("nonexistent").is_none());
}

// ---------------------------------------------------------------
// build_plan - schemata
// ---------------------------------------------------------------

#[test]
fn build_schemata_plan_includes_public_and_information_schema() {
    let catalog = mock_catalog();
    let plan = build_plan(&catalog, txn(), "schemata", None, None)
        .expect("ok")
        .expect("should be Some");

    let (fields, rows) = match plan {
        LogicalPlan::ProjectValues {
            output_fields,
            rows,
            ..
        } => (output_fields, rows),
        other => panic!("expected ProjectValues, got {other:?}"),
    };

    assert_eq!(fields.len(), 3);
    // Should have at least "public" and "information_schema"
    assert!(rows.len() >= 2);

    let schema_names: Vec<_> = rows
        .iter()
        .map(|r| extract_text_values(r)[1].clone().unwrap())
        .collect();
    assert!(schema_names.contains(&"public".to_owned()));
    assert!(schema_names.contains(&"information_schema".to_owned()));
}

// ---------------------------------------------------------------
// build_plan - tables
// ---------------------------------------------------------------

#[test]
fn build_tables_plan_lists_user_tables() {
    let catalog = mock_catalog();
    let plan = build_plan(&catalog, txn(), "tables", None, None)
        .expect("ok")
        .expect("should be Some");

    let rows = match plan {
        LogicalPlan::ProjectValues { rows, .. } => rows,
        other => panic!("expected ProjectValues, got {other:?}"),
    };

    assert_eq!(rows.len(), 2);
    let values = extract_text_values(&rows[0]);
    assert_eq!(values[0].as_deref(), Some("aiondb")); // table_catalog
    assert_eq!(values[1].as_deref(), Some("public")); // table_schema
    assert_eq!(values[2].as_deref(), Some("users")); // table_name
    assert_eq!(values[3].as_deref(), Some("BASE TABLE")); // table_type

    let view_values = extract_text_values(&rows[1]);
    assert_eq!(view_values[2].as_deref(), Some("active_users"));
    assert_eq!(view_values[3].as_deref(), Some("VIEW"));
}

// ---------------------------------------------------------------
// build_plan - columns
// ---------------------------------------------------------------

#[test]
fn build_columns_plan_lists_table_columns() {
    let catalog = mock_catalog();
    let plan = build_plan(&catalog, txn(), "columns", None, None)
        .expect("ok")
        .expect("should be Some");

    let rows = match plan {
        LogicalPlan::ProjectValues { rows, .. } => rows,
        other => panic!("expected ProjectValues, got {other:?}"),
    };

    // "users" table has 2 columns and the mock view has 2 projected columns
    assert_eq!(rows.len(), 4);

    let id_values = extract_text_values(&rows[0]);
    assert_eq!(id_values[0].as_deref(), Some("aiondb")); // table_catalog
    assert_eq!(id_values[2].as_deref(), Some("users")); // table_name
    assert_eq!(id_values[3].as_deref(), Some("id")); // column_name
    assert_eq!(id_values[4].as_deref(), Some("1")); // ordinal_position
    assert_eq!(id_values[6].as_deref(), Some("NO")); // is_nullable
    assert_eq!(id_values[7].as_deref(), Some("integer")); // data_type
    assert_eq!(id_values[10].as_deref(), Some("YES")); // is_identity
    assert_eq!(id_values[11].as_deref(), Some("BY DEFAULT")); // identity_generation
    assert_eq!(id_values[12].as_deref(), Some("1")); // identity_start
    assert_eq!(id_values[13].as_deref(), Some("1")); // identity_increment
    assert_eq!(id_values[16].as_deref(), Some("NO")); // identity_cycle

    let name_values = extract_text_values(&rows[1]);
    assert_eq!(name_values[3].as_deref(), Some("name")); // column_name
    assert_eq!(name_values[4].as_deref(), Some("2")); // ordinal_position
    assert_eq!(name_values[6].as_deref(), Some("YES")); // is_nullable
    assert_eq!(name_values[7].as_deref(), Some("text")); // data_type
    assert_eq!(name_values[10].as_deref(), Some("NO")); // is_identity
    assert_eq!(name_values[16].as_deref(), Some("NO")); // identity_cycle

    let view_rows: Vec<_> = rows
        .iter()
        .filter(|row| extract_text_values(row)[2].as_deref() == Some("active_users"))
        .collect();
    assert_eq!(view_rows.len(), 2);
    assert_eq!(
        extract_text_values(view_rows[0])[17].as_deref(),
        Some("YES")
    );
}

#[test]
fn build_views_plan_lists_views() {
    let catalog = mock_catalog();
    let plan = build_plan(&catalog, txn(), "views", None, None)
        .expect("ok")
        .expect("should be Some");

    let rows = match plan {
        LogicalPlan::ProjectValues { rows, .. } => rows,
        other => panic!("expected ProjectValues, got {other:?}"),
    };

    assert_eq!(rows.len(), 1);
    let values = extract_text_values(&rows[0]);
    assert_eq!(values[2].as_deref(), Some("active_users"));
    assert_eq!(values[5].as_deref(), Some("YES"));
    assert_eq!(values[6].as_deref(), Some("YES"));
}

#[test]
fn build_views_plan_omits_matview_sidecars() {
    let catalog = mock_catalog();
    let plan = build_plan(&catalog, txn(), "views", None, None)
        .expect("ok")
        .expect("should be Some");

    let rows = match plan {
        LogicalPlan::ProjectValues { rows, .. } => rows,
        other => panic!("expected ProjectValues, got {other:?}"),
    };

    let view_names: Vec<_> = rows
        .iter()
        .map(|row| extract_text_values(row)[2].clone())
        .collect();
    assert!(!view_names
        .iter()
        .any(|name| { name.as_deref() == Some("__aiondb_matview_active_users_snapshot") }));
}

#[test]
fn build_sequences_plan_lists_only_user_visible_sequences() {
    let catalog = mock_catalog();
    let plan = build_plan(&catalog, txn(), "sequences", None, None)
        .expect("ok")
        .expect("should be Some");

    let rows = match plan {
        LogicalPlan::ProjectValues { rows, .. } => rows,
        other => panic!("expected ProjectValues, got {other:?}"),
    };

    assert_eq!(rows.len(), 1);
    let values = extract_text_values(&rows[0]);
    assert_eq!(values[2].as_deref(), Some("public_seq"));
    assert_eq!(values[3].as_deref(), Some("bigint"));
    assert_eq!(values[7].as_deref(), Some("10"));
    assert_eq!(values[10].as_deref(), Some("5"));
}

#[test]
fn build_select_plan_supports_order_by_for_information_schema() {
    let catalog = mock_catalog();
    let statements = aiondb_parser::parse_sql(
        "SELECT table_name, column_name \
         FROM information_schema.columns \
         WHERE table_name = 'users' \
         ORDER BY ordinal_position DESC",
    )
    .expect("parse");
    let Statement::Select(select) = statements.into_iter().next().expect("statement") else {
        panic!("expected SELECT");
    };

    let plan = build_select_plan(&catalog, txn(), &select, None, None)
        .expect("ok")
        .expect("should be Some");

    match plan {
        LogicalPlan::ProjectValues { rows, order_by, .. } => {
            assert!(order_by.is_empty(), "rows are pre-sorted in-memory");
            let ordered_columns: Vec<_> = rows
                .iter()
                .map(|row| extract_text_values(row)[1].clone().unwrap())
                .collect();
            assert_eq!(ordered_columns, vec!["name".to_owned(), "id".to_owned()]);
        }
        other => panic!("expected ProjectValues, got {other:?}"),
    }
}

#[test]
fn build_select_plan_supports_count_aggregate_for_information_schema() {
    let catalog = mock_catalog();
    let statements =
        aiondb_parser::parse_sql("SELECT count(*) > 0 AS ok FROM information_schema.tables")
            .expect("parse");
    let Statement::Select(select) = statements.into_iter().next().expect("statement") else {
        panic!("expected SELECT");
    };

    let plan = build_select_plan(&catalog, txn(), &select, None, None)
        .expect("ok")
        .expect("should be Some");

    match plan {
        LogicalPlan::ProjectValues { rows, .. } => {
            assert_eq!(rows.len(), 1);
            assert!(matches!(
                rows[0][0].kind,
                TypedExprKind::Literal(Value::Boolean(true))
            ));
        }
        other => panic!("expected ProjectValues, got {other:?}"),
    }
}

#[test]
fn build_select_plan_supports_literal_projection_for_information_schema() {
    let catalog = mock_catalog();
    let statements = aiondb_parser::parse_sql(
        "SELECT table_name, 'marker'::text AS tag \
         FROM information_schema.tables \
         WHERE table_name = 'users'",
    )
    .expect("parse");
    let Statement::Select(select) = statements.into_iter().next().expect("statement") else {
        panic!("expected SELECT");
    };

    let plan = build_select_plan(&catalog, txn(), &select, None, None)
        .expect("ok")
        .expect("should be Some");

    match plan {
        LogicalPlan::ProjectValues { rows, .. } => {
            assert_eq!(rows.len(), 1);
            let values = extract_text_values(&rows[0]);
            assert_eq!(values[0].as_deref(), Some("users"));
            assert_eq!(values[1].as_deref(), Some("marker"));
        }
        other => panic!("expected ProjectValues, got {other:?}"),
    }
}

#[test]
fn build_select_plan_falls_back_for_unsupported_function_wrappers_in_information_schema() {
    let catalog = mock_catalog();
    let statements = aiondb_parser::parse_sql(
        "SELECT lower(column_name) AS name \
         FROM information_schema.columns \
         WHERE coalesce(ordinal_position, 1) = 1 \
         ORDER BY upper(column_name)",
    )
    .expect("parse");
    let Statement::Select(select) = statements.into_iter().next().expect("statement") else {
        panic!("expected SELECT");
    };

    let plan = build_select_plan(&catalog, txn(), &select, None, None)
        .expect("fast-path should defer instead of erroring");
    assert!(
        plan.is_none(),
        "unsupported information_schema wrappers should fall back to the general binder"
    );
}

// ---------------------------------------------------------------
// build_plan - unknown table returns None
// ---------------------------------------------------------------

#[test]
fn build_plan_unknown_table_returns_none() {
    let catalog = mock_catalog();
    let result = build_plan(&catalog, txn(), "nonexistent", None, None).expect("ok");
    assert!(result.is_none());
}

// ---------------------------------------------------------------
// build_plan - case insensitive table names
// ---------------------------------------------------------------

#[test]
fn build_plan_case_insensitive() {
    let catalog = mock_catalog();
    assert!(build_plan(&catalog, txn(), "TABLES", None, None)
        .expect("ok")
        .is_some());
    assert!(build_plan(&catalog, txn(), "Columns", None, None)
        .expect("ok")
        .is_some());
    assert!(build_plan(&catalog, txn(), "SCHEMATA", None, None)
        .expect("ok")
        .is_some());
    assert!(
        build_plan(&catalog, txn(), "FOREIGN_DATA_WRAPPER_OPTIONS", None, None)
            .expect("ok")
            .is_some()
    );
}

// ---------------------------------------------------------------
// Empty catalog produces no table/column rows
// ---------------------------------------------------------------

#[test]
fn build_tables_plan_empty_catalog() {
    let catalog: Arc<dyn CatalogReader> = Arc::new(crate::EmptyCatalog);
    let plan = build_plan(&catalog, txn(), "tables", None, None)
        .expect("ok")
        .expect("should be Some");

    let rows = match plan {
        LogicalPlan::ProjectValues { rows, .. } => rows,
        other => panic!("expected ProjectValues, got {other:?}"),
    };
    assert!(rows.is_empty());
}

#[test]
fn build_columns_plan_empty_catalog() {
    let catalog: Arc<dyn CatalogReader> = Arc::new(crate::EmptyCatalog);
    let plan = build_plan(&catalog, txn(), "columns", None, None)
        .expect("ok")
        .expect("should be Some");

    let rows = match plan {
        LogicalPlan::ProjectValues { rows, .. } => rows,
        other => panic!("expected ProjectValues, got {other:?}"),
    };
    assert!(rows.is_empty());
}

#[test]
fn build_schemata_plan_empty_catalog_has_information_schema() {
    let catalog: Arc<dyn CatalogReader> = Arc::new(crate::EmptyCatalog);
    let plan = build_plan(&catalog, txn(), "schemata", None, None)
        .expect("ok")
        .expect("should be Some");

    let rows = match plan {
        LogicalPlan::ProjectValues { rows, .. } => rows,
        other => panic!("expected ProjectValues, got {other:?}"),
    };
    // Even with no "public" schema, information_schema should appear
    assert_eq!(rows.len(), 1);
    let values = extract_text_values(&rows[0]);
    assert_eq!(values[1].as_deref(), Some("information_schema"));
}
