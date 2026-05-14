use super::*;
use aiondb_catalog::{
    CastContextDescriptor, CastDescriptor, CastMethodDescriptor, CatalogPrivilege,
    DomainConstraintDescriptor, DomainDescriptor, EdgeLabelDescriptor, FunctionPrivilegeTarget,
    NodeLabelDescriptor, PolicyCommandDescriptor, PolicyDescriptor, PolicyKindDescriptor,
    PrivilegeDescriptor, PrivilegeTarget, RoleDescriptor, RuleDescriptor, RuleEventDescriptor,
    UserTypeDescriptor, UserTypeFieldDescriptor,
};
use aiondb_core::DataType;
use aiondb_wal::WalRecord;

#[test]
fn create_schema_happy_path() {
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
    let schema = store
        .get_schema(auto(), &QualifiedName::unqualified("analytics"))
        .unwrap()
        .unwrap();
    assert_eq!(schema.schema_id, schema_id);
    assert_eq!(schema.name, "analytics");
}

#[test]
fn create_schema_duplicate_name_fails() {
    let store = CatalogStore::new();
    store
        .create_schema(
            auto(),
            SchemaDescriptor {
                schema_id: Default::default(),
                name: "dup_schema".to_owned(),
            },
        )
        .unwrap();
    let err = store
        .create_schema(
            auto(),
            SchemaDescriptor {
                schema_id: Default::default(),
                name: "dup_schema".to_owned(),
            },
        )
        .unwrap_err();
    assert_eq!(err.sqlstate(), SqlState::DuplicateSchema);
}

#[test]
fn create_schema_duplicate_case_insensitive_fails() {
    let store = CatalogStore::new();
    store
        .create_schema(
            auto(),
            SchemaDescriptor {
                schema_id: Default::default(),
                name: "MySchema".to_owned(),
            },
        )
        .unwrap();
    let err = store
        .create_schema(
            auto(),
            SchemaDescriptor {
                schema_id: Default::default(),
                name: "myschema".to_owned(),
            },
        )
        .unwrap_err();
    assert_eq!(err.sqlstate(), SqlState::DuplicateSchema);
}

#[test]
fn create_schema_public_already_exists() {
    let store = CatalogStore::new();
    let err = store
        .create_schema(
            auto(),
            SchemaDescriptor {
                schema_id: Default::default(),
                name: "public".to_owned(),
            },
        )
        .unwrap_err();
    assert_eq!(err.sqlstate(), SqlState::DuplicateSchema);
}

#[test]
fn create_tenant_happy_path() {
    let store = CatalogStore::new();
    let tenant = store.create_tenant(auto(), "acme").unwrap();
    let stored = store.get_tenant(auto(), "acme").unwrap().unwrap();
    assert_eq!(stored, tenant);
    let schema = store
        .get_schema(auto(), &QualifiedName::unqualified("tenant_acme"))
        .unwrap()
        .unwrap();
    assert_eq!(schema.schema_id, tenant.schema_id);
}

#[test]
fn create_tenant_writes_catalog_record_to_wal() {
    let (store, wal, dir) = store_with_wal("create_tenant_record");
    let tenant = store.create_tenant(auto(), "acme").unwrap();
    wal.flush().unwrap();

    let tenant_record = read_wal_records(&dir)
        .into_iter()
        .find_map(|record| match record {
            WalRecord::CatalogCreateTenant {
                descriptor_json, ..
            } => Some(crate::catalog_wal::from_json::<TenantDescriptor>(&descriptor_json).unwrap()),
            _ => None,
        });

    assert_eq!(
        tenant_record,
        Some(TenantDescriptor {
            tenant_id: tenant.tenant_id,
            name: "acme".to_owned(),
            schema_id: tenant.schema_id,
        })
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn autocommit_catalog_store_round_trips_through_wal_recovery() {
    let (store, wal, dir) = store_with_wal("autocommit_round_trip");

    let schema_id = store
        .create_schema(
            auto(),
            SchemaDescriptor {
                schema_id: Default::default(),
                name: "analytics".to_owned(),
            },
        )
        .unwrap();

    let mut table = make_table("events");
    table.name = QualifiedName::qualified("analytics", "events");
    let table_id = store.create_table(auto(), table).unwrap();

    store
        .create_role(
            auto(),
            RoleDescriptor {
                name: "app_user".to_owned(),
                login: true,
                superuser: false,
                password_hash: None,
                ..RoleDescriptor::default()
            },
        )
        .unwrap();

    let tenant = store.create_tenant(auto(), "acme").unwrap();
    wal.flush().unwrap();

    let recovered = crate::recovery::recover_catalog_state(&dir).unwrap();
    assert_eq!(recovered.schema_names.get("analytics"), Some(&schema_id));
    assert!(recovered.tables_by_id.contains_key(&table_id));
    assert!(recovered.roles.contains_key("app_user"));
    assert_eq!(recovered.tenants_by_name.get("acme"), Some(&tenant));

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn pg_object_descriptors_round_trip_through_wal_recovery() {
    // Disk-backed regression for the catalog persistence pipeline added
    // for CREATE TYPE / DOMAIN / CAST / POLICY / RULE: write each
    // descriptor, drop the in-memory store, recover from the WAL
    // directory, and assert the rebuilt state contains the same rows.
    let (store, wal, dir) = store_with_wal("pg_object_round_trip");

    store
        .create_domain(
            auto(),
            DomainDescriptor {
                name: "positive_int".to_owned(),
                schema_name: None,
                base_type: "int4".to_owned(),
                not_null: true,
                default_expr: None,
                constraints: vec![DomainConstraintDescriptor {
                    name: "positive_int_check".to_owned(),
                    check_expr: "VALUE > 0".to_owned(),
                }],
                char_length: None,
                owner: Some("aiondb".to_owned()),
            },
        )
        .unwrap();

    store
        .create_user_type(
            auto(),
            UserTypeDescriptor {
                name: "mood".to_owned(),
                schema_name: None,
                oid: 120_001,
                enum_labels: vec!["sad".to_owned(), "ok".to_owned(), "happy".to_owned()],
                composite_fields: Vec::new(),
                owner: Some("aiondb".to_owned()),
            },
        )
        .unwrap();

    store
        .create_user_type(
            auto(),
            UserTypeDescriptor {
                name: "point2d".to_owned(),
                schema_name: None,
                oid: 120_002,
                enum_labels: Vec::new(),
                composite_fields: vec![
                    UserTypeFieldDescriptor {
                        name: "x".to_owned(),
                        data_type: DataType::Int,
                        raw_type_name: Some("int4".to_owned()),
                    },
                    UserTypeFieldDescriptor {
                        name: "y".to_owned(),
                        data_type: DataType::Int,
                        raw_type_name: Some("int4".to_owned()),
                    },
                ],
                owner: None,
            },
        )
        .unwrap();

    store
        .create_cast(
            auto(),
            CastDescriptor {
                oid: 121_001,
                source_type: "int4".to_owned(),
                target_type: "text".to_owned(),
                context: CastContextDescriptor::Assignment,
                method: CastMethodDescriptor::Function {
                    function_name: "int4_to_text_double".to_owned(),
                    function_oid: 122_001,
                },
                owner: Some("aiondb".to_owned()),
            },
        )
        .unwrap();

    store
        .create_policy(
            auto(),
            PolicyDescriptor {
                name: "rls_persist_p".to_owned(),
                table_name: "rls_persist".to_owned(),
                command: PolicyCommandDescriptor::Select,
                kind: PolicyKindDescriptor::Permissive,
                roles: vec!["bob".to_owned()],
                using_expr: Some("owner = current_user".to_owned()),
                with_check_expr: None,
                owner: Some("aiondb".to_owned()),
            },
        )
        .unwrap();

    store
        .create_rule(
            auto(),
            RuleDescriptor {
                name: "block_persist".to_owned(),
                table_name: "rule_persist".to_owned(),
                event: RuleEventDescriptor::Delete,
                is_instead: true,
                action_sql: "NOTHING".to_owned(),
                returning_count: 0,
                owner: Some("aiondb".to_owned()),
            },
        )
        .unwrap();

    wal.flush().unwrap();
    drop(store);

    let recovered = crate::recovery::recover_catalog_state(&dir).unwrap();

    let domain = recovered
        .domains
        .get("positive_int")
        .expect("domain survived recovery");
    assert_eq!(domain.base_type, "int4");
    assert!(domain.not_null);
    assert_eq!(domain.constraints.len(), 1);
    assert_eq!(domain.constraints[0].check_expr, "VALUE > 0");

    let mood = recovered
        .user_types
        .get("mood")
        .expect("enum survived recovery");
    assert_eq!(mood.enum_labels, vec!["sad", "ok", "happy"]);
    assert!(mood.composite_fields.is_empty());

    let point = recovered
        .user_types
        .get("point2d")
        .expect("composite survived recovery");
    assert_eq!(point.composite_fields.len(), 2);
    assert_eq!(point.composite_fields[0].name, "x");
    assert!(point.enum_labels.is_empty());

    let cast_key = ("int4".to_owned(), "text".to_owned());
    let cast = recovered
        .casts
        .get(&cast_key)
        .expect("cast survived recovery");
    match &cast.method {
        CastMethodDescriptor::Function { function_name, .. } => {
            assert_eq!(function_name, "int4_to_text_double");
        }
        other => panic!("unexpected cast method {other:?}"),
    }

    let policy_key = ("rls_persist_p".to_owned(), "rls_persist".to_owned());
    let policy = recovered
        .policies
        .get(&policy_key)
        .expect("policy survived recovery");
    assert_eq!(policy.using_expr.as_deref(), Some("owner = current_user"));
    assert_eq!(policy.roles, vec!["bob"]);

    let rule_key = ("block_persist".to_owned(), "rule_persist".to_owned());
    let rule = recovered
        .rules
        .get(&rule_key)
        .expect("rule survived recovery");
    assert!(rule.is_instead);
    assert_eq!(rule.action_sql, "NOTHING");
    assert!(matches!(rule.event, RuleEventDescriptor::Delete));

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn drop_role_cascades_direct_privilege_dependencies() {
    let store = CatalogStore::new();
    store
        .create_role(
            auto(),
            RoleDescriptor {
                name: "owner_role".to_owned(),
                login: false,
                superuser: false,
                password_hash: None,
                ..RoleDescriptor::default()
            },
        )
        .unwrap();
    store
        .create_role(
            auto(),
            RoleDescriptor {
                name: "member_role".to_owned(),
                login: false,
                superuser: false,
                password_hash: None,
                ..RoleDescriptor::default()
            },
        )
        .unwrap();

    let table = make_table("drop_role_acl_tbl");
    store.create_table(auto(), table).unwrap();

    let privilege = PrivilegeDescriptor {
        role_name: "member_role".to_owned(),
        privilege: CatalogPrivilege::Select,
        target: PrivilegeTarget::Table(QualifiedName::qualified("public", "drop_role_acl_tbl")),
    };
    store.grant_privilege(auto(), privilege.clone()).unwrap();

    store.drop_role(auto(), "member_role").unwrap();
    assert!(store.get_role(auto(), "member_role").unwrap().is_none());
    assert!(store
        .get_privileges(auto(), "owner_role")
        .unwrap()
        .is_empty());
}

#[test]
fn drop_role_cascades_role_membership_references() {
    let store = CatalogStore::new();
    store
        .create_role(
            auto(),
            RoleDescriptor {
                name: "owner_role".to_owned(),
                login: false,
                superuser: false,
                password_hash: None,
                ..RoleDescriptor::default()
            },
        )
        .unwrap();
    store
        .create_role(
            auto(),
            RoleDescriptor {
                name: "member_role".to_owned(),
                login: false,
                superuser: false,
                password_hash: None,
                ..RoleDescriptor::default()
            },
        )
        .unwrap();

    let membership = PrivilegeDescriptor {
        role_name: "member_role".to_owned(),
        privilege: CatalogPrivilege::Usage,
        target: PrivilegeTarget::Role("owner_role".to_owned()),
    };
    store.grant_privilege(auto(), membership.clone()).unwrap();

    store.drop_role(auto(), "owner_role").unwrap();
    assert!(store.get_role(auto(), "owner_role").unwrap().is_none());
    assert_eq!(
        store.get_privileges(auto(), "member_role").unwrap(),
        Vec::new()
    );

    // The inverse role can still be dropped cleanly after membership cleanup.
    store.drop_role(auto(), "member_role").unwrap();
    assert!(store.get_role(auto(), "member_role").unwrap().is_none());
}

#[test]
fn drop_schema_happy_path() {
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

    store.drop_schema(auto(), schema_id).unwrap();

    assert!(store
        .get_schema(auto(), &QualifiedName::unqualified("analytics"))
        .unwrap()
        .is_none());
}

#[test]
fn drop_schema_nonexistent_fails() {
    let store = CatalogStore::new();
    let err = store
        .drop_schema(auto(), aiondb_core::SchemaId::new(9999))
        .unwrap_err();
    assert_eq!(err.sqlstate(), SqlState::InvalidSchemaName);
}

#[test]
fn drop_schema_nonempty_fails() {
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
    let mut table = make_table("events");
    table.name = QualifiedName::qualified("analytics", "events");
    store.create_table(auto(), table).unwrap();

    let err = store.drop_schema(auto(), schema_id).unwrap_err();
    assert_eq!(err.sqlstate(), SqlState::DependentObjectsStillExist);
    assert!(store
        .get_schema(auto(), &QualifiedName::unqualified("analytics"))
        .unwrap()
        .is_some());
}

#[test]
fn drop_tenant_writes_catalog_record_to_wal() {
    let (store, wal, dir) = store_with_wal("drop_tenant_record");
    store.create_tenant(auto(), "acme").unwrap();

    store.drop_tenant(auto(), "acme").unwrap();
    wal.flush().unwrap();

    let records = read_wal_records(&dir);
    assert!(records.iter().any(|record| matches!(
        record,
        WalRecord::CatalogDropTenant { tenant_name, .. } if tenant_name == "acme"
    )));
    assert!(store.get_tenant(auto(), "acme").unwrap().is_none());

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn revoke_modern_function_grant_removes_table_execute_grant() {
    let store = CatalogStore::new();
    store
        .create_role(
            auto(),
            RoleDescriptor {
                name: "reader".to_owned(),
                login: true,
                superuser: false,
                password_hash: None,
                ..RoleDescriptor::default()
            },
        )
        .expect("create role");

    store
        .grant_privilege(
            auto(),
            PrivilegeDescriptor {
                role_name: "reader".to_owned(),
                privilege: CatalogPrivilege::Execute,
                target: PrivilegeTarget::Table(QualifiedName::qualified("public", "function_v1")),
            },
        )
        .expect("table-style grant");
    let grants = store
        .get_privileges(auto(), "reader")
        .expect("list privileges");
    assert_eq!(
        grants,
        vec![PrivilegeDescriptor {
            role_name: "reader".to_owned(),
            privilege: CatalogPrivilege::Execute,
            target: PrivilegeTarget::Function(FunctionPrivilegeTarget {
                name: QualifiedName::qualified("public", "function_v1"),
                arg_types: None,
            }),
        }]
    );

    store
        .revoke_privilege(
            auto(),
            PrivilegeDescriptor {
                role_name: "reader".to_owned(),
                privilege: CatalogPrivilege::Execute,
                target: PrivilegeTarget::Function(FunctionPrivilegeTarget {
                    name: QualifiedName::qualified("public", "function_v1"),
                    arg_types: None,
                }),
            },
        )
        .expect("modern revoke must remove migrated grant");
    assert!(store
        .get_privileges(auto(), "reader")
        .expect("list privileges after revoke")
        .is_empty());
}

#[test]
fn revoke_modern_function_signature_removes_table_execute_grant() {
    let store = CatalogStore::new();
    store
        .create_role(
            auto(),
            RoleDescriptor {
                name: "reader".to_owned(),
                login: true,
                superuser: false,
                password_hash: None,
                ..RoleDescriptor::default()
            },
        )
        .expect("create role");

    store
        .grant_privilege(
            auto(),
            PrivilegeDescriptor {
                role_name: "reader".to_owned(),
                privilege: CatalogPrivilege::Execute,
                target: PrivilegeTarget::Table(QualifiedName::qualified("public", "function_v1")),
            },
        )
        .expect("table-style grant");

    store
        .revoke_privilege(
            auto(),
            PrivilegeDescriptor {
                role_name: "reader".to_owned(),
                privilege: CatalogPrivilege::Execute,
                target: PrivilegeTarget::Function(FunctionPrivilegeTarget {
                    name: QualifiedName::qualified("public", "function_v1"),
                    arg_types: Some(vec![aiondb_core::DataType::Int]),
                }),
            },
        )
        .expect("signature-qualified revoke must remove migrated grant");
    assert!(store
        .get_privileges(auto(), "reader")
        .expect("list privileges after revoke")
        .is_empty());
}

// ===================================================================
// create_table
// ===================================================================

#[test]
fn create_table_happy_path() {
    let store = CatalogStore::new();
    let table_id = store.create_table(auto(), make_table("users")).unwrap();
    assert_ne!(table_id.get(), 0);
    let table = store
        .get_table(auto(), &QualifiedName::qualified("public", "users"))
        .unwrap()
        .unwrap();
    assert_eq!(table.table_id, table_id);
    assert_eq!(table.columns.len(), 2);
    // Columns should have auto-assigned column_ids.
    assert_ne!(table.columns[0].column_id.get(), 0);
    assert_ne!(table.columns[1].column_id.get(), 0);
}

#[test]
fn create_table_duplicate_name_fails() {
    let store = CatalogStore::new();
    store.create_table(auto(), make_table("users")).unwrap();
    let err = store.create_table(auto(), make_table("users")).unwrap_err();
    assert_eq!(err.sqlstate(), SqlState::UniqueViolation);
}

#[test]
fn create_table_duplicate_name_case_insensitive() {
    let store = CatalogStore::new();
    store.create_table(auto(), make_table("Users")).unwrap();
    let err = store.create_table(auto(), make_table("users")).unwrap_err();
    assert_eq!(err.sqlstate(), SqlState::UniqueViolation);
}

#[test]
fn create_table_duplicate_column_names_fails() {
    let store = CatalogStore::new();
    let table = TableDescriptor {
        table_id: Default::default(),
        schema_id: Default::default(),
        name: QualifiedName::qualified("public", "bad_table"),
        columns: vec![
            ColumnDescriptor {
                column_id: Default::default(),
                name: "col".to_owned(),
                data_type: DataType::Int,
                raw_type_name: None,
                text_type_modifier: None,
                nullable: false,
                ordinal_position: 0,
                default_value: None,
            },
            ColumnDescriptor {
                column_id: Default::default(),
                name: "col".to_owned(),
                data_type: DataType::Text,
                raw_type_name: None,
                text_type_modifier: None,
                nullable: true,
                ordinal_position: 0,
                default_value: None,
            },
        ],
        identity_columns: Vec::new(),
        primary_key: None,
        foreign_keys: Vec::new(),
        check_constraints: Vec::new(),
        shard_config: None,
        owner: None,
    };
    let err = store.create_table(auto(), table).unwrap_err();
    assert_eq!(err.sqlstate(), SqlState::UniqueViolation);
}

#[test]
fn create_table_invalid_shard_config_fails() {
    let store = CatalogStore::new();
    let mut table = make_table("bad_shard");
    table.shard_config = Some(CatalogShardConfig {
        shard_key_columns: vec!["missing".to_owned()],
        shard_count: 4,
        virtual_nodes_per_shard: 128,
    });

    let err = store.create_table(auto(), table).unwrap_err();

    assert_eq!(err.sqlstate(), SqlState::UndefinedColumn);
}

#[test]
fn create_table_excessive_shard_count_fails() {
    let store = CatalogStore::new();
    let mut table = make_table("too_many_shards");
    table.shard_config = Some(CatalogShardConfig {
        shard_key_columns: vec!["id".to_owned()],
        shard_count: MAX_CATALOG_SHARD_COUNT + 1,
        virtual_nodes_per_shard: 128,
    });

    let err = store.create_table(auto(), table).unwrap_err();

    assert_eq!(err.sqlstate(), SqlState::InvalidParameterValue);
}

#[test]
fn create_table_excessive_virtual_nodes_fails() {
    let store = CatalogStore::new();
    let mut table = make_table("too_many_vnodes");
    table.shard_config = Some(CatalogShardConfig {
        shard_key_columns: vec!["id".to_owned()],
        shard_count: 4,
        virtual_nodes_per_shard: MAX_CATALOG_VIRTUAL_NODES_PER_SHARD + 1,
    });

    let err = store.create_table(auto(), table).unwrap_err();

    assert_eq!(err.sqlstate(), SqlState::InvalidParameterValue);
}

#[test]
fn create_table_excessive_hash_ring_size_fails() {
    let store = CatalogStore::new();
    let mut table = make_table("too_many_ring_points");
    table.shard_config = Some(CatalogShardConfig {
        shard_key_columns: vec!["id".to_owned()],
        shard_count: MAX_CATALOG_SHARD_COUNT,
        virtual_nodes_per_shard: 128,
    });

    let err = store.create_table(auto(), table).unwrap_err();

    assert_eq!(err.sqlstate(), SqlState::InvalidParameterValue);
    assert!(err
        .to_string()
        .contains(&MAX_CATALOG_HASH_RING_VIRTUAL_NODES.to_string()));
}

#[test]
fn create_table_in_nonexistent_schema_fails() {
    let store = CatalogStore::new();
    let table = TableDescriptor {
        table_id: Default::default(),
        schema_id: Default::default(),
        name: QualifiedName::qualified("nonexistent", "users"),
        columns: vec![],
        identity_columns: Vec::new(),
        primary_key: None,
        foreign_keys: Vec::new(),
        check_constraints: Vec::new(),
        shard_config: None,
        owner: None,
    };
    let err = store.create_table(auto(), table).unwrap_err();
    assert_eq!(err.sqlstate(), SqlState::InvalidCatalogName);
}

#[test]
fn create_table_assigns_ordinal_positions() {
    let store = CatalogStore::new();
    let table_id = store.create_table(auto(), make_table("users")).unwrap();
    let table = store.get_table_by_id(auto(), table_id).unwrap().unwrap();
    assert_eq!(table.columns[0].ordinal_position, 1);
    assert_eq!(table.columns[1].ordinal_position, 2);
}

#[test]
fn create_table_qualifies_name_with_schema() {
    let store = CatalogStore::new();
    // Use unqualified name; should resolve to public.
    let table = TableDescriptor {
        table_id: Default::default(),
        schema_id: Default::default(),
        name: QualifiedName::unqualified("auto_resolved"),
        columns: vec![],
        identity_columns: Vec::new(),
        primary_key: None,
        foreign_keys: Vec::new(),
        check_constraints: Vec::new(),
        shard_config: None,
        owner: None,
    };
    let table_id = store.create_table(auto(), table).unwrap();
    let result = store.get_table_by_id(auto(), table_id).unwrap().unwrap();
    assert_eq!(
        result.name,
        QualifiedName::qualified("public", "auto_resolved")
    );
}

// ===================================================================
// create_index
// ===================================================================

#[test]
fn create_index_happy_path() {
    let store = CatalogStore::new();
    let table_id = store.create_table(auto(), make_table("users")).unwrap();
    let index_id = store
        .create_index(auto(), make_index(table_id, "idx_users_id"))
        .unwrap();
    let index = store.get_index(auto(), index_id).unwrap().unwrap();
    assert_eq!(index.index_id, index_id);
    assert_eq!(index.table_id, table_id);
    assert_eq!(
        index.name,
        QualifiedName::qualified("public", "idx_users_id")
    );
}

#[test]
fn create_index_duplicate_name_fails() {
    let store = CatalogStore::new();
    let table_id = store.create_table(auto(), make_table("users")).unwrap();
    store
        .create_index(auto(), make_index(table_id, "idx1"))
        .unwrap();
    let err = store
        .create_index(auto(), make_index(table_id, "idx1"))
        .unwrap_err();
    assert_eq!(err.sqlstate(), SqlState::UniqueViolation);
}

#[test]
fn create_index_on_nonexistent_table_fails() {
    let store = CatalogStore::new();
    let err = store
        .create_index(auto(), make_index(RelationId::new(9999), "idx"))
        .unwrap_err();
    assert_eq!(err.sqlstate(), SqlState::UndefinedTable);
}

#[test]
fn create_index_inherits_table_schema() {
    let store = CatalogStore::new();
    let table_id = store.create_table(auto(), make_table("users")).unwrap();
    let index_id = store
        .create_index(auto(), make_index(table_id, "idx1"))
        .unwrap();
    let index = store.get_index(auto(), index_id).unwrap().unwrap();
    let table = store.get_table_by_id(auto(), table_id).unwrap().unwrap();
    assert_eq!(index.schema_id, table.schema_id);
}

// ===================================================================
// create_sequence
// ===================================================================

#[test]
fn create_sequence_happy_path() {
    let store = CatalogStore::new();
    let seq_id = store
        .create_sequence(auto(), make_sequence("my_seq"))
        .unwrap();
    let seq = store
        .get_sequence(auto(), &QualifiedName::qualified("public", "my_seq"))
        .unwrap()
        .unwrap();
    assert_eq!(seq.sequence_id, seq_id);
    assert_eq!(seq.start_value, 1);
}

#[test]
fn create_sequence_duplicate_name_fails() {
    let store = CatalogStore::new();
    store
        .create_sequence(auto(), make_sequence("my_seq"))
        .unwrap();
    let err = store
        .create_sequence(auto(), make_sequence("my_seq"))
        .unwrap_err();
    assert_eq!(err.sqlstate(), SqlState::UniqueViolation);
}

#[test]
fn create_sequence_in_nonexistent_schema_fails() {
    let store = CatalogStore::new();
    let seq = SequenceDescriptor {
        sequence_id: Default::default(),
        schema_id: Default::default(),
        name: QualifiedName::qualified("nonexistent", "s"),
        data_type: DataType::BigInt,
        start_value: 1,
        increment_by: 1,
        min_value: 1,
        max_value: i64::MAX,
        cache_size: 1,
        cycle: false,
        owned_by: None,
        owner: None,
    };
    let err = store.create_sequence(auto(), seq).unwrap_err();
    assert_eq!(err.sqlstate(), SqlState::InvalidCatalogName);
}

// ===================================================================
// drop_table
// ===================================================================

#[test]
fn drop_table_happy_path() {
    let store = CatalogStore::new();
    let table_id = store.create_table(auto(), make_table("users")).unwrap();
    store.drop_table(auto(), table_id).unwrap();
    assert!(store
        .get_table(auto(), &QualifiedName::qualified("public", "users"))
        .unwrap()
        .is_none());
    assert!(store.get_table_by_id(auto(), table_id).unwrap().is_none());
}

#[test]
fn drop_table_nonexistent_fails() {
    let store = CatalogStore::new();
    let err = store.drop_table(auto(), RelationId::new(9999)).unwrap_err();
    assert_eq!(err.sqlstate(), SqlState::UndefinedTable);
}

#[test]
fn drop_table_cascades_to_indexes() {
    let store = CatalogStore::new();
    let table_id = store.create_table(auto(), make_table("users")).unwrap();
    let index_id = store
        .create_index(auto(), make_index(table_id, "idx1"))
        .unwrap();
    store.drop_table(auto(), table_id).unwrap();
    assert!(store.get_index(auto(), index_id).unwrap().is_none());
    assert!(store.list_indexes(auto(), table_id).unwrap().is_empty());
}

#[test]
fn drop_table_removes_statistics() {
    let store = CatalogStore::new();
    let table_id = store.create_table(auto(), make_table("users")).unwrap();
    store
        .update_statistics(
            auto(),
            TableStatistics {
                table_id,
                row_count: 10,
                total_bytes: 100,
                dead_row_count: 0,
                last_updated_by: None,
                column_stats: Vec::new(),
            },
        )
        .unwrap();
    store.drop_table(auto(), table_id).unwrap();
    assert!(store.get_statistics(auto(), table_id).unwrap().is_none());
}

#[test]
fn drop_table_allows_recreate_with_same_name() {
    let store = CatalogStore::new();
    let t1 = store.create_table(auto(), make_table("reuse")).unwrap();
    store.drop_table(auto(), t1).unwrap();
    let t2 = store.create_table(auto(), make_table("reuse")).unwrap();
    assert_ne!(t1, t2);
    assert!(store
        .get_table(auto(), &QualifiedName::qualified("public", "reuse"))
        .unwrap()
        .is_some());
}

// ===================================================================
// drop_index
// ===================================================================

#[test]
fn drop_index_happy_path() {
    let store = CatalogStore::new();
    let table_id = store.create_table(auto(), make_table("users")).unwrap();
    let index_id = store
        .create_index(auto(), make_index(table_id, "idx1"))
        .unwrap();
    store.drop_index(auto(), index_id).unwrap();
    assert!(store.get_index(auto(), index_id).unwrap().is_none());
    assert!(store.list_indexes(auto(), table_id).unwrap().is_empty());
}

#[test]
fn drop_index_nonexistent_fails() {
    let store = CatalogStore::new();
    let err = store.drop_index(auto(), IndexId::new(9999)).unwrap_err();
    assert_eq!(err.sqlstate(), SqlState::UndefinedObject);
}

#[test]
fn drop_index_one_of_two_keeps_the_other() {
    let store = CatalogStore::new();
    let table_id = store.create_table(auto(), make_table("users")).unwrap();
    let idx1 = store
        .create_index(auto(), make_index(table_id, "idx1"))
        .unwrap();
    let idx2 = store
        .create_index(auto(), make_index(table_id, "idx2"))
        .unwrap();
    store.drop_index(auto(), idx1).unwrap();
    assert!(store.get_index(auto(), idx1).unwrap().is_none());
    assert!(store.get_index(auto(), idx2).unwrap().is_some());
    assert_eq!(store.list_indexes(auto(), table_id).unwrap().len(), 1);
}

// ===================================================================
// drop_sequence
// ===================================================================

#[test]
fn drop_sequence_happy_path() {
    let store = CatalogStore::new();
    let seq_id = store
        .create_sequence(auto(), make_sequence("my_seq"))
        .unwrap();
    store.drop_sequence(auto(), seq_id).unwrap();
    assert!(store
        .get_sequence(auto(), &QualifiedName::qualified("public", "my_seq"))
        .unwrap()
        .is_none());
}

#[test]
fn drop_sequence_nonexistent_fails() {
    let store = CatalogStore::new();
    let err = store
        .drop_sequence(auto(), SequenceId::new(9999))
        .unwrap_err();
    assert_eq!(err.sqlstate(), SqlState::UndefinedObject);
}

#[test]
fn drop_sequence_allows_recreate_with_same_name() {
    let store = CatalogStore::new();
    let s1 = store
        .create_sequence(auto(), make_sequence("reuse_seq"))
        .unwrap();
    store.drop_sequence(auto(), s1).unwrap();
    let s2 = store
        .create_sequence(auto(), make_sequence("reuse_seq"))
        .unwrap();
    assert_ne!(s1, s2);
}

#[test]
fn drop_table_writes_catalog_drop_table_record_to_wal() {
    let (store, wal, dir) = store_with_wal("drop_table_record");
    let table_id = store.create_table(auto(), make_table("users")).unwrap();

    store.drop_table(auto(), table_id).unwrap();
    wal.flush().unwrap();

    let records = read_wal_records(&dir);
    assert!(records.iter().any(|record| matches!(
        record,
        WalRecord::CatalogDropTable { table_id_raw, .. } if *table_id_raw == table_id.get()
    )));

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn create_index_writes_catalog_index_descriptor_record_to_wal() {
    let (store, wal, dir) = store_with_wal("create_index_record");
    let table_id = store.create_table(auto(), make_table("users")).unwrap();
    let index_id = store
        .create_index(auto(), make_index(table_id, "users_idx"))
        .unwrap();
    wal.flush().unwrap();

    let index_record = read_wal_records(&dir)
        .into_iter()
        .find_map(|record| match record {
            WalRecord::CatalogSetIndexDescriptor {
                descriptor_json, ..
            } => Some(crate::catalog_wal::from_json::<IndexDescriptor>(&descriptor_json).unwrap()),
            _ => None,
        });

    assert!(matches!(
        index_record,
        Some(IndexDescriptor {
            index_id: recorded_id,
            ref name,
            ..
        }) if recorded_id == index_id && name.name == "users_idx"
    ));

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn drop_index_writes_catalog_drop_index_record_to_wal() {
    let (store, wal, dir) = store_with_wal("drop_index_record");
    let table_id = store.create_table(auto(), make_table("users")).unwrap();
    let index_id = store
        .create_index(auto(), make_index(table_id, "users_idx"))
        .unwrap();

    store.drop_index(auto(), index_id).unwrap();
    wal.flush().unwrap();

    let records = read_wal_records(&dir);
    assert!(records.iter().any(|record| matches!(
        record,
        WalRecord::CatalogDropIndex { index_id_raw, .. } if *index_id_raw == index_id.get()
    )));

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn graph_label_operations_write_wal_records() {
    let (store, wal, dir) = store_with_wal("graph_label_records");
    let node_table_id = store.create_table(auto(), make_table("people")).unwrap();
    let edge_table_id = store
        .create_table(auto(), make_edge_table("knows_edges"))
        .unwrap();

    store
        .create_node_label(
            auto(),
            NodeLabelDescriptor {
                label: "person".to_owned(),
                table_id: node_table_id,
            },
        )
        .unwrap();
    store
        .create_edge_label(
            auto(),
            EdgeLabelDescriptor {
                label: "knows".to_owned(),
                table_id: edge_table_id,
                source_label: "person".to_owned(),
                target_label: "person".to_owned(),
                endpoints: None,
            },
        )
        .unwrap();
    store.drop_edge_label(auto(), "knows").unwrap();
    store.drop_node_label(auto(), "person").unwrap();
    wal.flush().unwrap();

    let records = read_wal_records(&dir);
    assert!(records
        .iter()
        .any(|record| matches!(record, WalRecord::CatalogCreateNodeLabel { .. })));
    assert!(records
        .iter()
        .any(|record| matches!(record, WalRecord::CatalogCreateEdgeLabel { .. })));
    assert!(records.iter().any(|record| matches!(
        record,
        WalRecord::CatalogDropNodeLabel { label_name, .. } if label_name == "person"
    )));
    assert!(records.iter().any(|record| matches!(
        record,
        WalRecord::CatalogDropEdgeLabel { label_name, .. } if label_name == "knows"
    )));

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn create_node_label_requires_id_as_first_column() {
    let store = CatalogStore::new();
    let invalid_node_table = TableDescriptor {
        table_id: Default::default(),
        schema_id: Default::default(),
        name: QualifiedName::qualified("public", "events"),
        columns: vec![
            ColumnDescriptor {
                column_id: Default::default(),
                name: "slug".to_owned(),
                data_type: DataType::Text,
                raw_type_name: None,
                text_type_modifier: None,
                nullable: false,
                ordinal_position: 0,
                default_value: None,
            },
            ColumnDescriptor {
                column_id: Default::default(),
                name: "id".to_owned(),
                data_type: DataType::Int,
                raw_type_name: None,
                text_type_modifier: None,
                nullable: false,
                ordinal_position: 0,
                default_value: None,
            },
        ],
        identity_columns: Vec::new(),
        primary_key: None,
        foreign_keys: Vec::new(),
        check_constraints: Vec::new(),
        shard_config: None,
        owner: None,
    };
    let table_id = store.create_table(auto(), invalid_node_table).unwrap();

    let err = store
        .create_node_label(
            auto(),
            NodeLabelDescriptor {
                label: "event".to_owned(),
                table_id,
            },
        )
        .unwrap_err();

    assert_eq!(err.sqlstate(), SqlState::InvalidTableDefinition);
    assert!(format!("{err}").contains("\"id\" as its first column"));
}

#[test]
fn create_edge_label_requires_canonical_endpoint_columns() {
    let store = CatalogStore::new();
    let node_table_id = store.create_table(auto(), make_table("people")).unwrap();
    let invalid_edge_table = TableDescriptor {
        table_id: Default::default(),
        schema_id: Default::default(),
        name: QualifiedName::qualified("public", "works_at_edges"),
        columns: vec![
            ColumnDescriptor {
                column_id: Default::default(),
                name: "person_id".to_owned(),
                data_type: DataType::Int,
                raw_type_name: None,
                text_type_modifier: None,
                nullable: false,
                ordinal_position: 0,
                default_value: None,
            },
            ColumnDescriptor {
                column_id: Default::default(),
                name: "company_id".to_owned(),
                data_type: DataType::Int,
                raw_type_name: None,
                text_type_modifier: None,
                nullable: false,
                ordinal_position: 0,
                default_value: None,
            },
        ],
        identity_columns: Vec::new(),
        primary_key: None,
        foreign_keys: Vec::new(),
        check_constraints: Vec::new(),
        shard_config: None,
        owner: None,
    };
    let edge_table_id = store.create_table(auto(), invalid_edge_table).unwrap();

    store
        .create_node_label(
            auto(),
            NodeLabelDescriptor {
                label: "person".to_owned(),
                table_id: node_table_id,
            },
        )
        .unwrap();

    let err = store
        .create_edge_label(
            auto(),
            EdgeLabelDescriptor {
                label: "works_at".to_owned(),
                table_id: edge_table_id,
                source_label: "person".to_owned(),
                target_label: "person".to_owned(),
                endpoints: None,
            },
        )
        .unwrap_err();

    assert_eq!(err.sqlstate(), SqlState::InvalidTableDefinition);
    assert!(format!("{err}").contains("\"source_id\" and \"target_id\""));
}

#[test]
fn graph_backing_table_cannot_be_reused_by_second_label() {
    let store = CatalogStore::new();
    let node_table_id = store.create_table(auto(), make_table("people")).unwrap();

    store
        .create_node_label(
            auto(),
            NodeLabelDescriptor {
                label: "person".to_owned(),
                table_id: node_table_id,
            },
        )
        .unwrap();

    let err = store
        .create_node_label(
            auto(),
            NodeLabelDescriptor {
                label: "person_v2".to_owned(),
                table_id: node_table_id,
            },
        )
        .unwrap_err();

    assert_eq!(err.sqlstate(), SqlState::DuplicateObject);
    assert!(format!("{err}").contains("already registered as node label"));
}
