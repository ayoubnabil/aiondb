use super::*;

#[test]
fn cypher_create_existing_label_mixes_existing_and_missing_properties() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE TABLE people (id BIGINT NOT NULL, score BIGINT); \
             CREATE TABLE derived (id BIGINT, category TEXT); \
             INSERT INTO people VALUES (1, 7); \
             INSERT INTO derived VALUES (100, 'legacy'); \
             CREATE NODE LABEL Person ON people; \
             CREATE NODE LABEL Derived ON derived",
        )
        .expect("setup");

    engine
        .execute_sql(
            &session,
            "MATCH (m:Person {id: 1}) \
             CREATE (n:Derived {category: 'fresh', copied: m.score}) \
             RETURN 1",
        )
        .expect("create derived node with existing+missing properties");

    let legacy = engine
        .execute_sql(
            &session,
            "SELECT category, copied FROM derived WHERE id = 100",
        )
        .expect("legacy row should remain readable after mixed property insert");
    assert_eq!(
        legacy,
        vec![StatementResult::Query {
            columns: vec![
                ResultColumn {
                    name: "category".to_owned(),
                    data_type: aiondb_core::DataType::Text,
                    text_type_modifier: None,
                    nullable: true,
                },
                ResultColumn {
                    name: "copied".to_owned(),
                    data_type: aiondb_core::DataType::BigInt,
                    text_type_modifier: None,
                    nullable: true,
                },
            ],
            rows: vec![aiondb_core::Row::new(vec![
                aiondb_core::Value::Text("legacy".to_owned()),
                aiondb_core::Value::Null,
            ])],
        }]
    );

    let created = engine
        .execute_sql(
            &session,
            "SELECT category, copied FROM derived WHERE copied = 7",
        )
        .expect("new row should persist both existing and inferred properties");
    assert_eq!(
        created,
        vec![StatementResult::Query {
            columns: vec![
                ResultColumn {
                    name: "category".to_owned(),
                    data_type: aiondb_core::DataType::Text,
                    text_type_modifier: None,
                    nullable: true,
                },
                ResultColumn {
                    name: "copied".to_owned(),
                    data_type: aiondb_core::DataType::BigInt,
                    text_type_modifier: None,
                    nullable: true,
                },
            ],
            rows: vec![aiondb_core::Row::new(vec![
                aiondb_core::Value::Text("fresh".to_owned()),
                aiondb_core::Value::BigInt(7),
            ])],
        }]
    );
}

#[test]
fn cypher_create_existing_label_respects_statement_memory_budget_during_rewrite() {
    let mut runtime = aiondb_config::RuntimeConfig::default();
    runtime.limits.max_memory_bytes = 64;
    let engine = EngineBuilder::for_testing()
        .with_runtime_config(runtime)
        .build()
        .unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let oversized_payload = "x".repeat(256);
    engine
        .execute_sql(
            &session,
            &format!(
                "CREATE TABLE people (id BIGINT NOT NULL, score BIGINT); \
                 CREATE TABLE derived (id BIGINT, payload TEXT); \
                 INSERT INTO people VALUES (1, 7); \
                 INSERT INTO derived VALUES (100, '{oversized_payload}'); \
                 CREATE NODE LABEL Person ON people; \
                 CREATE NODE LABEL Derived ON derived"
            ),
        )
        .expect("setup");

    let err = engine
        .execute_sql(
            &session,
            "MATCH (m:Person {id: 1}) \
             CREATE (n:Derived {copied: m.score}) \
             RETURN 1",
        )
        .expect_err("rewrite should fail under a tight statement memory budget");
    let msg = format!("{err}");
    assert!(
        msg.contains("maximum memory budget exceeded for this statement"),
        "unexpected error: {msg}"
    );
}

#[test]
fn cypher_implicit_column_rewrite_failure_does_not_publish_partial_state_in_explicit_txn() {
    let mut runtime = aiondb_config::RuntimeConfig::default();
    runtime.limits.max_memory_bytes = 64;
    let engine = EngineBuilder::for_testing()
        .with_runtime_config(runtime)
        .build()
        .unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let oversized_payload = "x".repeat(256);
    engine
        .execute_sql(
            &session,
            &format!(
                "CREATE TABLE people (id BIGINT NOT NULL, score BIGINT); \
                 CREATE TABLE derived (id BIGINT, payload TEXT); \
                 INSERT INTO people VALUES (1, 7); \
                 INSERT INTO derived VALUES (100, '{oversized_payload}'); \
                 CREATE NODE LABEL Person ON people; \
                 CREATE NODE LABEL Derived ON derived"
            ),
        )
        .expect("setup");

    engine.execute_sql(&session, "BEGIN").expect("begin");
    engine
        .execute_sql(&session, "SAVEPOINT sp_graph_rewrite")
        .expect("savepoint");
    let err = engine
        .execute_sql(
            &session,
            "MATCH (m:Person {id: 1}) \
             CREATE (n:Derived {copied: m.score}) \
             RETURN 1",
        )
        .expect_err("rewrite should fail under a tight statement memory budget");
    assert_eq!(err.sqlstate(), aiondb_core::SqlState::ProgramLimitExceeded);
    engine
        .execute_sql(&session, "ROLLBACK TO SAVEPOINT sp_graph_rewrite")
        .expect("recover transaction after failed Cypher rewrite");

    engine
        .execute_sql(
            &session,
            "INSERT INTO derived (id, payload) VALUES (101, 'ok_after_failure')",
        )
        .expect("transaction should remain usable after failed Cypher rewrite");
    engine
        .execute_sql(&session, "COMMIT")
        .expect("commit after failed Cypher rewrite");

    let inserted = engine
        .execute_sql(&session, "SELECT payload FROM derived WHERE id = 101")
        .expect("post-failure insert should commit");
    assert_eq!(
        inserted,
        vec![StatementResult::Query {
            columns: vec![ResultColumn {
                name: "payload".to_owned(),
                data_type: aiondb_core::DataType::Text,
                text_type_modifier: None,
                nullable: true,
            }],
            rows: vec![aiondb_core::Row::new(vec![aiondb_core::Value::Text(
                "ok_after_failure".to_owned(),
            )])],
        }]
    );

    let error = engine
        .execute_sql(&session, "SELECT copied FROM derived")
        .expect_err("failed rewrite must not publish implicit column metadata");
    assert_eq!(error.sqlstate(), aiondb_core::SqlState::UndefinedColumn);
}

#[test]
fn cypher_implicit_column_rewrite_failure_can_be_rolled_back_to_user_savepoint() {
    let mut runtime = aiondb_config::RuntimeConfig::default();
    runtime.limits.max_memory_bytes = 64;
    let engine = EngineBuilder::for_testing()
        .with_runtime_config(runtime)
        .build()
        .unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let oversized_payload = "x".repeat(256);
    engine
        .execute_sql(
            &session,
            &format!(
                "CREATE TABLE people (id BIGINT NOT NULL, score BIGINT); \
                 CREATE TABLE derived (id BIGINT, payload TEXT); \
                 INSERT INTO people VALUES (1, 7); \
                 INSERT INTO derived VALUES (100, '{oversized_payload}'); \
                 CREATE NODE LABEL Person ON people; \
                 CREATE NODE LABEL Derived ON derived"
            ),
        )
        .expect("setup");

    engine.execute_sql(&session, "BEGIN").expect("begin");
    engine
        .execute_sql(&session, "SAVEPOINT sp1")
        .expect("create user savepoint");

    let err = engine
        .execute_sql(
            &session,
            "MATCH (m:Person {id: 1}) \
             CREATE (n:Derived {copied: m.score}) \
             RETURN 1",
        )
        .expect_err("rewrite should fail under a tight statement memory budget");
    assert_eq!(err.sqlstate(), aiondb_core::SqlState::ProgramLimitExceeded);

    engine
        .execute_sql(&session, "ROLLBACK TO SAVEPOINT sp1")
        .expect("rollback to user savepoint after failed Cypher rewrite");
    engine
        .execute_sql(
            &session,
            "INSERT INTO derived (id, payload) VALUES (101, 'ok_after_savepoint')",
        )
        .expect("transaction should remain usable after rollback to savepoint");
    engine
        .execute_sql(&session, "COMMIT")
        .expect("commit after rollback to savepoint");

    let inserted = engine
        .execute_sql(&session, "SELECT payload FROM derived WHERE id = 101")
        .expect("post-savepoint insert should commit");
    assert_eq!(
        inserted,
        vec![StatementResult::Query {
            columns: vec![ResultColumn {
                name: "payload".to_owned(),
                data_type: aiondb_core::DataType::Text,
                text_type_modifier: None,
                nullable: true,
            }],
            rows: vec![aiondb_core::Row::new(vec![aiondb_core::Value::Text(
                "ok_after_savepoint".to_owned(),
            )])],
        }]
    );

    let error = engine
        .execute_sql(&session, "SELECT copied FROM derived")
        .expect_err("failed rewrite must not publish implicit column metadata");
    assert_eq!(error.sqlstate(), aiondb_core::SqlState::UndefinedColumn);
}

#[test]
fn cypher_create_existing_label_adds_missing_column_from_bound_property() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE TABLE people (id BIGINT NOT NULL, score BIGINT); \
             CREATE TABLE derived (id BIGINT NOT NULL); \
             INSERT INTO people VALUES (1, 7); \
             CREATE NODE LABEL Person ON people; \
             CREATE NODE LABEL Derived ON derived",
        )
        .expect("setup");

    engine
        .execute_sql(
            &session,
            "MATCH (m:Person {id: 1}) CREATE (n:Derived {copied: m.score}) RETURN 1",
        )
        .expect("create derived node on existing label");

    let results = engine
        .execute_sql(&session, "SELECT copied FROM derived")
        .expect("select derived rows");
    assert_eq!(
        results,
        vec![StatementResult::Query {
            columns: vec![ResultColumn {
                name: "copied".to_owned(),
                data_type: aiondb_core::DataType::BigInt,
                text_type_modifier: None,
                nullable: true,
            }],
            rows: vec![aiondb_core::Row::new(vec![aiondb_core::Value::BigInt(7)])],
        }]
    );
}

#[test]
fn cypher_set_existing_label_adds_missing_column_from_property_assignment() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE TABLE people (id BIGINT NOT NULL, name TEXT); \
             INSERT INTO people VALUES (1, 'alice'); \
             CREATE NODE LABEL Person ON people",
        )
        .expect("setup");

    engine
        .execute_sql(
            &session,
            "MATCH (n:Person {id: 1}) SET n.score = 7 RETURN 1",
        )
        .expect("set missing property on existing label");

    let results = engine
        .execute_sql(&session, "SELECT score FROM people WHERE id = 1")
        .expect("select updated score");
    assert_eq!(
        results,
        vec![StatementResult::Query {
            columns: vec![ResultColumn {
                name: "score".to_owned(),
                data_type: aiondb_core::DataType::BigInt,
                text_type_modifier: None,
                nullable: true,
            }],
            rows: vec![aiondb_core::Row::new(vec![aiondb_core::Value::BigInt(7)])],
        }]
    );
}

#[test]
fn cypher_set_existing_edge_label_adds_missing_column_from_property_assignment() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE TABLE people (id BIGINT NOT NULL, name TEXT); \
             CREATE TABLE knows (source_id BIGINT NOT NULL, target_id BIGINT NOT NULL); \
             CREATE NODE LABEL Person ON people; \
             CREATE EDGE LABEL KNOWS ON knows SOURCE Person TARGET Person; \
             CREATE (:Person {id: 1, name: 'alice'}) RETURN 1; \
             CREATE (:Person {id: 2, name: 'bob'}) RETURN 1; \
             MATCH (a:Person {id: 1}), (b:Person {id: 2}) CREATE (a)-[:KNOWS]->(b) RETURN 1",
        )
        .expect("setup graph data");

    engine
        .execute_sql(
            &session,
            "MATCH (a:Person {id: 1})-[e:KNOWS]->(b:Person {id: 2}) SET e.since = 2024 RETURN 1",
        )
        .expect("set missing edge property on existing label");

    let results = engine
        .execute_sql(&session, "SELECT since FROM knows")
        .expect("select updated edge property");
    assert_eq!(
        results,
        vec![StatementResult::Query {
            columns: vec![ResultColumn {
                name: "since".to_owned(),
                data_type: aiondb_core::DataType::BigInt,
                text_type_modifier: None,
                nullable: true,
            }],
            rows: vec![aiondb_core::Row::new(vec![aiondb_core::Value::BigInt(
                2024
            )])],
        }]
    );
}

#[test]
fn cypher_set_id_then_detach_delete_removes_incident_edges() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE TABLE people (id BIGINT NOT NULL, name TEXT); \
             CREATE TABLE knows (source_id BIGINT NOT NULL, target_id BIGINT NOT NULL); \
             CREATE NODE LABEL Person ON people; \
             CREATE EDGE LABEL KNOWS ON knows SOURCE Person TARGET Person; \
             CREATE (:Person {id: 1, name: 'alice'}) RETURN 1; \
             CREATE (:Person {id: 2, name: 'bob'}) RETURN 1; \
             MATCH (a:Person {id: 1}), (b:Person {id: 2}) CREATE (a)-[:KNOWS]->(b) RETURN 1",
        )
        .expect("setup graph data");

    engine
        .execute_sql(
            &session,
            "MATCH (a:Person {id: 1}) SET a.id = 99 DETACH DELETE a",
        )
        .expect("set id then detach delete");

    let remaining_people = query_rows(&engine, &session, "SELECT id, name FROM people");
    assert_eq!(remaining_people.len(), 1);
    assert_eq!(remaining_people[0].values[0], Value::BigInt(2));
    assert_eq!(remaining_people[0].values[1], Value::Text("bob".to_owned()));

    let remaining_edges = query_rows(&engine, &session, "SELECT COUNT(*) FROM knows");
    assert_eq!(remaining_edges[0].values[0], Value::BigInt(0));
}

#[test]
fn cypher_create_existing_label_backfills_existing_rows_after_implicit_column_add() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE TABLE people (id BIGINT NOT NULL, score BIGINT); \
             CREATE TABLE derived (id BIGINT, note TEXT); \
             INSERT INTO people VALUES (1, 7); \
             INSERT INTO derived VALUES (100, 'legacy'); \
             CREATE NODE LABEL Person ON people; \
             CREATE NODE LABEL Derived ON derived",
        )
        .expect("setup");

    engine
        .execute_sql(
            &session,
            "MATCH (m:Person {id: 1}) CREATE (n:Derived {copied: m.score}) RETURN 1",
        )
        .expect("create derived node on existing label with legacy rows");

    let legacy = engine
        .execute_sql(&session, "SELECT copied FROM derived WHERE id = 100")
        .expect("legacy row should remain readable after implicit column add");
    assert_eq!(
        legacy,
        vec![StatementResult::Query {
            columns: vec![ResultColumn {
                name: "copied".to_owned(),
                data_type: aiondb_core::DataType::BigInt,
                text_type_modifier: None,
                nullable: true,
            }],
            rows: vec![aiondb_core::Row::new(vec![aiondb_core::Value::Null])],
        }]
    );

    let created = engine
        .execute_sql(&session, "SELECT copied FROM derived WHERE copied = 7")
        .expect("new row should persist copied value");
    assert_eq!(
        created,
        vec![StatementResult::Query {
            columns: vec![ResultColumn {
                name: "copied".to_owned(),
                data_type: aiondb_core::DataType::BigInt,
                text_type_modifier: None,
                nullable: true,
            }],
            rows: vec![aiondb_core::Row::new(vec![aiondb_core::Value::BigInt(7)])],
        }]
    );
}

#[test]
fn cypher_create_existing_label_adds_multiple_missing_columns_in_single_create() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE TABLE people (id BIGINT NOT NULL, score BIGINT); \
             CREATE TABLE derived (id BIGINT, note TEXT); \
             INSERT INTO people VALUES (1, 7); \
             INSERT INTO derived VALUES (100, 'legacy'); \
             CREATE NODE LABEL Person ON people; \
             CREATE NODE LABEL Derived ON derived",
        )
        .expect("setup");

    engine
        .execute_sql(
            &session,
            "MATCH (m:Person {id: 1}) \
             CREATE (n:Derived {copied: m.score, category: 'fresh'}) \
             RETURN 1",
        )
        .expect("create derived node with multiple new properties");

    let legacy = engine
        .execute_sql(
            &session,
            "SELECT copied, category FROM derived WHERE id = 100",
        )
        .expect("legacy row should remain readable after multiple implicit column adds");
    assert_eq!(
        legacy,
        vec![StatementResult::Query {
            columns: vec![
                ResultColumn {
                    name: "copied".to_owned(),
                    data_type: aiondb_core::DataType::BigInt,
                    text_type_modifier: None,
                    nullable: true,
                },
                ResultColumn {
                    name: "category".to_owned(),
                    data_type: aiondb_core::DataType::Text,
                    text_type_modifier: None,
                    nullable: true,
                },
            ],
            rows: vec![aiondb_core::Row::new(vec![
                aiondb_core::Value::Null,
                aiondb_core::Value::Null,
            ])],
        }]
    );

    let created = engine
        .execute_sql(
            &session,
            "SELECT copied, category FROM derived WHERE copied = 7",
        )
        .expect("new row should persist all newly inferred properties");
    assert_eq!(
        created,
        vec![StatementResult::Query {
            columns: vec![
                ResultColumn {
                    name: "copied".to_owned(),
                    data_type: aiondb_core::DataType::BigInt,
                    text_type_modifier: None,
                    nullable: true,
                },
                ResultColumn {
                    name: "category".to_owned(),
                    data_type: aiondb_core::DataType::Text,
                    text_type_modifier: None,
                    nullable: true,
                },
            ],
            rows: vec![aiondb_core::Row::new(vec![
                aiondb_core::Value::BigInt(7),
                aiondb_core::Value::Text("fresh".to_owned()),
            ])],
        }]
    );
}
