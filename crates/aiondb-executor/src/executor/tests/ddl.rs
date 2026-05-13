use super::*;

#[test]
fn create_table_returns_command_result() {
    let (executor, _, _) = make_executor();
    let ctx = default_context();

    let plan = PhysicalPlan::CreateTable {
        relation_name: "users".to_string(),
        columns: vec![
            ColumnPlan {
                name: "id".to_string(),
                data_type: DataType::Int,
                raw_type_name: None,
                text_type_modifier: None,
                nullable: false,
                has_default: false,
            },
            ColumnPlan {
                name: "name".to_string(),
                data_type: DataType::Text,
                raw_type_name: None,
                text_type_modifier: None,
                nullable: true,
                has_default: false,
            },
        ],
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

    let result = executor.execute(&plan, &ctx).unwrap();
    assert_eq!(result, ExecutionResult::command("CREATE TABLE"));
}

#[test]
fn create_table_registers_in_catalog() {
    let (executor, catalog, _) = make_executor();
    let ctx = default_context();

    let plan = PhysicalPlan::CreateTable {
        relation_name: "products".to_string(),
        columns: vec![ColumnPlan {
            name: "id".to_string(),
            data_type: DataType::Int,
            raw_type_name: None,
            text_type_modifier: None,
            nullable: false,
            has_default: false,
        }],
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

    executor.execute(&plan, &ctx).unwrap();

    let table = catalog
        .get_table(TxnId::default(), &QualifiedName::unqualified("products"))
        .unwrap();
    assert!(table.is_some());
    let table = table.unwrap();
    assert_eq!(table.columns.len(), 1);
    assert_eq!(table.columns[0].name, "id");
}

#[test]
fn create_table_creates_storage() {
    let (executor, _, storage) = make_executor();
    let ctx = default_context();

    let plan = PhysicalPlan::CreateTable {
        relation_name: "items".to_string(),
        columns: vec![ColumnPlan {
            name: "id".to_string(),
            data_type: DataType::Int,
            raw_type_name: None,
            text_type_modifier: None,
            nullable: false,
            has_default: false,
        }],
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

    executor.execute(&plan, &ctx).unwrap();

    // The storage should have an entry for the new table
    let tables = storage.tables.lock().unwrap();
    assert!(!tables.is_empty(), "storage should have the new table");
}

#[test]
fn create_edge_label_existing_table_creates_graph_indexes_and_registers_adjacency() {
    let (executor, catalog, storage) = make_executor();
    let ctx = default_context();

    let table_id = create_test_table(
        &executor,
        &catalog,
        "knows_edges_existing",
        vec![
            ColumnPlan {
                name: "source_id".to_string(),
                data_type: DataType::BigInt,
                raw_type_name: None,
                text_type_modifier: None,
                nullable: true,
                has_default: false,
            },
            ColumnPlan {
                name: "target_id".to_string(),
                data_type: DataType::BigInt,
                raw_type_name: None,
                text_type_modifier: None,
                nullable: true,
                has_default: false,
            },
        ],
    );

    let plan = PhysicalPlan::CreateEdgeLabel {
        label: "knows_existing".to_string(),
        table_id,
        source_label: "person".to_string(),
        target_label: "person".to_string(),
        endpoints: None,
    };

    let result = executor.execute(&plan, &ctx).unwrap();
    assert_eq!(result, ExecutionResult::command("CREATE EDGE LABEL"));

    let indexes = catalog.list_indexes(TxnId::default(), table_id).unwrap();
    let index_names: Vec<&str> = indexes.iter().map(|idx| idx.name.object_name()).collect();
    assert!(
        index_names.contains(&"idx_knows_edges_existing_source_id"),
        "missing source_id graph index"
    );
    assert!(
        index_names.contains(&"idx_knows_edges_existing_target_id"),
        "missing target_id graph index"
    );

    let created_index_ids = storage.created_index_ids.lock().unwrap();
    assert_eq!(created_index_ids.len(), 2);
    drop(created_index_ids);

    let registered_edge_tables = storage.registered_edge_tables.lock().unwrap();
    assert_eq!(registered_edge_tables.as_slice(), &[(table_id, 0, 1)]);
}

#[test]
fn drop_table_returns_command_result() {
    let (executor, catalog, _) = make_executor();
    let ctx = default_context();

    let table_id = create_test_table(
        &executor,
        &catalog,
        "to_drop",
        vec![ColumnPlan {
            name: "id".to_string(),
            data_type: DataType::Int,
            raw_type_name: None,
            text_type_modifier: None,
            nullable: false,
            has_default: false,
        }],
    );

    let plan = PhysicalPlan::DropTable {
        table_id,
        cascade: false,
    };
    let result = executor.execute(&plan, &ctx).unwrap();

    assert_eq!(result, ExecutionResult::command("DROP TABLE"));
}

#[test]
fn drop_table_removes_from_catalog() {
    let (executor, catalog, _) = make_executor();
    let ctx = default_context();

    let table_id = create_test_table(
        &executor,
        &catalog,
        "dropme",
        vec![ColumnPlan {
            name: "id".to_string(),
            data_type: DataType::Int,
            raw_type_name: None,
            text_type_modifier: None,
            nullable: false,
            has_default: false,
        }],
    );

    let plan = PhysicalPlan::DropTable {
        table_id,
        cascade: false,
    };
    executor.execute(&plan, &ctx).unwrap();

    let table = catalog.get_table_by_id(TxnId::default(), table_id).unwrap();
    assert!(table.is_none(), "table should be removed after drop");
}

#[test]
fn drop_table_removes_storage() {
    let (executor, catalog, storage) = make_executor();
    let ctx = default_context();

    let table_id = create_test_table(
        &executor,
        &catalog,
        "drop_storage",
        vec![ColumnPlan {
            name: "id".to_string(),
            data_type: DataType::Int,
            raw_type_name: None,
            text_type_modifier: None,
            nullable: false,
            has_default: false,
        }],
    );

    let plan = PhysicalPlan::DropTable {
        table_id,
        cascade: false,
    };
    executor.execute(&plan, &ctx).unwrap();

    let tables = storage.tables.lock().unwrap();
    assert!(
        !tables.contains_key(&table_id),
        "storage should not have the dropped table"
    );
}

// =========================================================================
