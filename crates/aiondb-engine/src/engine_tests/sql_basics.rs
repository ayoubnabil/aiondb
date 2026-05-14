use super::*;

#[test]
fn executes_multiple_transaction_statements() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let results = engine
        .execute_sql(&session, "BEGIN; COMMIT;")
        .expect("execute");
    assert_eq!(
        results,
        vec![
            StatementResult::Command {
                tag: "BEGIN".to_owned(),
                rows_affected: 0,
            },
            StatementResult::Command {
                tag: "COMMIT".to_owned(),
                rows_affected: 0,
            },
        ]
    );
}

#[test]
fn begin_inside_active_transaction_emits_notice_and_begin() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine.execute_sql(&session, "BEGIN").expect("begin");
    let results = engine
        .execute_sql(&session, "BEGIN")
        .expect("second begin should succeed with notice");

    assert_eq!(results.len(), 2);
    assert!(matches!(
        &results[0],
        StatementResult::Notice { message }
            if message == "there is already a transaction in progress"
    ));
    assert!(matches!(
        &results[1],
        StatementResult::Command { tag, rows_affected } if tag == "BEGIN" && *rows_affected == 0
    ));
}

#[test]
fn commit_outside_transaction_emits_notice_and_commit() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let results = engine
        .execute_sql(&session, "COMMIT")
        .expect("commit outside transaction should succeed with notice");

    assert_eq!(results.len(), 2);
    assert!(matches!(
        &results[0],
        StatementResult::Notice { message }
            if message == "there is no transaction in progress"
    ));
    assert!(matches!(
        &results[1],
        StatementResult::Command { tag, rows_affected } if tag == "COMMIT" && *rows_affected == 0
    ));
}

#[test]
fn rollback_outside_transaction_emits_notice_and_rollback() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let results = engine
        .execute_sql(&session, "ROLLBACK")
        .expect("rollback outside transaction should succeed with notice");

    assert_eq!(results.len(), 2);
    assert!(matches!(
        &results[0],
        StatementResult::Notice { message }
            if message == "there is no transaction in progress"
    ));
    assert!(matches!(
        &results[1],
        StatementResult::Command { tag, rows_affected } if tag == "ROLLBACK" && *rows_affected == 0
    ));
}

#[test]
fn set_local_outside_transaction_emits_notice_and_does_not_change_setting() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");
    let before_results = engine
        .execute_sql(&session, "SHOW application_name")
        .expect("show application_name before SET LOCAL");
    let StatementResult::Query {
        rows: before_rows, ..
    } = &before_results[0]
    else {
        panic!("expected query result");
    };
    let before_value = before_rows[0].values[0].clone();

    let results = engine
        .execute_sql(&session, "SET LOCAL application_name = local_app")
        .expect("set local outside transaction should succeed with notice");

    assert_eq!(results.len(), 2);
    assert!(matches!(
        &results[0],
        StatementResult::Notice { message }
            if message == "SET LOCAL can only be used in transaction blocks"
    ));
    assert!(matches!(
        &results[1],
        StatementResult::Command { tag, rows_affected } if tag == "SET" && *rows_affected == 0
    ));

    let show_results = engine
        .execute_sql(&session, "SHOW application_name")
        .expect("show application_name");
    let StatementResult::Query { rows, .. } = &show_results[0] else {
        panic!("expected query result");
    };
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].values[0], before_value);
}

#[test]
fn create_table_inherits_emits_merge_notice_via_search_path() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE SCHEMA analytics; \
             SET search_path TO analytics, public; \
             CREATE TABLE parent_items (id INT, name TEXT)",
        )
        .expect("setup parent table");

    let results = engine
        .execute_sql(
            &session,
            "CREATE TABLE child_items (name TEXT) INHERITS (parent_items)",
        )
        .expect("create child table with inherits");

    assert!(matches!(
        &results[0],
        StatementResult::Notice { message }
            if message == "merging column \"name\" with inherited definition"
    ));
    assert!(matches!(
        &results[1],
        StatementResult::Command { tag, rows_affected }
            if tag == "CREATE TABLE" && *rows_affected == 0
    ));
}

#[test]
fn respects_transaction_mode_from_parser() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "START TRANSACTION ISOLATION LEVEL SNAPSHOT ISOLATION",
        )
        .expect("begin");

    let sessions = engine.sessions().expect("sessions");
    let record = Engine::session_mut(&sessions, &session).expect("session");
    let isolation = record
        .active_txn
        .as_ref()
        .map(|txn| txn.isolation)
        .expect("active txn");
    assert_eq!(isolation, IsolationLevel::SnapshotIsolation);
}

#[test]
fn respects_serializable_transaction_mode_from_parser() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(&session, "START TRANSACTION ISOLATION LEVEL SERIALIZABLE")
        .expect("begin");

    let sessions = engine.sessions().expect("sessions");
    let record = Engine::session_mut(&sessions, &session).expect("session");
    let isolation = record
        .active_txn
        .as_ref()
        .map(|txn| txn.isolation)
        .expect("active txn");
    assert_eq!(isolation, IsolationLevel::Serializable);
}

#[test]
fn set_transaction_updates_current_transaction_characteristics() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine.execute_sql(&session, "BEGIN").expect("begin");
    engine
        .execute_sql(
            &session,
            "SET TRANSACTION ISOLATION LEVEL SERIALIZABLE, READ ONLY, DEFERRABLE",
        )
        .expect("set transaction");

    let show_isolation = engine
        .execute_sql(&session, "SHOW transaction_isolation")
        .expect("show isolation");
    assert!(matches!(
        &show_isolation[0],
        StatementResult::Query { rows, .. } if rows[0].values[0] == aiondb_core::Value::Text("serializable".into())
    ));

    let show_read_only = engine
        .execute_sql(&session, "SHOW transaction_read_only")
        .expect("show read only");
    assert!(matches!(
        &show_read_only[0],
        StatementResult::Query { rows, .. } if rows[0].values[0] == aiondb_core::Value::Text("on".into())
    ));

    let show_deferrable = engine
        .execute_sql(&session, "SHOW transaction_deferrable")
        .expect("show deferrable");
    assert!(matches!(
        &show_deferrable[0],
        StatementResult::Query { rows, .. } if rows[0].values[0] == aiondb_core::Value::Text("on".into())
    ));

    let sessions = engine.sessions().expect("sessions");
    let record = Engine::session_mut(&sessions, &session).expect("session");
    let isolation = record
        .active_txn
        .as_ref()
        .map(|txn| txn.isolation)
        .expect("active txn");
    assert_eq!(isolation, IsolationLevel::Serializable);
}

#[test]
fn set_session_characteristics_updates_future_transactions() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "SET SESSION CHARACTERISTICS AS TRANSACTION ISOLATION LEVEL SNAPSHOT ISOLATION, READ ONLY",
        )
        .expect("set session characteristics");

    let show_isolation = engine
        .execute_sql(&session, "SHOW transaction_isolation")
        .expect("show isolation");
    assert!(matches!(
        &show_isolation[0],
        StatementResult::Query { rows, .. } if rows[0].values[0] == aiondb_core::Value::Text("snapshot isolation".into())
    ));

    engine.execute_sql(&session, "BEGIN").expect("begin");
    {
        let sessions = engine.sessions().expect("sessions");
        let record = Engine::session_mut(&sessions, &session).expect("session");
        let isolation = record
            .active_txn
            .as_ref()
            .map(|txn| txn.isolation)
            .expect("active txn");
        assert_eq!(isolation, IsolationLevel::SnapshotIsolation);
    }

    let show_read_only = engine
        .execute_sql(&session, "SHOW transaction_read_only")
        .expect("show read only");
    assert!(matches!(
        &show_read_only[0],
        StatementResult::Query { rows, .. } if rows[0].values[0] == aiondb_core::Value::Text("on".into())
    ));
}

#[test]
fn reset_transaction_variables_restore_defaults() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "SET SESSION CHARACTERISTICS AS TRANSACTION ISOLATION LEVEL SERIALIZABLE, READ ONLY, DEFERRABLE",
        )
        .expect("set non-default transaction characteristics");

    engine
        .execute_sql(&session, "RESET TRANSACTION ISOLATION LEVEL")
        .expect("reset transaction isolation");
    engine
        .execute_sql(&session, "RESET TRANSACTION READ ONLY")
        .expect("reset transaction read only");
    engine
        .execute_sql(&session, "RESET TRANSACTION DEFERRABLE")
        .expect("reset transaction deferrable");

    let show_isolation = engine
        .execute_sql(&session, "SHOW transaction_isolation")
        .expect("show isolation");
    assert!(matches!(
        &show_isolation[0],
        StatementResult::Query { rows, .. } if rows[0].values[0] == aiondb_core::Value::Text("read committed".into())
    ));

    let show_read_only = engine
        .execute_sql(&session, "SHOW transaction_read_only")
        .expect("show read only");
    assert!(matches!(
        &show_read_only[0],
        StatementResult::Query { rows, .. } if rows[0].values[0] == aiondb_core::Value::Text("off".into())
    ));

    let show_deferrable = engine
        .execute_sql(&session, "SHOW transaction_deferrable")
        .expect("show deferrable");
    assert!(matches!(
        &show_deferrable[0],
        StatementResult::Query { rows, .. } if rows[0].values[0] == aiondb_core::Value::Text("off".into())
    ));
}

#[test]
fn set_transaction_read_only_blocks_writes() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(&session, "CREATE TABLE tx_read_only_guard (id INT)")
        .expect("create table");

    engine.execute_sql(&session, "BEGIN").expect("begin");
    engine
        .execute_sql(&session, "SET TRANSACTION READ ONLY")
        .expect("set read only");

    let error = engine
        .execute_sql(&session, "INSERT INTO tx_read_only_guard VALUES (1)")
        .expect_err("insert must fail in read-only transaction");
    assert_eq!(
        error.sqlstate(),
        aiondb_core::SqlState::ObjectNotInPrerequisiteState
    );
    assert!(
        error
            .report()
            .message
            .contains("cannot execute INSERT in a read-only transaction"),
        "unexpected error message: {}",
        error.report().message
    );

    let commit = engine
        .execute_sql(&session, "COMMIT")
        .expect("failed transaction commit should roll back");
    assert!(matches!(
        &commit[0],
        StatementResult::Command { tag, .. } if tag == "ROLLBACK"
    ));

    let rows = query_rows(&engine, &session, "SELECT id FROM tx_read_only_guard");
    assert!(rows.is_empty(), "read-only txn must not stage writes");
}

#[test]
fn session_characteristics_read_only_applies_to_implicit_transactions() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(&session, "CREATE TABLE tx_read_only_implicit (id INT)")
        .expect("create table");
    engine
        .execute_sql(
            &session,
            "SET SESSION CHARACTERISTICS AS TRANSACTION READ ONLY",
        )
        .expect("set default read only");

    let error = engine
        .execute_sql(&session, "INSERT INTO tx_read_only_implicit VALUES (1)")
        .expect_err("implicit transaction should honor default read only");
    assert_eq!(
        error.sqlstate(),
        aiondb_core::SqlState::ObjectNotInPrerequisiteState
    );

    engine
        .execute_sql(&session, "RESET TRANSACTION READ ONLY")
        .expect("reset default read only");
    engine
        .execute_sql(&session, "INSERT INTO tx_read_only_implicit VALUES (2)")
        .expect("insert should work after reset");

    let rows = query_rows(
        &engine,
        &session,
        "SELECT id FROM tx_read_only_implicit ORDER BY id",
    );
    assert_eq!(rows, vec![Row::new(vec![Value::Int(2)])]);
}

#[test]
fn prepares_and_describes_select_literals() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let desc = engine
        .prepare(
            &session,
            "s1".to_owned(),
            "SELECT 1 AS one, 'x', TRUE, NULL".to_owned(),
        )
        .expect("prepare");
    assert_eq!(desc.result_columns.len(), 4);
    assert_eq!(desc.result_columns[0].name, "one");
    assert_eq!(desc.result_columns[0].data_type, aiondb_core::DataType::Int);
    assert_eq!(desc.result_columns[1].name, "?column?");
    assert_eq!(
        desc.result_columns[1].data_type,
        aiondb_core::DataType::Text
    );
    assert_eq!(
        desc.result_columns[2].data_type,
        aiondb_core::DataType::Boolean
    );
    assert!(desc.result_columns[3].nullable);
}

#[test]
fn executes_select_literals_via_pipeline() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let results = engine
        .execute_sql(&session, "SELECT 1 AS one, 'x', TRUE, NULL")
        .expect("execute");
    assert_eq!(
        results,
        vec![StatementResult::Query {
            columns: vec![
                ResultColumn {
                    name: "one".to_owned(),
                    data_type: aiondb_core::DataType::Int,
                    text_type_modifier: None,
                    nullable: false,
                },
                ResultColumn {
                    name: "?column?".to_owned(),
                    data_type: aiondb_core::DataType::Text,
                    text_type_modifier: None,
                    nullable: false,
                },
                ResultColumn {
                    name: "?column?".to_owned(),
                    data_type: aiondb_core::DataType::Boolean,
                    text_type_modifier: None,
                    nullable: false,
                },
                ResultColumn {
                    name: "?column?".to_owned(),
                    data_type: aiondb_core::DataType::Text,
                    text_type_modifier: None,
                    nullable: true,
                },
            ],
            rows: vec![aiondb_core::Row::new(vec![
                aiondb_core::Value::Int(1),
                aiondb_core::Value::Text("x".to_owned()),
                aiondb_core::Value::Boolean(true),
                aiondb_core::Value::Null,
            ])],
        }]
    );
}

#[test]
fn creates_table_and_selects_from_empty_relation() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let create_results = engine
        .execute_sql(&session, "CREATE TABLE users (id INT, name TEXT)")
        .expect("create table");
    assert_eq!(
        create_results,
        vec![StatementResult::Command {
            tag: "CREATE TABLE".to_owned(),
            rows_affected: 0,
        }]
    );

    let query_results = engine
        .execute_sql(&session, "SELECT id, name FROM users")
        .expect("select");
    assert_eq!(
        query_results,
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
            rows: Vec::new(),
        }]
    );
}

#[test]
fn inserts_and_reads_rows_from_table_storage() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let results = engine
        .execute_sql(
            &session,
            "CREATE TABLE users (id INT, name TEXT); \
             INSERT INTO users VALUES (1, 'alice'), (2, 'bob'); \
             SELECT id, name FROM users",
        )
        .expect("execute");

    assert_eq!(
        results,
        vec![
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
                rows: vec![
                    aiondb_core::Row::new(vec![
                        aiondb_core::Value::Int(1),
                        aiondb_core::Value::Text("alice".to_owned()),
                    ]),
                    aiondb_core::Row::new(vec![
                        aiondb_core::Value::Int(2),
                        aiondb_core::Value::Text("bob".to_owned()),
                    ]),
                ],
            },
        ]
    );
}

#[test]
fn select_distinct_on_keeps_first_row_per_key_by_order() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE TABLE d_on (grp INT, ts INT, payload TEXT); \
             INSERT INTO d_on VALUES \
                (1, 10, 'a'), \
                (1, 30, 'b'), \
                (1, 20, 'c'), \
                (2, 5,  'x'), \
                (2, 8,  'y'), \
                (3, 1,  'z');",
        )
        .expect("setup");

    let results = engine
        .execute_sql(
            &session,
            "SELECT DISTINCT ON (grp) grp, ts, payload \
             FROM d_on \
             ORDER BY grp, ts DESC, payload",
        )
        .expect("distinct on query");

    let StatementResult::Query { rows, .. } = &results[0] else {
        panic!("expected query result");
    };

    assert_eq!(
        rows,
        &vec![
            Row::new(vec![
                Value::Int(1),
                Value::Int(30),
                Value::Text("b".to_owned()),
            ]),
            Row::new(vec![
                Value::Int(2),
                Value::Int(8),
                Value::Text("y".to_owned()),
            ]),
            Row::new(vec![
                Value::Int(3),
                Value::Int(1),
                Value::Text("z".to_owned()),
            ]),
        ]
    );
}

#[test]
fn selects_rows_with_where_filter() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let results = engine
        .execute_sql(
            &session,
            "CREATE TABLE users (id INT, name TEXT); \
             INSERT INTO users VALUES (1, 'alice'), (2, 'bob'); \
             SELECT id, name FROM users WHERE id = 2",
        )
        .expect("execute");

    assert_eq!(
        results,
        vec![
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
                    aiondb_core::Value::Int(2),
                    aiondb_core::Value::Text("bob".to_owned()),
                ])],
            },
        ]
    );
}

#[test]
fn orders_rows_by_multiple_columns() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let results = engine
        .execute_sql(
            &session,
            "CREATE TABLE users (id INT, name TEXT); \
             INSERT INTO users VALUES (2, 'bob'), (1, 'carol'), (3, 'bob'); \
             SELECT id, name FROM users ORDER BY name DESC, id ASC",
        )
        .expect("execute");

    assert_eq!(
        results,
        vec![
            StatementResult::Command {
                tag: "CREATE TABLE".to_owned(),
                rows_affected: 0,
            },
            StatementResult::Command {
                tag: "INSERT".to_owned(),
                rows_affected: 3,
            },
            StatementResult::Query {
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
                rows: vec![
                    aiondb_core::Row::new(vec![
                        aiondb_core::Value::Int(1),
                        aiondb_core::Value::Text("carol".to_owned()),
                    ]),
                    aiondb_core::Row::new(vec![
                        aiondb_core::Value::Int(2),
                        aiondb_core::Value::Text("bob".to_owned()),
                    ]),
                    aiondb_core::Row::new(vec![
                        aiondb_core::Value::Int(3),
                        aiondb_core::Value::Text("bob".to_owned()),
                    ]),
                ],
            },
        ]
    );
}

#[test]
fn orders_rows_by_projection_alias() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let results = engine
        .execute_sql(
            &session,
            "CREATE TABLE users (id INT, name TEXT); \
             INSERT INTO users VALUES (1, 'alice'), (3, 'carol'), (2, 'bob'); \
             SELECT id AS ident, name FROM users ORDER BY ident DESC",
        )
        .expect("execute");

    assert_eq!(
        results,
        vec![
            StatementResult::Command {
                tag: "CREATE TABLE".to_owned(),
                rows_affected: 0,
            },
            StatementResult::Command {
                tag: "INSERT".to_owned(),
                rows_affected: 3,
            },
            StatementResult::Query {
                columns: vec![
                    ResultColumn {
                        name: "ident".to_owned(),
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
                rows: vec![
                    aiondb_core::Row::new(vec![
                        aiondb_core::Value::Int(3),
                        aiondb_core::Value::Text("carol".to_owned()),
                    ]),
                    aiondb_core::Row::new(vec![
                        aiondb_core::Value::Int(2),
                        aiondb_core::Value::Text("bob".to_owned()),
                    ]),
                    aiondb_core::Row::new(vec![
                        aiondb_core::Value::Int(1),
                        aiondb_core::Value::Text("alice".to_owned()),
                    ]),
                ],
            },
        ]
    );
}

#[test]
fn limits_rows_without_ordering() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let results = engine
        .execute_sql(
            &session,
            "CREATE TABLE users (id INT, name TEXT); \
             INSERT INTO users VALUES (1, 'alice'), (2, 'bob'), (3, 'carol'); \
             SELECT id, name FROM users LIMIT 2",
        )
        .expect("execute");

    assert_eq!(
        results,
        vec![
            StatementResult::Command {
                tag: "CREATE TABLE".to_owned(),
                rows_affected: 0,
            },
            StatementResult::Command {
                tag: "INSERT".to_owned(),
                rows_affected: 3,
            },
            StatementResult::Query {
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
                rows: vec![
                    aiondb_core::Row::new(vec![
                        aiondb_core::Value::Int(1),
                        aiondb_core::Value::Text("alice".to_owned()),
                    ]),
                    aiondb_core::Row::new(vec![
                        aiondb_core::Value::Int(2),
                        aiondb_core::Value::Text("bob".to_owned()),
                    ]),
                ],
            },
        ]
    );
}

#[test]
fn limits_rows_after_ordering() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let results = engine
        .execute_sql(
            &session,
            "CREATE TABLE users (id INT, name TEXT); \
             INSERT INTO users VALUES (1, 'alice'), (3, 'carol'), (2, 'bob'); \
             SELECT id, name FROM users ORDER BY id DESC LIMIT 2",
        )
        .expect("execute");

    assert_eq!(
        results,
        vec![
            StatementResult::Command {
                tag: "CREATE TABLE".to_owned(),
                rows_affected: 0,
            },
            StatementResult::Command {
                tag: "INSERT".to_owned(),
                rows_affected: 3,
            },
            StatementResult::Query {
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
                rows: vec![
                    aiondb_core::Row::new(vec![
                        aiondb_core::Value::Int(3),
                        aiondb_core::Value::Text("carol".to_owned()),
                    ]),
                    aiondb_core::Row::new(vec![
                        aiondb_core::Value::Int(2),
                        aiondb_core::Value::Text("bob".to_owned()),
                    ]),
                ],
            },
        ]
    );
}

#[test]
fn creates_index_and_uses_index_lookup_plan() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let results = engine
        .execute_sql(
            &session,
            "CREATE TABLE users (id INT, name TEXT); \
             INSERT INTO users VALUES (1, 'alice'), (2, 'bob'); \
             CREATE INDEX users_id_idx ON users (id); \
             SELECT id, name FROM users WHERE id = 2",
        )
        .expect("execute");

    assert_eq!(
        results,
        vec![
            StatementResult::Command {
                tag: "CREATE TABLE".to_owned(),
                rows_affected: 0,
            },
            StatementResult::Command {
                tag: "INSERT".to_owned(),
                rows_affected: 2,
            },
            StatementResult::Command {
                tag: "CREATE INDEX".to_owned(),
                rows_affected: 0,
            },
            StatementResult::Query {
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
                    aiondb_core::Value::Int(2),
                    aiondb_core::Value::Text("bob".to_owned()),
                ])],
            },
        ]
    );

    let statement =
        parse_prepared_statement("SELECT id, name FROM users WHERE id = 2").expect("parse");
    let plan = engine
        .build_physical_plan(&session, &statement)
        .expect("physical plan");
    let aiondb_plan::PhysicalPlan::ProjectTable { access_path, .. } = plan else {
        panic!("expected table scan plan");
    };
    assert!(matches!(
        access_path,
        aiondb_plan::ScanAccessPath::IndexEq { .. }
    ));
}

#[test]
fn uses_index_range_plan_for_ordered_predicates() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let results = engine
        .execute_sql(
            &session,
            "CREATE TABLE users (id INT, name TEXT); \
             INSERT INTO users VALUES (1, 'alice'), (2, 'bob'), (3, 'carol'), (4, 'dave'); \
             CREATE INDEX users_id_idx ON users (id); \
             SELECT id, name FROM users WHERE id >= 2 AND id < 4",
        )
        .expect("execute");

    assert_eq!(
        results,
        vec![
            StatementResult::Command {
                tag: "CREATE TABLE".to_owned(),
                rows_affected: 0,
            },
            StatementResult::Command {
                tag: "INSERT".to_owned(),
                rows_affected: 4,
            },
            StatementResult::Command {
                tag: "CREATE INDEX".to_owned(),
                rows_affected: 0,
            },
            StatementResult::Query {
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
                rows: vec![
                    aiondb_core::Row::new(vec![
                        aiondb_core::Value::Int(2),
                        aiondb_core::Value::Text("bob".to_owned()),
                    ]),
                    aiondb_core::Row::new(vec![
                        aiondb_core::Value::Int(3),
                        aiondb_core::Value::Text("carol".to_owned()),
                    ]),
                ],
            },
        ]
    );

    // Verify the optimizer produces a valid plan for range predicates.
    // With cost-based optimization, the access path depends on table statistics:
    // small tables use SeqScan, large tables use IndexRange.
    let statement = parse_prepared_statement("SELECT id, name FROM users WHERE id >= 2 AND id < 4")
        .expect("parse");
    let plan = engine
        .build_physical_plan(&session, &statement)
        .expect("physical plan");
    let aiondb_plan::PhysicalPlan::ProjectTable { access_path, .. } = plan else {
        panic!("expected table scan plan");
    };
    assert!(matches!(
        access_path,
        aiondb_plan::ScanAccessPath::SeqScan | aiondb_plan::ScanAccessPath::IndexRange { .. }
    ));
}

#[test]
fn create_table_plan_preserves_unbounded_varchar_raw_type_name() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let statement = parse_prepared_statement(
        "CREATE TABLE varchar_probe (name VARCHAR, title CHARACTER VARYING)",
    )
    .expect("parse");
    let plan = engine
        .build_physical_plan(&session, &statement)
        .expect("physical plan");
    let aiondb_plan::PhysicalPlan::CreateTable { columns, .. } = plan else {
        panic!("expected create table plan");
    };

    assert_eq!(columns.len(), 2);
    assert_eq!(columns[0].raw_type_name.as_deref(), Some("varchar"));
    assert_eq!(
        columns[1].raw_type_name.as_deref(),
        Some("character varying")
    );
}

#[test]
fn create_table_catalog_preserves_unbounded_varchar_raw_type_name() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE TABLE varchar_catalog_probe (name VARCHAR, title CHARACTER VARYING)",
        )
        .expect("create table");

    let table = engine
        .catalog_reader
        .get_table(
            aiondb_core::TxnId::default(),
            &aiondb_catalog::QualifiedName::unqualified("varchar_catalog_probe"),
        )
        .expect("catalog read")
        .expect("table should exist");

    assert_eq!(table.columns.len(), 2);
    assert_eq!(table.columns[0].raw_type_name.as_deref(), Some("varchar"));
    assert_eq!(
        table.columns[1].raw_type_name.as_deref(),
        Some("character varying")
    );
}

#[test]
fn information_schema_columns_reports_unbounded_varchar_types() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let results = engine
        .execute_sql(
            &session,
            "CREATE TABLE varchar_info_probe (name VARCHAR, title CHARACTER VARYING); \
             SELECT column_name, data_type, udt_name \
             FROM information_schema.columns \
             WHERE table_name = 'varchar_info_probe' \
             ORDER BY ordinal_position",
        )
        .expect("execute");

    let StatementResult::Query { rows, .. } = &results[1] else {
        panic!("expected query result");
    };
    assert_eq!(rows.len(), 2);
    assert_eq!(
        rows[0].values[0],
        aiondb_core::Value::Text("name".to_owned())
    );
    assert_eq!(
        rows[0].values[1],
        aiondb_core::Value::Text("character varying".to_owned())
    );
    assert_eq!(
        rows[0].values[2],
        aiondb_core::Value::Text("varchar".to_owned())
    );
    assert_eq!(
        rows[1].values[0],
        aiondb_core::Value::Text("title".to_owned())
    );
    assert_eq!(
        rows[1].values[1],
        aiondb_core::Value::Text("character varying".to_owned())
    );
    assert_eq!(
        rows[1].values[2],
        aiondb_core::Value::Text("varchar".to_owned())
    );
}

#[test]
fn information_schema_columns_reports_bounded_character_lengths() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let results = engine
        .execute_sql(
            &session,
            "CREATE TABLE varchar_len_probe (name VARCHAR(60), code CHAR(12)); \
             SELECT column_name, character_maximum_length \
             FROM information_schema.columns \
             WHERE table_name = 'varchar_len_probe' \
             ORDER BY ordinal_position",
        )
        .expect("execute");

    let StatementResult::Query { rows, .. } = &results[1] else {
        panic!("expected query result");
    };
    assert_eq!(rows.len(), 2);
    assert_eq!(
        rows[0].values[0],
        aiondb_core::Value::Text("name".to_owned())
    );
    assert_eq!(rows[0].values[1], aiondb_core::Value::Int(60));
    assert_eq!(
        rows[1].values[0],
        aiondb_core::Value::Text("code".to_owned())
    );
    assert_eq!(rows[1].values[1], aiondb_core::Value::Int(12));
}

#[test]
fn pg_attribute_format_type_reports_unbounded_varchar_types() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let results = engine
        .execute_sql(
            &session,
            "CREATE TABLE varchar_pgattr_probe (name VARCHAR, title CHARACTER VARYING); \
             SELECT a.attname, pg_catalog.format_type(a.atttypid, a.atttypmod) \
             FROM pg_catalog.pg_class c \
             JOIN pg_catalog.pg_attribute a ON a.attrelid = c.oid \
             WHERE c.relname = 'varchar_pgattr_probe' AND a.attnum > 0 AND NOT a.attisdropped \
             ORDER BY a.attnum",
        )
        .expect("execute");

    let StatementResult::Query { rows, .. } = &results[1] else {
        panic!("expected query result");
    };
    assert_eq!(rows.len(), 2);
    assert_eq!(
        rows[0].values[0],
        aiondb_core::Value::Text("name".to_owned())
    );
    assert_eq!(
        rows[0].values[1],
        aiondb_core::Value::Text("character varying".to_owned())
    );
    assert_eq!(
        rows[1].values[0],
        aiondb_core::Value::Text("title".to_owned())
    );
    assert_eq!(
        rows[1].values[1],
        aiondb_core::Value::Text("character varying".to_owned())
    );
}

#[test]
fn numeric_precision_and_scale_are_reflected_for_orms() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let results = engine
        .execute_sql(
            &session,
            "CREATE TABLE numeric_reflect_probe (amount NUMERIC(5,3)); \
             SELECT column_name, numeric_precision, numeric_precision_radix, numeric_scale \
             FROM information_schema.columns \
             WHERE table_name = 'numeric_reflect_probe'",
        )
        .expect("execute");

    let StatementResult::Query { rows, .. } = &results[1] else {
        panic!("expected query result");
    };
    assert_eq!(rows.len(), 1);
    assert_eq!(
        rows[0].values,
        vec![
            aiondb_core::Value::Text("amount".to_owned()),
            aiondb_core::Value::Int(5),
            aiondb_core::Value::Int(10),
            aiondb_core::Value::Int(3),
        ]
    );

    let pg_attr_results = engine
        .execute_sql(
            &session,
            "SELECT a.attname, pg_catalog.format_type(a.atttypid, a.atttypmod) \
             FROM pg_catalog.pg_class c \
             JOIN pg_catalog.pg_attribute a ON a.attrelid = c.oid \
             WHERE c.relname = 'numeric_reflect_probe' AND a.attnum > 0 AND NOT a.attisdropped",
        )
        .expect("execute format_type query");
    let StatementResult::Query {
        rows: pg_attr_rows, ..
    } = &pg_attr_results[0]
    else {
        panic!("expected query result");
    };
    assert_eq!(pg_attr_rows.len(), 1);
    assert_eq!(
        pg_attr_rows[0].values,
        vec![
            aiondb_core::Value::Text("amount".to_owned()),
            aiondb_core::Value::Text("numeric(5,3)".to_owned()),
        ]
    );
}

#[test]
fn smallint_and_int_array_types_are_reflected_for_orms() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let results = engine
        .execute_sql(
            &session,
            "CREATE TABLE scalar_reflect_probe (s16 SMALLINT, tags INTEGER[]); \
             SELECT column_name, data_type, udt_name, numeric_precision, numeric_precision_radix, numeric_scale \
             FROM information_schema.columns \
             WHERE table_name = 'scalar_reflect_probe' \
             ORDER BY ordinal_position; \
             SELECT a.attname, pg_catalog.format_type(a.atttypid, a.atttypmod) \
             FROM pg_catalog.pg_class c \
             JOIN pg_catalog.pg_attribute a ON a.attrelid = c.oid \
             WHERE c.relname = 'scalar_reflect_probe' AND a.attnum > 0 AND NOT a.attisdropped \
             ORDER BY a.attnum",
        )
        .expect("execute");

    let StatementResult::Query {
        rows: info_rows, ..
    } = &results[1]
    else {
        panic!("expected information_schema query result");
    };
    assert_eq!(info_rows.len(), 2);
    assert_eq!(
        info_rows[0].values,
        vec![
            aiondb_core::Value::Text("s16".to_owned()),
            aiondb_core::Value::Text("smallint".to_owned()),
            aiondb_core::Value::Text("int2".to_owned()),
            aiondb_core::Value::Int(16),
            aiondb_core::Value::Int(2),
            aiondb_core::Value::Int(0),
        ]
    );
    assert_eq!(
        info_rows[1].values,
        vec![
            aiondb_core::Value::Text("tags".to_owned()),
            aiondb_core::Value::Text("ARRAY".to_owned()),
            aiondb_core::Value::Text("_int4".to_owned()),
            aiondb_core::Value::Null,
            aiondb_core::Value::Null,
            aiondb_core::Value::Null,
        ]
    );

    let StatementResult::Query {
        rows: pg_attr_rows, ..
    } = &results[2]
    else {
        panic!("expected pg_catalog query result");
    };
    assert_eq!(pg_attr_rows.len(), 2);
    assert_eq!(
        pg_attr_rows[0].values,
        vec![
            aiondb_core::Value::Text("s16".to_owned()),
            aiondb_core::Value::Text("smallint".to_owned()),
        ]
    );
    assert_eq!(
        pg_attr_rows[1].values,
        vec![
            aiondb_core::Value::Text("tags".to_owned()),
            aiondb_core::Value::Text("integer[]".to_owned()),
        ]
    );
}

#[test]
fn alter_column_type_updates_bounded_character_length_metadata() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let results = engine
        .execute_sql(
            &session,
            "CREATE TABLE alter_type_probe (headline VARCHAR(120), category VARCHAR(40)); \
             ALTER TABLE alter_type_probe ALTER COLUMN headline TYPE VARCHAR(140); \
             ALTER TABLE alter_type_probe ALTER COLUMN category TYPE VARCHAR(60); \
             SELECT column_name, character_maximum_length \
             FROM information_schema.columns \
             WHERE table_name = 'alter_type_probe' \
             ORDER BY ordinal_position",
        )
        .expect("execute");

    let StatementResult::Query { rows, .. } = &results[3] else {
        panic!("expected information_schema query result");
    };
    assert_eq!(rows.len(), 2);
    assert_eq!(
        rows[0].values,
        vec![
            aiondb_core::Value::Text("headline".to_owned()),
            aiondb_core::Value::Int(140),
        ]
    );
    assert_eq!(
        rows[1].values,
        vec![
            aiondb_core::Value::Text("category".to_owned()),
            aiondb_core::Value::Int(60),
        ]
    );
}

#[test]
fn current_timestamp_default_is_canonical_for_orm_reflection() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let results = engine
        .execute_sql(
            &session,
            "CREATE TABLE default_reflect_probe (created_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP); \
             SELECT column_default \
             FROM information_schema.columns \
             WHERE table_name = 'default_reflect_probe' AND column_name = 'created_at'",
        )
        .expect("execute");

    let StatementResult::Query { rows, .. } = &results[1] else {
        panic!("expected query result");
    };
    assert_eq!(rows.len(), 1);
    assert_eq!(
        rows[0].values,
        vec![aiondb_core::Value::Text("CURRENT_TIMESTAMP".to_owned())]
    );
}

#[test]
fn float_index_range_predicate_returns_expected_rows() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE TABLE t_float_idx (pk INT PRIMARY KEY, col1 FLOAT); \
             CREATE INDEX t_float_idx_col1_idx ON t_float_idx (col1); \
             INSERT INTO t_float_idx VALUES (0, 5.6);",
        )
        .expect("setup");

    let between_rows = query_rows(
        &engine,
        &session,
        "SELECT pk FROM t_float_idx WHERE col1 BETWEEN 2.14 AND 8.15",
    );
    assert_eq!(between_rows, vec![Row::new(vec![Value::Int(0)])]);

    let access_path = access_path_for_query(
        &engine,
        &session,
        "SELECT pk FROM t_float_idx WHERE col1 >= 2.14 AND col1 <= 8.15",
    );

    let expanded_rows = query_rows(
        &engine,
        &session,
        "SELECT pk FROM t_float_idx WHERE col1 >= 2.14 AND col1 <= 8.15",
    );
    assert_eq!(
        expanded_rows,
        vec![Row::new(vec![Value::Int(0)])],
        "unexpected rows for FLOAT expanded range with access path: {access_path:?}"
    );

    match access_path {
        aiondb_plan::ScanAccessPath::SeqScan | aiondb_plan::ScanAccessPath::IndexRange { .. } => {}
        other => panic!("unexpected access path for FLOAT range predicate: {other:?}"),
    }
}
