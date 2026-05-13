use super::*;

#[test]
fn creates_and_drops_sequence() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let results = engine
        .execute_sql(
            &session,
            "CREATE SEQUENCE user_ids; \
             DROP SEQUENCE user_ids; \
             CREATE SEQUENCE user_ids",
        )
        .expect("execute");

    assert_eq!(
        results,
        vec![
            StatementResult::Command {
                tag: "CREATE SEQUENCE".to_owned(),
                rows_affected: 0,
            },
            StatementResult::Command {
                tag: "DROP SEQUENCE".to_owned(),
                rows_affected: 0,
            },
            StatementResult::Command {
                tag: "CREATE SEQUENCE".to_owned(),
                rows_affected: 0,
            },
        ]
    );
}

#[test]
fn selects_nextval_from_sequence() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(&session, "CREATE SEQUENCE user_ids")
        .expect("create sequence");

    let first = engine
        .execute_sql(&session, "SELECT nextval('user_ids') AS id")
        .expect("first nextval");
    let second = engine
        .execute_sql(&session, "SELECT nextval('user_ids') AS id")
        .expect("second nextval");

    assert_eq!(
        first,
        vec![StatementResult::Query {
            columns: vec![ResultColumn {
                name: "id".to_owned(),
                data_type: aiondb_core::DataType::BigInt,
                text_type_modifier: None,
                nullable: false,
            }],
            rows: vec![aiondb_core::Row::new(vec![aiondb_core::Value::BigInt(1)])],
        }]
    );
    assert_eq!(
        second,
        vec![StatementResult::Query {
            columns: vec![ResultColumn {
                name: "id".to_owned(),
                data_type: aiondb_core::DataType::BigInt,
                text_type_modifier: None,
                nullable: false,
            }],
            rows: vec![aiondb_core::Row::new(vec![aiondb_core::Value::BigInt(2)])],
        }]
    );
}

#[test]
fn nextval_resolves_default_user_search_path_schema() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE SCHEMA alice; \
             CREATE SEQUENCE alice.user_ids",
        )
        .expect("create user schema sequence");

    let first = engine
        .execute_sql(&session, "SELECT nextval('user_ids') AS id")
        .expect("nextval via default $user search_path");
    let second = engine
        .execute_sql(&session, "SELECT nextval('user_ids') AS id")
        .expect("second nextval via default $user search_path");

    assert_eq!(
        first,
        vec![StatementResult::Query {
            columns: vec![ResultColumn {
                name: "id".to_owned(),
                data_type: aiondb_core::DataType::BigInt,
                text_type_modifier: None,
                nullable: false,
            }],
            rows: vec![aiondb_core::Row::new(vec![aiondb_core::Value::BigInt(1)])],
        }]
    );
    assert_eq!(
        second,
        vec![StatementResult::Query {
            columns: vec![ResultColumn {
                name: "id".to_owned(),
                data_type: aiondb_core::DataType::BigInt,
                text_type_modifier: None,
                nullable: false,
            }],
            rows: vec![aiondb_core::Row::new(vec![aiondb_core::Value::BigInt(2)])],
        }]
    );
}

#[test]
fn insert_default_without_catalog_default_uses_null() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE TABLE users (id INT, name TEXT); \
             INSERT INTO users VALUES (DEFAULT, 'alice')",
        )
        .expect("insert default");

    let results = engine
        .execute_sql(&session, "SELECT id, name FROM users")
        .expect("select");
    assert_eq!(
        results,
        vec![StatementResult::Query {
            columns: vec![
                ResultColumn {
                    name: "id".to_owned(),
                    data_type: aiondb_core::DataType::Int,
                    text_type_modifier: None,
                    nullable: true,
                },
                ResultColumn {
                    name: "name".to_owned(),
                    data_type: aiondb_core::DataType::Text,
                    text_type_modifier: None,
                    nullable: true,
                },
            ],
            rows: vec![aiondb_core::Row::new(vec![
                aiondb_core::Value::Null,
                aiondb_core::Value::Text("alice".to_owned()),
            ])],
        }]
    );
}

#[test]
fn insert_null_literal_into_not_null_column_is_lenient() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(&session, "CREATE TABLE users (id INT NOT NULL, name TEXT)")
        .expect("create table");

    let err = engine
        .execute_sql(&session, "INSERT INTO users VALUES (NULL, 'alice')")
        .expect_err("NOT NULL column must reject NULL insert");
    assert_eq!(err.sqlstate(), aiondb_core::SqlState::NotNullViolation);
}

#[test]
fn insert_default_without_catalog_default_into_not_null_column_is_lenient() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(&session, "CREATE TABLE users (id INT NOT NULL, name TEXT)")
        .expect("create table");

    let err = engine
        .execute_sql(&session, "INSERT INTO users VALUES (DEFAULT, 'alice')")
        .expect_err("NOT NULL column must reject NULL insert");
    assert_eq!(err.sqlstate(), aiondb_core::SqlState::NotNullViolation);
}

#[test]
fn insert_default_uses_catalog_nextval_expression() {
    let catalog = Arc::new(aiondb_catalog_store::CatalogStore::default());
    let storage = Arc::new(aiondb_storage_engine::InMemoryStorage::new_without_wal());
    let txn = aiondb_core::TxnId::default();

    let table_id = aiondb_catalog::CatalogWriter::create_table(
        &*catalog,
        txn,
        aiondb_catalog::TableDescriptor {
            table_id: aiondb_core::RelationId::default(),
            schema_id: aiondb_core::SchemaId::default(),
            name: aiondb_catalog::QualifiedName::unqualified("users"),
            columns: vec![
                aiondb_catalog::ColumnDescriptor {
                    column_id: aiondb_core::ColumnId::default(),
                    name: "id".to_owned(),
                    data_type: aiondb_core::DataType::BigInt,
                    raw_type_name: None,
                    text_type_modifier: None,
                    nullable: false,
                    ordinal_position: 1,
                    default_value: Some("nextval('user_ids')".to_owned()),
                },
                aiondb_catalog::ColumnDescriptor {
                    column_id: aiondb_core::ColumnId::default(),
                    name: "name".to_owned(),
                    data_type: aiondb_core::DataType::Text,
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
        },
    )
    .expect("create catalog table");
    let table = aiondb_catalog::CatalogReader::get_table_by_id(&*catalog, txn, table_id)
        .expect("get table")
        .expect("table exists");
    aiondb_storage_api::StorageDDL::create_table_storage(
        &*storage,
        txn,
        &aiondb_schema_bridge::to_table_storage_descriptor(&table).unwrap(),
    )
    .expect("create storage table");

    let engine = build_engine_with_store(catalog, storage);
    let (session, _) = engine.startup(startup_params()).expect("startup");
    engine
        .execute_sql(
            &session,
            "CREATE SEQUENCE user_ids; \
             INSERT INTO users VALUES (DEFAULT, 'alice'), (DEFAULT, 'bob')",
        )
        .expect("insert using catalog default");

    let results = engine
        .execute_sql(&session, "SELECT id, name FROM users ORDER BY id")
        .expect("select");
    assert_eq!(
        results,
        vec![StatementResult::Query {
            columns: vec![
                ResultColumn {
                    name: "id".to_owned(),
                    data_type: aiondb_core::DataType::BigInt,
                    text_type_modifier: None,
                    nullable: false,
                },
                ResultColumn {
                    name: "name".to_owned(),
                    data_type: aiondb_core::DataType::Text,
                    text_type_modifier: None,
                    nullable: false,
                },
            ],
            rows: vec![
                aiondb_core::Row::new(vec![
                    aiondb_core::Value::BigInt(1),
                    aiondb_core::Value::Text("alice".to_owned()),
                ]),
                aiondb_core::Row::new(vec![
                    aiondb_core::Value::BigInt(2),
                    aiondb_core::Value::Text("bob".to_owned()),
                ]),
            ],
        }]
    );
}

#[test]
fn default_is_rejected_outside_insert_values() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let error = engine
        .execute_sql(&session, "SELECT DEFAULT")
        .expect_err("default outside insert");
    assert_eq!(error.sqlstate(), aiondb_core::SqlState::SyntaxError);
}

#[test]
fn inserts_rows_with_nextval_sequence_values() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE SEQUENCE user_ids; \
             CREATE TABLE users (id BIGINT, name TEXT); \
             INSERT INTO users VALUES (nextval('user_ids'), 'alice'), (nextval('user_ids'), 'bob')",
        )
        .expect("seed with nextval");

    let results = engine
        .execute_sql(&session, "SELECT id, name FROM users ORDER BY id")
        .expect("select");
    assert_eq!(
        results,
        vec![StatementResult::Query {
            columns: vec![
                ResultColumn {
                    name: "id".to_owned(),
                    data_type: aiondb_core::DataType::BigInt,
                    text_type_modifier: None,
                    nullable: true,
                },
                ResultColumn {
                    name: "name".to_owned(),
                    data_type: aiondb_core::DataType::Text,
                    text_type_modifier: None,
                    nullable: true,
                },
            ],
            rows: vec![
                aiondb_core::Row::new(vec![
                    aiondb_core::Value::BigInt(1),
                    aiondb_core::Value::Text("alice".to_owned()),
                ]),
                aiondb_core::Row::new(vec![
                    aiondb_core::Value::BigInt(2),
                    aiondb_core::Value::Text("bob".to_owned()),
                ]),
            ],
        }]
    );
}

#[test]
fn executes_prepared_insert_with_nextval_and_parameter() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE SEQUENCE user_ids; CREATE TABLE users (id BIGINT, name TEXT)",
        )
        .expect("setup");

    let desc = engine
        .prepare(
            &session,
            "ins_nextval".to_owned(),
            "INSERT INTO users VALUES (nextval('user_ids'), $1)".to_owned(),
        )
        .expect("prepare");
    assert_eq!(desc.param_types, vec![aiondb_core::DataType::Text]);

    engine
        .bind(
            &session,
            "p1".to_owned(),
            "ins_nextval".to_owned(),
            vec![aiondb_core::Value::Text("alice".to_owned())],
        )
        .expect("bind");
    let batch = engine
        .execute_portal(&session, "p1", 0)
        .expect("execute portal");
    assert_eq!(batch.tag, "INSERT");

    let results = engine
        .execute_sql(&session, "SELECT id, name FROM users")
        .expect("select");
    assert_eq!(
        results,
        vec![StatementResult::Query {
            columns: vec![
                ResultColumn {
                    name: "id".to_owned(),
                    data_type: aiondb_core::DataType::BigInt,
                    text_type_modifier: None,
                    nullable: true,
                },
                ResultColumn {
                    name: "name".to_owned(),
                    data_type: aiondb_core::DataType::Text,
                    text_type_modifier: None,
                    nullable: true,
                },
            ],
            rows: vec![aiondb_core::Row::new(vec![
                aiondb_core::Value::BigInt(1),
                aiondb_core::Value::Text("alice".to_owned()),
            ])],
        }]
    );
}

#[test]
fn inserts_rows_from_select_with_order_and_defaults() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let results = engine
        .execute_sql(
            &session,
            "CREATE SEQUENCE user_ids; \
             CREATE TABLE src (name TEXT); \
             INSERT INTO src VALUES ('alice'), ('bob'); \
             CREATE TABLE users (id BIGINT NOT NULL DEFAULT nextval('user_ids'), name TEXT NOT NULL); \
             INSERT INTO users (name) SELECT name FROM src ORDER BY name DESC; \
             SELECT id, name FROM users ORDER BY id ASC",
        )
        .expect("execute");

    assert_eq!(
        results,
        vec![
            StatementResult::Command {
                tag: "CREATE SEQUENCE".to_owned(),
                rows_affected: 0,
            },
            StatementResult::Command {
                tag: "CREATE TABLE".to_owned(),
                rows_affected: 0,
            },
            StatementResult::Command {
                tag: "INSERT".to_owned(),
                rows_affected: 2,
            },
            StatementResult::Command {
                tag: "CREATE TABLE".to_owned(),
                rows_affected: 0,
            },
            StatementResult::Command {
                tag: "INSERT".to_owned(),
                rows_affected: 2,
            },
            StatementResult::Query {
                columns: vec![
                    ResultColumn {
                        name: "id".to_owned(),
                        data_type: aiondb_core::DataType::BigInt,
                        text_type_modifier: None,
                        nullable: false,
                    },
                    ResultColumn {
                        name: "name".to_owned(),
                        data_type: aiondb_core::DataType::Text,
                        text_type_modifier: None,
                        nullable: false,
                    },
                ],
                rows: vec![
                    aiondb_core::Row::new(vec![
                        aiondb_core::Value::BigInt(1),
                        aiondb_core::Value::Text("bob".to_owned()),
                    ]),
                    aiondb_core::Row::new(vec![
                        aiondb_core::Value::BigInt(2),
                        aiondb_core::Value::Text("alice".to_owned()),
                    ]),
                ],
            },
        ]
    );
}

#[test]
fn executes_prepared_insert_select_with_defaulted_columns() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE SEQUENCE user_ids; \
             CREATE TABLE users (id BIGINT NOT NULL DEFAULT nextval('user_ids'), name TEXT NOT NULL)",
        )
        .expect("setup");

    let desc = engine
        .prepare(
            &session,
            "ins_select".to_owned(),
            "INSERT INTO users (name) SELECT $1".to_owned(),
        )
        .expect("prepare");
    assert_eq!(desc.param_types, vec![aiondb_core::DataType::Text]);

    engine
        .bind(
            &session,
            "p_select".to_owned(),
            "ins_select".to_owned(),
            vec![aiondb_core::Value::Text("alice".to_owned())],
        )
        .expect("bind");
    let batch = engine
        .execute_portal(&session, "p_select", 0)
        .expect("execute portal");
    assert_eq!(batch.tag, "INSERT");

    let results = engine
        .execute_sql(&session, "SELECT id, name FROM users")
        .expect("select");
    assert_eq!(
        results,
        vec![StatementResult::Query {
            columns: vec![
                ResultColumn {
                    name: "id".to_owned(),
                    data_type: aiondb_core::DataType::BigInt,
                    text_type_modifier: None,
                    nullable: false,
                },
                ResultColumn {
                    name: "name".to_owned(),
                    data_type: aiondb_core::DataType::Text,
                    text_type_modifier: None,
                    nullable: false,
                },
            ],
            rows: vec![aiondb_core::Row::new(vec![
                aiondb_core::Value::BigInt(1),
                aiondb_core::Value::Text("alice".to_owned()),
            ])],
        }]
    );
}

#[test]
fn nextval_reports_undefined_sequence() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let error = engine
        .execute_sql(&session, "SELECT nextval('missing_seq')")
        .expect_err("missing sequence");
    assert_eq!(error.sqlstate(), aiondb_core::SqlState::UndefinedObject);
}

#[test]
fn updates_rows_with_nextval_sequence_values() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let results = engine
        .execute_sql(
            &session,
            "CREATE SEQUENCE user_ids; \
             CREATE TABLE users (id BIGINT, name TEXT); \
             INSERT INTO users VALUES (0, 'alice'), (0, 'bob'); \
             UPDATE users SET id = nextval('user_ids'); \
             SELECT id, name FROM users ORDER BY id ASC",
        )
        .expect("execute");

    assert_eq!(
        results,
        vec![
            StatementResult::Command {
                tag: "CREATE SEQUENCE".to_owned(),
                rows_affected: 0,
            },
            StatementResult::Command {
                tag: "CREATE TABLE".to_owned(),
                rows_affected: 0,
            },
            StatementResult::Command {
                tag: "INSERT".to_owned(),
                rows_affected: 2,
            },
            StatementResult::Command {
                tag: "UPDATE".to_owned(),
                rows_affected: 2,
            },
            StatementResult::Query {
                columns: vec![
                    ResultColumn {
                        name: "id".to_owned(),
                        data_type: aiondb_core::DataType::BigInt,
                        text_type_modifier: None,
                        nullable: true,
                    },
                    ResultColumn {
                        name: "name".to_owned(),
                        data_type: aiondb_core::DataType::Text,
                        text_type_modifier: None,
                        nullable: true,
                    },
                ],
                rows: vec![
                    aiondb_core::Row::new(vec![
                        aiondb_core::Value::BigInt(1),
                        aiondb_core::Value::Text("alice".to_owned()),
                    ]),
                    aiondb_core::Row::new(vec![
                        aiondb_core::Value::BigInt(2),
                        aiondb_core::Value::Text("bob".to_owned()),
                    ]),
                ],
            },
        ]
    );
}

#[test]
fn inserts_rows_using_column_defaults() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let results = engine
        .execute_sql(
            &session,
            "CREATE SEQUENCE user_ids; \
             CREATE TABLE users (id BIGINT NOT NULL DEFAULT nextval('user_ids'), name TEXT NOT NULL DEFAULT 'anon'); \
             INSERT INTO users VALUES (DEFAULT, DEFAULT), (DEFAULT, 'bob'); \
             SELECT id, name FROM users ORDER BY id ASC",
        )
        .expect("execute");

    assert_eq!(
        results,
        vec![
            StatementResult::Command {
                tag: "CREATE SEQUENCE".to_owned(),
                rows_affected: 0,
            },
            StatementResult::Command {
                tag: "CREATE TABLE".to_owned(),
                rows_affected: 0,
            },
            StatementResult::Command {
                tag: "INSERT".to_owned(),
                rows_affected: 2,
            },
            StatementResult::Query {
                columns: vec![
                    ResultColumn {
                        name: "id".to_owned(),
                        data_type: aiondb_core::DataType::BigInt,
                        text_type_modifier: None,
                        nullable: false,
                    },
                    ResultColumn {
                        name: "name".to_owned(),
                        data_type: aiondb_core::DataType::Text,
                        text_type_modifier: None,
                        nullable: false,
                    },
                ],
                rows: vec![
                    aiondb_core::Row::new(vec![
                        aiondb_core::Value::BigInt(1),
                        aiondb_core::Value::Text("anon".to_owned()),
                    ]),
                    aiondb_core::Row::new(vec![
                        aiondb_core::Value::BigInt(2),
                        aiondb_core::Value::Text("bob".to_owned()),
                    ]),
                ],
            },
        ]
    );
}

#[test]
fn inserts_rows_with_column_list_and_omitted_defaults() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let results = engine
        .execute_sql(
            &session,
            "CREATE SEQUENCE user_ids; \
             CREATE TABLE users (id BIGINT NOT NULL DEFAULT nextval('user_ids'), name TEXT NOT NULL, active BOOLEAN NOT NULL DEFAULT TRUE); \
             INSERT INTO users (name) VALUES ('alice'), ('bob'); \
             SELECT id, name, active FROM users ORDER BY id ASC",
        )
        .expect("execute");

    assert_eq!(
        results,
        vec![
            StatementResult::Command {
                tag: "CREATE SEQUENCE".to_owned(),
                rows_affected: 0,
            },
            StatementResult::Command {
                tag: "CREATE TABLE".to_owned(),
                rows_affected: 0,
            },
            StatementResult::Command {
                tag: "INSERT".to_owned(),
                rows_affected: 2,
            },
            StatementResult::Query {
                columns: vec![
                    ResultColumn {
                        name: "id".to_owned(),
                        data_type: aiondb_core::DataType::BigInt,
                        text_type_modifier: None,
                        nullable: false,
                    },
                    ResultColumn {
                        name: "name".to_owned(),
                        data_type: aiondb_core::DataType::Text,
                        text_type_modifier: None,
                        nullable: false,
                    },
                    ResultColumn {
                        name: "active".to_owned(),
                        data_type: aiondb_core::DataType::Boolean,
                        text_type_modifier: None,
                        nullable: false,
                    },
                ],
                rows: vec![
                    aiondb_core::Row::new(vec![
                        aiondb_core::Value::BigInt(1),
                        aiondb_core::Value::Text("alice".to_owned()),
                        aiondb_core::Value::Boolean(true),
                    ]),
                    aiondb_core::Row::new(vec![
                        aiondb_core::Value::BigInt(2),
                        aiondb_core::Value::Text("bob".to_owned()),
                        aiondb_core::Value::Boolean(true),
                    ]),
                ],
            },
        ]
    );
}

#[test]
fn inserts_default_values_row() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let results = engine
        .execute_sql(
            &session,
            "CREATE SEQUENCE user_ids; \
             CREATE TABLE users (id BIGINT NOT NULL DEFAULT nextval('user_ids'), name TEXT NOT NULL DEFAULT 'anon'); \
             INSERT INTO users DEFAULT VALUES; \
             SELECT id, name FROM users",
        )
        .expect("execute");

    assert_eq!(
        results,
        vec![
            StatementResult::Command {
                tag: "CREATE SEQUENCE".to_owned(),
                rows_affected: 0,
            },
            StatementResult::Command {
                tag: "CREATE TABLE".to_owned(),
                rows_affected: 0,
            },
            StatementResult::Command {
                tag: "INSERT".to_owned(),
                rows_affected: 1,
            },
            StatementResult::Query {
                columns: vec![
                    ResultColumn {
                        name: "id".to_owned(),
                        data_type: aiondb_core::DataType::BigInt,
                        text_type_modifier: None,
                        nullable: false,
                    },
                    ResultColumn {
                        name: "name".to_owned(),
                        data_type: aiondb_core::DataType::Text,
                        text_type_modifier: None,
                        nullable: false,
                    },
                ],
                rows: vec![aiondb_core::Row::new(vec![
                    aiondb_core::Value::BigInt(1),
                    aiondb_core::Value::Text("anon".to_owned()),
                ])],
            },
        ]
    );
}

#[test]
fn executes_prepared_insert_with_column_list_and_defaulted_columns() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE SEQUENCE user_ids; \
             CREATE TABLE users (id BIGINT NOT NULL DEFAULT nextval('user_ids'), name TEXT NOT NULL, active BOOLEAN NOT NULL DEFAULT TRUE)",
        )
        .expect("setup");

    let desc = engine
        .prepare(
            &session,
            "ins_user".to_owned(),
            "INSERT INTO users (name) VALUES ($1)".to_owned(),
        )
        .expect("prepare");
    assert_eq!(desc.param_types, vec![aiondb_core::DataType::Text]);

    engine
        .bind(
            &session,
            "p_user".to_owned(),
            "ins_user".to_owned(),
            vec![aiondb_core::Value::Text("alice".to_owned())],
        )
        .expect("bind");

    let batch = engine
        .execute_portal(&session, "p_user", 0)
        .expect("execute portal");
    assert_eq!(batch.tag, "INSERT");

    let results = engine
        .execute_sql(&session, "SELECT id, name, active FROM users")
        .expect("select");
    assert_eq!(
        results,
        vec![StatementResult::Query {
            columns: vec![
                ResultColumn {
                    name: "id".to_owned(),
                    data_type: aiondb_core::DataType::BigInt,
                    text_type_modifier: None,
                    nullable: false,
                },
                ResultColumn {
                    name: "name".to_owned(),
                    data_type: aiondb_core::DataType::Text,
                    text_type_modifier: None,
                    nullable: false,
                },
                ResultColumn {
                    name: "active".to_owned(),
                    data_type: aiondb_core::DataType::Boolean,
                    text_type_modifier: None,
                    nullable: false,
                },
            ],
            rows: vec![aiondb_core::Row::new(vec![
                aiondb_core::Value::BigInt(1),
                aiondb_core::Value::Text("alice".to_owned()),
                aiondb_core::Value::Boolean(true),
            ])],
        }]
    );
}

#[test]
fn schema_qualified_identity_defaults_use_schema_qualified_sequence_names() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let results = engine
        .execute_sql(
            &session,
            "CREATE SCHEMA app; \
             CREATE TABLE app.parent ( \
                 id INT GENERATED BY DEFAULT AS IDENTITY PRIMARY KEY, \
                 name TEXT NOT NULL \
             ); \
             INSERT INTO app.parent (name) VALUES ('alice'); \
             SELECT id, name FROM app.parent",
        )
        .expect("execute schema-qualified identity flow");

    assert_eq!(
        results,
        vec![
            StatementResult::Command {
                tag: "CREATE SCHEMA".to_owned(),
                rows_affected: 0,
            },
            StatementResult::Command {
                tag: "CREATE TABLE".to_owned(),
                rows_affected: 0,
            },
            StatementResult::Command {
                tag: "INSERT".to_owned(),
                rows_affected: 1,
            },
            StatementResult::Query {
                columns: vec![
                    ResultColumn {
                        name: "id".to_owned(),
                        data_type: aiondb_core::DataType::Int,
                        text_type_modifier: None,
                        nullable: false,
                    },
                    ResultColumn {
                        name: "name".to_owned(),
                        data_type: aiondb_core::DataType::Text,
                        text_type_modifier: None,
                        nullable: false,
                    },
                ],
                rows: vec![aiondb_core::Row::new(vec![
                    aiondb_core::Value::Int(1),
                    aiondb_core::Value::Text("alice".to_owned()),
                ])],
            },
        ]
    );

    let defaults = query_rows(
        &engine,
        &session,
        "SELECT adbin \
           FROM pg_catalog.pg_attrdef \
          WHERE adrelid = 'app.parent'::regclass \
            AND adnum = 1",
    );
    assert_eq!(defaults.len(), 1);
    assert_eq!(
        defaults[0].values[0],
        aiondb_core::Value::Text("nextval('app.parent_id_seq')".to_owned())
    );
}
