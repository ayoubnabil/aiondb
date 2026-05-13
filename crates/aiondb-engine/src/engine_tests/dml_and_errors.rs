use super::*;

mod compat_misc;
#[path = "dml_and_errors_compat_prepare_and_describe.rs"]
mod compat_prepare_and_describe;

#[test]
fn selects_rows_with_not_and_not_equal_filter() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let results = engine
        .execute_sql(
            &session,
            "CREATE TABLE users (id INT, name TEXT); \
             INSERT INTO users VALUES (1, 'alice'), (2, 'bob'), (3, 'bob'); \
             SELECT id, name FROM users WHERE name != 'alice' AND NOT id = 3",
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
                rows: vec![aiondb_core::Row::new(vec![
                    aiondb_core::Value::Int(2),
                    aiondb_core::Value::Text("bob".to_owned()),
                ])],
            },
        ]
    );
}

#[test]
fn updates_rows_in_table_storage() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let results = engine
        .execute_sql(
            &session,
            "CREATE TABLE users (id INT, name TEXT); \
             INSERT INTO users VALUES (1, 'alice'), (2, 'bob'); \
             UPDATE users SET id = id, name = 'updated'; \
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
            StatementResult::Command {
                tag: "UPDATE".to_owned(),
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
                        aiondb_core::Value::Text("updated".to_owned()),
                    ]),
                    aiondb_core::Row::new(vec![
                        aiondb_core::Value::Int(2),
                        aiondb_core::Value::Text("updated".to_owned()),
                    ]),
                ],
            },
        ]
    );
}

#[test]
fn updates_rows_with_where_filter() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let results = engine
        .execute_sql(
            &session,
            "CREATE TABLE users (id INT, name TEXT); \
             INSERT INTO users VALUES (1, 'alice'), (2, 'bob'); \
             UPDATE users SET name = 'updated' WHERE id = 2; \
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
            StatementResult::Command {
                tag: "UPDATE".to_owned(),
                rows_affected: 1,
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
                        aiondb_core::Value::Text("updated".to_owned()),
                    ]),
                ],
            },
        ]
    );
}

#[test]
fn updates_rows_with_ordered_and_logical_filter() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let results = engine
        .execute_sql(
            &session,
            "CREATE TABLE users (id INT, name TEXT); \
             INSERT INTO users VALUES (1, 'alice'), (2, 'bob'), (3, 'carol'); \
             UPDATE users SET name = 'matched' WHERE id >= 2 AND name > 'a'; \
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
                rows_affected: 3,
            },
            StatementResult::Command {
                tag: "UPDATE".to_owned(),
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
                        aiondb_core::Value::Text("matched".to_owned()),
                    ]),
                    aiondb_core::Row::new(vec![
                        aiondb_core::Value::Int(3),
                        aiondb_core::Value::Text("matched".to_owned()),
                    ]),
                ],
            },
        ]
    );
}

#[test]
fn deletes_rows_from_table_storage() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let results = engine
        .execute_sql(
            &session,
            "CREATE TABLE users (id INT, name TEXT); \
             INSERT INTO users VALUES (1, 'alice'), (2, 'bob'); \
             DELETE FROM users; \
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
            StatementResult::Command {
                tag: "DELETE".to_owned(),
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
                rows: Vec::new(),
            },
        ]
    );
}

#[test]
fn deletes_rows_with_where_filter() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let results = engine
        .execute_sql(
            &session,
            "CREATE TABLE users (id INT, name TEXT); \
             INSERT INTO users VALUES (1, 'alice'), (2, 'bob'); \
             DELETE FROM users WHERE id = 1; \
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
            StatementResult::Command {
                tag: "DELETE".to_owned(),
                rows_affected: 1,
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
fn deletes_rows_with_logical_or_filter() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let results = engine
        .execute_sql(
            &session,
            "CREATE TABLE users (id INT, name TEXT); \
             INSERT INTO users VALUES (1, 'alice'), (2, 'bob'), (3, 'carol'); \
             DELETE FROM users WHERE id < 2 OR name = 'carol'; \
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
                rows_affected: 3,
            },
            StatementResult::Command {
                tag: "DELETE".to_owned(),
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
fn reports_undefined_table_for_unknown_relation() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let error = engine
        .execute_sql(&session, "SELECT id FROM users")
        .expect_err("undefined table");
    assert_eq!(error.sqlstate(), aiondb_core::SqlState::UndefinedTable);
}

#[test]
fn execute_sql_reports_undefined_column_for_unbound_identifier() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let error = engine
        .execute_sql(&session, "SELECT missing")
        .expect_err("undefined column");
    assert_eq!(error.sqlstate(), aiondb_core::SqlState::UndefinedColumn);
}

#[test]
fn update_reports_undefined_column_for_unknown_target() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(&session, "CREATE TABLE users (id INT, name TEXT)")
        .expect("create");
    let error = engine
        .execute_sql(&session, "UPDATE users SET missing = 1")
        .expect_err("undefined column");
    assert_eq!(error.sqlstate(), aiondb_core::SqlState::UndefinedColumn);
}

#[test]
fn where_rejects_numeric_expression() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(&session, "CREATE TABLE users (id INT, name TEXT)")
        .expect("create");
    let error = engine
        .execute_sql(&session, "SELECT id FROM users WHERE 1")
        .expect_err("non-boolean WHERE must be rejected");
    assert_eq!(error.sqlstate(), aiondb_core::SqlState::DatatypeMismatch);
}

#[test]
fn not_rejects_numeric_expression() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(&session, "CREATE TABLE users (id INT, name TEXT)")
        .expect("create");
    let error = engine
        .execute_sql(&session, "SELECT id FROM users WHERE NOT 1")
        .expect_err("NOT on non-boolean must be rejected");
    assert_eq!(error.sqlstate(), aiondb_core::SqlState::DatatypeMismatch);
}

#[test]
fn rejects_invalid_sql_during_prepare() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let error = engine
        .prepare(&session, "bad".to_owned(), "CREATE TABLE".to_owned())
        .expect_err("invalid sql");
    assert_eq!(error.sqlstate(), aiondb_core::SqlState::SyntaxError);
}

#[test]
fn rejects_unsupported_compatibility_command_during_execute() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    // CREATE EVENT TRIGGER is parsed as a tagged compat statement and
    // intentionally not present in the matrix (no real engine support);
    // the terminal compat guardrail must reject it with
    // `feature_not_supported` instead of forging a fake `command_ok`.
    // a real typed Statement and routes through the planner.)
    let error = engine
        .execute_sql(
            &session,
            "CREATE EVENT TRIGGER my_et ON ddl_command_start \
             EXECUTE PROCEDURE my_fn()",
        )
        .expect_err("CREATE EVENT TRIGGER should fail instead of reporting a fake success");
    assert_eq!(error.sqlstate(), aiondb_core::SqlState::FeatureNotSupported);
    assert!(error.report().message.contains("CREATE EVENT TRIGGER"));
}

#[test]
fn rejects_malformed_prepare_and_execute_compatibility_commands_during_execute() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    for command in ["PREPARE", "EXECUTE"] {
        let error = engine
            .execute_sql(&session, command)
            .expect_err("malformed compatibility command should fail");
        assert_eq!(error.sqlstate(), aiondb_core::SqlState::FeatureNotSupported);
        assert!(
            error.report().message.contains(command),
            "expected error to mention {command}, got: {error}"
        );
    }
}

#[test]
fn prepare_rejects_malformed_prepare_and_execute_compatibility_commands() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    for (statement_name, sql, tag) in [
        ("bad_prepare", "PREPARE", "PREPARE"),
        ("bad_execute", "EXECUTE", "EXECUTE"),
    ] {
        let error = engine
            .prepare(&session, statement_name.to_owned(), sql.to_owned())
            .expect_err("malformed compatibility command should fail at prepare time");
        assert_eq!(error.sqlstate(), aiondb_core::SqlState::FeatureNotSupported);
        assert!(
            error.report().message.contains(tag),
            "expected error to mention {tag}, got: {error}"
        );
    }
}

#[test]
fn executes_well_formed_compat_prepare_and_execute_commands() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let prepare_results = engine
        .execute_sql(&session, "PREPARE stmt AS SELECT 7")
        .expect("well-formed PREPARE should succeed");
    assert_eq!(
        prepare_results,
        vec![StatementResult::Command {
            tag: "PREPARE".to_owned(),
            rows_affected: 0,
        }]
    );

    let execute_results = engine
        .execute_sql(&session, "EXECUTE stmt")
        .expect("well-formed EXECUTE should succeed");
    assert_eq!(execute_results.len(), 1);
    match &execute_results[0] {
        StatementResult::Query { rows, .. } => {
            assert_eq!(
                rows,
                &vec![aiondb_core::Row::new(vec![aiondb_core::Value::Int(7)])]
            );
        }
        other => panic!("expected query result, got {other:?}"),
    }
}

#[test]
fn compat_execute_evaluates_argument_expressions_as_grouped_sql() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(&session, "PREPARE stmt(int) AS SELECT $1 * 2")
        .expect("prepare should succeed");

    let execute_results = engine
        .execute_sql(&session, "EXECUTE stmt(1 + 1)")
        .expect("execute should succeed");
    assert_eq!(execute_results.len(), 1);
    match &execute_results[0] {
        StatementResult::Query { rows, .. } => {
            assert_eq!(
                rows,
                &vec![aiondb_core::Row::new(vec![aiondb_core::Value::Int(4)])]
            );
        }
        other => panic!("expected query result, got {other:?}"),
    }
}

#[test]
fn compat_execute_does_not_substitute_placeholders_inside_string_literals() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "PREPARE stmt AS SELECT '$1' AS literal, $1::INT AS bound",
        )
        .expect("prepare should succeed");

    let execute_results = engine
        .execute_sql(&session, "EXECUTE stmt(42)")
        .expect("execute should succeed");
    assert_eq!(execute_results.len(), 1);
    match &execute_results[0] {
        StatementResult::Query { rows, .. } => {
            assert_eq!(
                rows,
                &vec![aiondb_core::Row::new(vec![
                    aiondb_core::Value::Text("$1".to_owned()),
                    aiondb_core::Value::Int(42),
                ])]
            );
        }
        other => panic!("expected query result, got {other:?}"),
    }
}

#[test]
fn compat_execute_allows_comment_commas_in_argument_list() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "PREPARE stmt(int, int) AS SELECT $1 + $2 AS total",
        )
        .expect("prepare should succeed");

    let execute_results = engine
        .execute_sql(&session, "EXECUTE stmt(1 /*, ignored */, 2)")
        .expect("execute with comment comma should succeed");
    assert_eq!(execute_results.len(), 1);
    match &execute_results[0] {
        StatementResult::Query { rows, .. } => {
            assert_eq!(
                rows,
                &vec![aiondb_core::Row::new(vec![aiondb_core::Value::Int(3)])]
            );
        }
        other => panic!("expected query result, got {other:?}"),
    }
}

#[test]
fn compat_execute_rewrites_current_of_for_prepared_update() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE TABLE compat_exec_current_of (id INT); \
             INSERT INTO compat_exec_current_of VALUES (1), (2)",
        )
        .expect("seed");
    engine.execute_sql(&session, "BEGIN").expect("begin");
    engine
        .execute_sql(
            &session,
            "DECLARE c CURSOR FOR SELECT id FROM compat_exec_current_of ORDER BY id",
        )
        .expect("declare cursor");
    engine
        .execute_sql(&session, "FETCH NEXT IN c")
        .expect("position cursor");
    engine
        .execute_sql(
            &session,
            "PREPARE stmt(int) AS UPDATE compat_exec_current_of SET id = $1 WHERE CURRENT OF c",
        )
        .expect("prepare current of update");

    let execute_results = engine
        .execute_sql(&session, "EXECUTE stmt(20)")
        .expect("execute should succeed");
    assert_eq!(
        execute_results,
        vec![StatementResult::Command {
            tag: "UPDATE".to_owned(),
            rows_affected: 1,
        }]
    );

    assert_eq!(
        query_rows(
            &engine,
            &session,
            "SELECT id FROM compat_exec_current_of ORDER BY id"
        ),
        vec![
            aiondb_core::Row::new(vec![aiondb_core::Value::Int(2)]),
            aiondb_core::Row::new(vec![aiondb_core::Value::Int(20)]),
        ]
    );
}

#[test]
fn current_of_text_inside_string_literal_does_not_trigger_cursor_rewrite() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE TABLE current_of_literal_guard (id INT, note TEXT); \
             INSERT INTO current_of_literal_guard VALUES (1, 'seed')",
        )
        .expect("seed");

    let update_results = engine
        .execute_sql(
            &session,
            "UPDATE current_of_literal_guard \
             SET note = 'current of missing_cursor' \
             WHERE id = 1",
        )
        .expect("string literal containing current of should not trigger cursor rewrite");
    assert_eq!(
        update_results,
        vec![StatementResult::Command {
            tag: "UPDATE".to_owned(),
            rows_affected: 1,
        }]
    );

    match &engine
        .execute_sql(
            &session,
            "SELECT note FROM current_of_literal_guard WHERE id = 1",
        )
        .expect("select updated note")[0]
    {
        StatementResult::Query { rows, .. } => {
            assert_eq!(
                rows,
                &vec![aiondb_core::Row::new(vec![aiondb_core::Value::Text(
                    "current of missing_cursor".to_owned()
                )])]
            );
        }
        other => panic!("expected query result, got {other:?}"),
    }
}

#[test]
fn compat_execute_supports_prepared_do_block() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "PREPARE stmt AS DO $$ BEGIN RAISE NOTICE '%', 'hello from compat execute'; END $$ LANGUAGE plpgsql",
        )
        .expect("prepare compat do");

    let results = engine
        .execute_sql(&session, "EXECUTE stmt")
        .expect("execute compat do");
    assert_eq!(results.len(), 2);
    assert!(matches!(
        &results[0],
        StatementResult::Notice { message } if message == "hello from compat execute"
    ));
    assert!(matches!(
        &results[1],
        StatementResult::Command { tag, rows_affected } if tag == "DO" && *rows_affected == 0
    ));
}

#[test]
fn compat_do_block_with_declared_null_variable_executes_via_plpgsql_v2() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let results = engine
        .execute_sql(
            &session,
            "DO $$ DECLARE x text; BEGIN RAISE NOTICE '%', x; END $$ LANGUAGE plpgsql",
        )
        .expect("DO block should execute");
    assert_eq!(results.len(), 2);
    assert!(matches!(
        &results[0],
        StatementResult::Notice { message } if message == "<NULL>"
    ));
    assert!(matches!(
        &results[1],
        StatementResult::Command { tag, rows_affected } if tag == "DO" && *rows_affected == 0
    ));
}

#[test]
fn compat_do_block_insert_returning_into_tid_variable_tracks_ctid() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(&session, "CREATE TABLE do_tid_probe (v INT)")
        .expect("create probe table");

    let results = engine
        .execute_sql(
            &session,
            "DO $$
             DECLARE curtid tid;
             BEGIN
               INSERT INTO do_tid_probe VALUES (1) RETURNING ctid INTO curtid;
               RAISE NOTICE '%', curtid;
             END $$ LANGUAGE plpgsql",
        )
        .expect("do block with INSERT RETURNING INTO should execute");

    assert!(
        results.iter().any(|result| {
            matches!(
                result,
                StatementResult::Notice { message }
                    if message.contains("(0,") || message.contains("TidValue")
            )
        }),
        "expected non-null ctid notice, got: {results:?}"
    );
}

#[test]
fn insert_returning_ctid_is_non_null() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(&session, "CREATE TABLE ctid_probe (v INT)")
        .expect("create table");
    let results = engine
        .execute_sql(&session, "INSERT INTO ctid_probe VALUES (1) RETURNING ctid")
        .expect("insert returning ctid");

    let first = results.first().expect("query result");
    match first {
        StatementResult::Query { rows, .. } => {
            let value = rows
                .first()
                .and_then(|row| row.values.first())
                .cloned()
                .unwrap_or(aiondb_core::Value::Null);
            assert!(!value.is_null(), "ctid should be non-null, got {value:?}");
        }
        other => panic!("expected query result, got {other:?}"),
    }
}

#[test]
fn invalid_compat_do_language_suffix_is_rejected() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let error = engine
        .execute_sql(
            &session,
            "DO $$ BEGIN RAISE NOTICE '%', 'hello'; END $$ LANGUAGE sql",
        )
        .expect_err("unsupported DO language should fail");
    assert_eq!(error.sqlstate(), aiondb_core::SqlState::FeatureNotSupported);
    assert!(error
        .report()
        .message
        .contains("unsupported compatibility command: DO"));
}

#[test]
fn unsupported_compat_do_dynamic_execute_is_rejected_explicitly() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let error = engine
        .execute_sql(
            &session,
            "DO $$ BEGIN EXECUTE format('SELECT %s', 1); END $$ LANGUAGE plpgsql",
        )
        .expect_err("unsupported compat DO fallback must fail explicitly");
    assert_eq!(error.sqlstate(), aiondb_core::SqlState::FeatureNotSupported);
    assert!(error
        .report()
        .message
        .contains("unsupported compatibility command: DO"));
}

#[test]
fn compat_do_psql_dobody_variable_is_rejected_explicitly() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let error = engine
        .execute_sql(&session, "DO :'dobody'")
        .expect_err("compat DO psql variable fallback must fail explicitly");
    assert_eq!(error.sqlstate(), aiondb_core::SqlState::FeatureNotSupported);
    assert!(error
        .report()
        .message
        .contains("unsupported compatibility command: DO"));
}

#[test]
fn compat_do_invalid_parameter_swallow_pattern_is_rejected_explicitly() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let error = engine
        .execute_sql(
            &session,
            "DO $$
             BEGIN
               BEGIN
                 EXECUTE 'SET effective_io_concurrency = 0';
               EXCEPTION WHEN invalid_parameter_value THEN
                 NULL;
               END;
             END $$ LANGUAGE plpgsql",
        )
        .expect_err("compat DO no-op swallow path must fail explicitly");
    assert_eq!(error.sqlstate(), aiondb_core::SqlState::FeatureNotSupported);
    assert!(error
        .report()
        .message
        .contains("unsupported compatibility command: DO"));
}

#[test]
fn compat_do_oidjoins_synthetic_notice_path_is_rejected_explicitly() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let error = engine
        .execute_sql(
            &session,
            "DO $$
             BEGIN
               PERFORM pg_get_catalog_foreign_keys();
               RAISE NOTICE 'checking % % => % %';
             END $$ LANGUAGE plpgsql",
        )
        .expect_err("compat oidjoins synthetic DO path must fail explicitly");
    assert_eq!(error.sqlstate(), aiondb_core::SqlState::FeatureNotSupported);
    assert!(error
        .report()
        .message
        .contains("unsupported compatibility command: DO"));
}

#[test]
fn compat_do_exec_format_alter_database_owner_placeholder_form_is_rejected_explicitly() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let error = engine
        .execute_sql(
            &session,
            "DO $$
             BEGIN
               EXECUTE format('ALTER DATABASE %I OWNER TO %I', current_catalog, 'aiondb');
             END $$ LANGUAGE plpgsql",
        )
        .expect_err("unsupported compat DO ALTER DATABASE format variant must fail explicitly");
    assert_eq!(error.sqlstate(), aiondb_core::SqlState::FeatureNotSupported);
    assert!(error
        .report()
        .message
        .contains("unsupported compatibility command: DO"));
}

#[test]
fn compat_do_object_address_warning_probe_shape_is_rejected_explicitly() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let error = engine
        .execute_sql(
            &session,
            "DO $$
             DECLARE objtype text;
             BEGIN
               FOR objtype IN VALUES ('toast table'), ('index column'), ('sequence column'), ('toast table column'), ('view column'), ('materialized view column') LOOP
                 PERFORM pg_get_object_address(objtype, '{one}', '{}');
               EXCEPTION WHEN invalid_parameter_value THEN
                 RAISE WARNING 'error for %: %', objtype, SQLERRM;
               END LOOP;
             END $$ LANGUAGE plpgsql",
        )
        .expect_err("unsupported compat DO object_address warning path must fail explicitly");
    assert_eq!(error.sqlstate(), aiondb_core::SqlState::FeatureNotSupported);
    assert!(error
        .report()
        .message
        .contains("unsupported compatibility command: DO"));
}

#[test]
fn compat_execute_supports_prepared_create_type_command() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(&session, "PREPARE stmt AS CREATE TYPE shell_alias")
        .expect("prepare compat create type");

    let results = engine
        .execute_sql(&session, "EXECUTE stmt")
        .expect("execute compat create type");
    assert_eq!(
        results,
        vec![StatementResult::Command {
            tag: "CREATE TYPE".to_owned(),
            rows_affected: 0,
        }]
    );

    engine
        .execute_sql(
            &session,
            "CREATE FUNCTION shell_alias_out(shell_alias) RETURNS cstring \
             STRICT IMMUTABLE LANGUAGE internal AS 'int8out'",
        )
        .expect("shell type created through EXECUTE stmt should be tracked");
}

#[test]
fn compat_create_operator_is_rejected_explicitly() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let error = engine
        .execute_sql(
            &session,
            "CREATE OPERATOR === (
               LEFTARG = integer,
               RIGHTARG = integer,
               PROCEDURE = int4eq
             )",
        )
        .expect_err("CREATE OPERATOR compat path must fail explicitly");
    assert_eq!(error.sqlstate(), aiondb_core::SqlState::FeatureNotSupported);
    assert!(error
        .report()
        .message
        .contains("unsupported compatibility command: CREATE OPERATOR"));
}

#[test]
fn compat_drop_operator_is_rejected_explicitly() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let missing = engine
        .execute_sql(&session, "DROP OPERATOR === (integer, integer)")
        .expect_err("DROP OPERATOR compat path must fail explicitly");
    assert_eq!(
        missing.sqlstate(),
        aiondb_core::SqlState::FeatureNotSupported
    );
    assert!(missing
        .report()
        .message
        .contains("unsupported compatibility command: DROP OPERATOR"));

    let if_exists = engine
        .execute_sql(&session, "DROP OPERATOR IF EXISTS === (integer, integer)")
        .expect_err("DROP OPERATOR IF EXISTS must fail explicitly");
    assert_eq!(
        if_exists.sqlstate(),
        aiondb_core::SqlState::FeatureNotSupported
    );
    assert!(if_exists
        .report()
        .message
        .contains("unsupported compatibility command: DROP OPERATOR"));
}

#[test]
fn alter_function_existing_target_is_rejected_explicitly() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE FUNCTION alter_fn_probe(i int4) RETURNS int4 \
             LANGUAGE sql IMMUTABLE AS $$ SELECT i $$",
        )
        .expect("create function");

    let error = engine
        .execute_sql(
            &session,
            "ALTER FUNCTION alter_fn_probe(int4) OWNER TO aiondb",
        )
        .expect_err("ALTER FUNCTION should fail explicitly when target exists");
    assert_eq!(error.sqlstate(), aiondb_core::SqlState::FeatureNotSupported);
    assert!(error
        .report()
        .message
        .contains("unsupported compatibility command: ALTER FUNCTION"));
}

#[test]
fn alter_aggregate_existing_target_is_rejected_explicitly() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE AGGREGATE agg_alter_probe(int4) (stype = int4, sfunc = int4pl)",
        )
        .expect("create aggregate");

    let error = engine
        .execute_sql(
            &session,
            "ALTER AGGREGATE agg_alter_probe(int4) OWNER TO aiondb",
        )
        .expect_err("ALTER AGGREGATE should fail explicitly when target exists");
    assert_eq!(error.sqlstate(), aiondb_core::SqlState::FeatureNotSupported);
    assert!(error
        .report()
        .message
        .contains("unsupported compatibility command: ALTER AGGREGATE"));
}

#[test]
fn alter_procedure_is_rejected_explicitly() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let error = engine
        .execute_sql(
            &session,
            "ALTER PROCEDURE alter_proc_probe() OWNER TO aiondb",
        )
        .expect_err("ALTER PROCEDURE compat path must fail explicitly");
    assert_eq!(error.sqlstate(), aiondb_core::SqlState::FeatureNotSupported);
    assert!(error
        .report()
        .message
        .contains("unsupported compatibility command: ALTER PROCEDURE"));
}

#[test]
fn alter_schema_existing_target_is_rejected_explicitly() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(&session, "CREATE SCHEMA alter_schema_probe")
        .expect("create schema");

    let error = engine
        .execute_sql(&session, "ALTER SCHEMA alter_schema_probe OWNER TO aiondb")
        .expect_err("ALTER SCHEMA should fail explicitly when target exists");
    assert_eq!(error.sqlstate(), aiondb_core::SqlState::FeatureNotSupported);
    assert!(error
        .report()
        .message
        .contains("unsupported compatibility command: ALTER SCHEMA"));
}

#[test]
fn alter_sequence_existing_target_is_rejected_explicitly() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(&session, "CREATE SEQUENCE alter_seq_probe")
        .expect("create sequence");

    let error = engine
        .execute_sql(&session, "ALTER SEQUENCE alter_seq_probe OWNER TO aiondb")
        .expect_err("ALTER SEQUENCE should fail explicitly when target exists");
    assert_eq!(error.sqlstate(), aiondb_core::SqlState::FeatureNotSupported);
    assert!(error
        .report()
        .message
        .contains("unsupported compatibility command: ALTER SEQUENCE"));
}

#[test]
fn alter_index_owner_is_rejected_explicitly() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(&session, "CREATE TABLE alter_idx_owner_probe (id INT)")
        .expect("create table");

    let error = engine
        .execute_sql(
            &session,
            "ALTER INDEX alter_idx_owner_probe OWNER TO aiondb",
        )
        .expect_err("ALTER INDEX OWNER compat path must fail explicitly");
    assert_eq!(error.sqlstate(), aiondb_core::SqlState::FeatureNotSupported);
    assert!(error
        .report()
        .message
        .contains("unsupported compatibility command: ALTER INDEX"));
}

#[test]
fn alter_table_replica_identity_is_rejected_explicitly() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(&session, "CREATE TABLE alter_tbl_replica_probe (id INT)")
        .expect("create table");

    let error = engine
        .execute_sql(
            &session,
            "ALTER TABLE alter_tbl_replica_probe REPLICA IDENTITY FULL",
        )
        .expect_err("ALTER TABLE REPLICA IDENTITY compat path must fail explicitly");
    assert_eq!(error.sqlstate(), aiondb_core::SqlState::FeatureNotSupported);
    assert!(error
        .report()
        .message
        .contains("unsupported compatibility command: ALTER TABLE"));
}

#[test]
fn alter_table_owner_to_is_rejected_explicitly() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(&session, "CREATE TABLE alter_tbl_owner_probe (id INT)")
        .expect("create table");

    let error = engine
        .execute_sql(
            &session,
            "ALTER TABLE alter_tbl_owner_probe OWNER TO aiondb",
        )
        .expect_err("ALTER TABLE OWNER TO compat path must fail explicitly");
    assert_eq!(error.sqlstate(), aiondb_core::SqlState::FeatureNotSupported);
    assert!(error
        .report()
        .message
        .contains("unsupported compatibility command: ALTER TABLE"));
}

#[test]
fn alter_table_if_exists_missing_owner_to_is_rejected_explicitly() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let error = engine
        .execute_sql(
            &session,
            "ALTER TABLE IF EXISTS missing_tbl_owner_probe OWNER TO aiondb",
        )
        .expect_err("unsupported ALTER TABLE form must not be skipped by IF EXISTS");
    assert_eq!(error.sqlstate(), aiondb_core::SqlState::FeatureNotSupported);
    assert!(error
        .report()
        .message
        .contains("unsupported compatibility command: ALTER TABLE"));
}

#[test]
fn alter_table_without_name_is_syntax_error() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let error = engine
        .execute_sql(&session, "ALTER TABLE")
        .expect_err("ALTER TABLE without target name must fail");
    assert_eq!(error.sqlstate(), aiondb_core::SqlState::SyntaxError);
}

#[test]
fn alter_table_if_exists_missing_emits_notice() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let results = engine
        .execute_sql(
            &session,
            "ALTER TABLE IF EXISTS missing_tbl_notice ENABLE ROW LEVEL SECURITY",
        )
        .expect("IF EXISTS on missing table should skip with notice");
    assert_eq!(
        results,
        vec![
            StatementResult::Notice {
                message: "relation \"missing_tbl_notice\" does not exist, skipping".to_owned(),
            },
            StatementResult::Command {
                tag: "ALTER TABLE".to_owned(),
                rows_affected: 0,
            },
        ]
    );
}

#[test]
fn alter_table_if_exists_missing_with_trailing_tokens_is_syntax_error() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let error = engine
        .execute_sql(
            &session,
            "ALTER TABLE IF EXISTS missing_tbl_notice ENABLE ROW LEVEL SECURITY trailing",
        )
        .expect_err("trailing tokens must fail even when relation is missing");
    assert_eq!(error.sqlstate(), aiondb_core::SqlState::SyntaxError);
}

#[test]
fn alter_table_enable_rls_with_trailing_tokens_is_syntax_error() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(&session, "CREATE TABLE alter_tbl_tail_probe (id INT)")
        .expect("create table");

    let error = engine
        .execute_sql(
            &session,
            "ALTER TABLE alter_tbl_tail_probe ENABLE ROW LEVEL SECURITY trailing",
        )
        .expect_err("ALTER TABLE with trailing tokens must fail");
    assert_eq!(error.sqlstate(), aiondb_core::SqlState::SyntaxError);
}

#[test]
fn alter_view_owner_to_is_rejected_explicitly() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE TABLE alter_view_owner_base (id INT); \
             CREATE VIEW alter_view_owner_probe AS SELECT id FROM alter_view_owner_base",
        )
        .expect("create table/view");

    let error = engine
        .execute_sql(
            &session,
            "ALTER VIEW alter_view_owner_probe OWNER TO aiondb",
        )
        .expect_err("ALTER VIEW OWNER TO compat path must fail explicitly");
    assert_eq!(error.sqlstate(), aiondb_core::SqlState::FeatureNotSupported);
    assert!(error
        .report()
        .message
        .contains("unsupported compatibility command: ALTER VIEW"));
}

#[test]
fn alter_view_without_name_is_syntax_error() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let error = engine
        .execute_sql(&session, "ALTER VIEW")
        .expect_err("ALTER VIEW without target name must fail");
    assert_eq!(error.sqlstate(), aiondb_core::SqlState::SyntaxError);
    assert!(error
        .report()
        .message
        .contains("syntax error in ALTER VIEW"));
}

#[test]
fn alter_view_if_exists_missing_emits_notice() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let results = engine
        .execute_sql(
            &session,
            "ALTER VIEW IF EXISTS missing_view_notice SET (check_option = local)",
        )
        .expect("IF EXISTS on missing view should skip with notice");
    assert_eq!(
        results,
        vec![
            StatementResult::Notice {
                message: "view \"missing_view_notice\" does not exist, skipping".to_owned(),
            },
            StatementResult::Command {
                tag: "ALTER VIEW".to_owned(),
                rows_affected: 0,
            },
        ]
    );
}

#[test]
fn alter_view_if_exists_missing_owner_to_is_rejected_explicitly() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let error = engine
        .execute_sql(
            &session,
            "ALTER VIEW IF EXISTS missing_view_notice OWNER TO aiondb",
        )
        .expect_err("unsupported ALTER VIEW form must not be skipped by IF EXISTS");
    assert_eq!(error.sqlstate(), aiondb_core::SqlState::FeatureNotSupported);
    assert!(error
        .report()
        .message
        .contains("unsupported compatibility command: ALTER VIEW"));
}

#[test]
fn alter_view_if_exists_missing_with_trailing_tokens_is_syntax_error() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let error = engine
        .execute_sql(
            &session,
            "ALTER VIEW IF EXISTS missing_view_notice SET (check_option = local) trailing",
        )
        .expect_err("trailing tokens must fail even when view is missing");
    assert_eq!(error.sqlstate(), aiondb_core::SqlState::SyntaxError);
}

#[test]
fn alter_publication_is_rejected_explicitly() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let error = engine
        .execute_sql(&session, "ALTER PUBLICATION pub_probe OWNER TO aiondb")
        .expect_err("ALTER PUBLICATION compat path must fail explicitly");
    assert_eq!(error.sqlstate(), aiondb_core::SqlState::FeatureNotSupported);
    assert!(error
        .report()
        .message
        .contains("unsupported compatibility command: ALTER PUBLICATION"));
}

#[test]
fn alter_subscription_is_rejected_explicitly() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let error = engine
        .execute_sql(&session, "ALTER SUBSCRIPTION sub_probe OWNER TO aiondb")
        .expect_err("ALTER SUBSCRIPTION compat path must fail explicitly");
    assert_eq!(error.sqlstate(), aiondb_core::SqlState::FeatureNotSupported);
    assert!(error
        .report()
        .message
        .contains("unsupported compatibility command: ALTER SUBSCRIPTION"));
}

#[test]
fn alter_server_is_rejected_explicitly() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let error = engine
        .execute_sql(&session, "ALTER SERVER srv_probe OWNER TO aiondb")
        .expect_err("ALTER SERVER compat path must fail explicitly");
    assert_eq!(error.sqlstate(), aiondb_core::SqlState::FeatureNotSupported);
    assert!(error
        .report()
        .message
        .contains("unsupported compatibility command: ALTER SERVER"));
}

#[test]
fn alter_user_mapping_is_rejected_explicitly() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let error = engine
        .execute_sql(
            &session,
            "ALTER USER MAPPING FOR aiondb SERVER srv_probe OPTIONS (SET user 'x')",
        )
        .expect_err("ALTER USER MAPPING compat path must fail explicitly");
    assert_eq!(error.sqlstate(), aiondb_core::SqlState::FeatureNotSupported);
    assert!(error
        .report()
        .message
        .contains("unsupported compatibility command: ALTER USER MAPPING"));
}

#[test]
fn alter_foreign_data_wrapper_is_rejected_explicitly() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let error = engine
        .execute_sql(
            &session,
            "ALTER FOREIGN DATA WRAPPER fdw_probe OWNER TO aiondb",
        )
        .expect_err("ALTER FOREIGN DATA WRAPPER compat path must fail explicitly");
    assert_eq!(error.sqlstate(), aiondb_core::SqlState::FeatureNotSupported);
    assert!(error
        .report()
        .message
        .contains("unsupported compatibility command: ALTER FOREIGN DATA WRAPPER"));
}

#[test]
fn alter_collation_is_rejected_explicitly() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let error = engine
        .execute_sql(&session, "ALTER COLLATION coll_probe OWNER TO aiondb")
        .expect_err("ALTER COLLATION compat path must fail explicitly");
    assert_eq!(error.sqlstate(), aiondb_core::SqlState::FeatureNotSupported);
    assert!(error
        .report()
        .message
        .contains("unsupported compatibility command: ALTER COLLATION"));
}

#[test]
fn alter_text_search_set_schema_is_rejected_explicitly() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let error = engine
        .execute_sql(
            &session,
            "ALTER TEXT SEARCH CONFIGURATION ts_probe SET SCHEMA public",
        )
        .expect_err("ALTER TEXT SEARCH compat path must fail explicitly");
    assert_eq!(error.sqlstate(), aiondb_core::SqlState::FeatureNotSupported);
    assert!(error
        .report()
        .message
        .contains("unsupported compatibility command: ALTER TEXT SEARCH"));
}

#[test]
fn alter_extension_is_rejected_explicitly() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let error = engine
        .execute_sql(&session, "ALTER EXTENSION ext_probe UPDATE")
        .expect_err("ALTER EXTENSION compat path must fail explicitly");
    assert_eq!(error.sqlstate(), aiondb_core::SqlState::FeatureNotSupported);
    assert!(error
        .report()
        .message
        .contains("unsupported compatibility command: ALTER EXTENSION"));
}

#[test]
fn alter_operator_is_rejected_explicitly() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let error = engine
        .execute_sql(
            &session,
            "ALTER OPERATOR === (boolean, boolean) SET (RESTRICT = NONE)",
        )
        .expect_err("ALTER OPERATOR compat path must fail explicitly");
    assert_eq!(error.sqlstate(), aiondb_core::SqlState::FeatureNotSupported);
    assert!(error
        .report()
        .message
        .contains("unsupported compatibility command: ALTER OPERATOR"));
}

#[test]
fn alter_tablespace_is_rejected_explicitly() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let error = engine
        .execute_sql(&session, "ALTER TABLESPACE tsp_probe OWNER TO aiondb")
        .expect_err("ALTER TABLESPACE compat path must fail explicitly");
    assert_eq!(error.sqlstate(), aiondb_core::SqlState::FeatureNotSupported);
    assert!(error
        .report()
        .message
        .contains("unsupported compatibility command: ALTER TABLESPACE"));
}

#[test]
fn drop_tablespace_is_rejected_explicitly() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let error = engine
        .execute_sql(&session, "DROP TABLESPACE tsp_probe")
        .expect_err("DROP TABLESPACE compat path must fail explicitly");
    assert_eq!(error.sqlstate(), aiondb_core::SqlState::FeatureNotSupported);
    assert!(error
        .report()
        .message
        .contains("unsupported compatibility command: DROP TABLESPACE"));
}

#[test]
fn drop_collation_is_rejected_explicitly() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let error = engine
        .execute_sql(&session, "DROP COLLATION coll_probe")
        .expect_err("DROP COLLATION compat path must fail explicitly");
    assert_eq!(error.sqlstate(), aiondb_core::SqlState::FeatureNotSupported);
    assert!(error
        .report()
        .message
        .contains("unsupported compatibility command: DROP COLLATION"));
}

#[test]
fn drop_statistics_is_rejected_explicitly() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let error = engine
        .execute_sql(&session, "DROP STATISTICS stx_probe")
        .expect_err("DROP STATISTICS compat path must fail explicitly");
    assert_eq!(error.sqlstate(), aiondb_core::SqlState::FeatureNotSupported);
    assert!(error
        .report()
        .message
        .contains("unsupported compatibility command: DROP STATISTICS"));
}

#[test]
fn drop_user_mapping_is_rejected_explicitly() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let error = engine
        .execute_sql(&session, "DROP USER MAPPING FOR aiondb SERVER srv_probe")
        .expect_err("DROP USER MAPPING compat path must fail explicitly");
    assert_eq!(error.sqlstate(), aiondb_core::SqlState::FeatureNotSupported);
    assert!(error
        .report()
        .message
        .contains("unsupported compatibility command: DROP USER MAPPING"));
}

#[test]
fn drop_server_is_rejected_explicitly() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let error = engine
        .execute_sql(&session, "DROP SERVER srv_probe")
        .expect_err("DROP SERVER compat path must fail explicitly");
    assert_eq!(error.sqlstate(), aiondb_core::SqlState::FeatureNotSupported);
    assert!(error
        .report()
        .message
        .contains("unsupported compatibility command: DROP SERVER"));
}

#[test]
fn drop_publication_is_rejected_explicitly() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let error = engine
        .execute_sql(&session, "DROP PUBLICATION pub_probe")
        .expect_err("DROP PUBLICATION compat path must fail explicitly");
    assert_eq!(error.sqlstate(), aiondb_core::SqlState::FeatureNotSupported);
    assert!(error
        .report()
        .message
        .contains("unsupported compatibility command: DROP PUBLICATION"));
}

#[test]
fn drop_subscription_is_rejected_explicitly() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let error = engine
        .execute_sql(&session, "DROP SUBSCRIPTION sub_probe")
        .expect_err("DROP SUBSCRIPTION compat path must fail explicitly");
    assert_eq!(error.sqlstate(), aiondb_core::SqlState::FeatureNotSupported);
    assert!(error
        .report()
        .message
        .contains("unsupported compatibility command: DROP SUBSCRIPTION"));
}

#[test]
fn drop_foreign_data_wrapper_is_rejected_explicitly() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let error = engine
        .execute_sql(&session, "DROP FOREIGN DATA WRAPPER fdw_probe")
        .expect_err("DROP FOREIGN DATA WRAPPER compat path must fail explicitly");
    assert_eq!(error.sqlstate(), aiondb_core::SqlState::FeatureNotSupported);
    assert!(error
        .report()
        .message
        .contains("unsupported compatibility command: DROP FOREIGN DATA WRAPPER"));
}

#[test]
fn alter_materialized_view_is_rejected_explicitly() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let error = engine
        .execute_sql(
            &session,
            "ALTER MATERIALIZED VIEW mv_alter_probe OWNER TO aiondb",
        )
        .expect_err("ALTER MATERIALIZED VIEW compat path must fail explicitly");
    assert_eq!(error.sqlstate(), aiondb_core::SqlState::FeatureNotSupported);
    assert!(error
        .report()
        .message
        .contains("unsupported compatibility command: ALTER MATERIALIZED"));
}

#[test]
fn alter_trigger_compat_form_is_rejected_explicitly() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let error = engine
        .execute_sql(&session, "ALTER TRIGGER trg_probe ON tbl_probe ENABLE")
        .expect_err("ALTER TRIGGER compat form must fail explicitly");
    assert_eq!(error.sqlstate(), aiondb_core::SqlState::FeatureNotSupported);
    assert!(error
        .report()
        .message
        .contains("unsupported compatibility command: ALTER TRIGGER"));
}

#[test]
fn alter_default_privileges_is_rejected_explicitly() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let error = engine
        .execute_sql(
            &session,
            "ALTER DEFAULT PRIVILEGES FOR ROLE aiondb \
             GRANT SELECT ON TABLES TO aiondb",
        )
        .expect_err("ALTER DEFAULT PRIVILEGES compat path must fail explicitly");
    assert_eq!(error.sqlstate(), aiondb_core::SqlState::FeatureNotSupported);
    assert!(error
        .report()
        .message
        .contains("unsupported compatibility command: ALTER DEFAULT"));
}

#[test]
fn compat_create_rule_is_persistent_and_duplicate_is_rejected() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(&session, "CREATE TABLE rule_insert_target (id INT)")
        .expect("create rule target relation");

    let first = engine
        .execute_sql(
            &session,
            "CREATE RULE r_insert AS ON INSERT TO rule_insert_target DO INSTEAD NOTHING",
        )
        .expect("first CREATE RULE should succeed");
    assert_eq!(
        first,
        vec![StatementResult::Command {
            tag: "CREATE RULE".to_owned(),
            rows_affected: 0,
        }]
    );

    let duplicate = engine
        .execute_sql(
            &session,
            "CREATE RULE r_insert AS ON INSERT TO rule_insert_target DO INSTEAD NOTHING",
        )
        .expect_err("duplicate rule should fail");
    assert_eq!(duplicate.sqlstate(), aiondb_core::SqlState::DuplicateObject);
    assert!(duplicate
        .report()
        .message
        .contains("rule \"r_insert\" already exists"));
}

#[test]
fn compat_create_rule_missing_relation_is_undefined_table() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let error = engine
        .execute_sql(
            &session,
            "CREATE RULE r_missing_rel AS ON INSERT TO missing_tbl DO INSTEAD NOTHING",
        )
        .expect_err("CREATE RULE on missing relation must fail");
    assert_eq!(error.sqlstate(), aiondb_core::SqlState::UndefinedTable);
}

#[test]
fn compat_create_rule_with_invalid_body_is_syntax_error() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let error = engine
        .execute_sql(&session, "CREATE RULE r_bad AS ON INSERT TO missing_tbl")
        .expect_err("malformed CREATE RULE must fail");
    assert_eq!(error.sqlstate(), aiondb_core::SqlState::SyntaxError);
}

#[test]
fn compat_create_rule_with_trailing_tokens_is_syntax_error() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let error = engine
        .execute_sql(
            &session,
            "CREATE RULE r_tail AS ON INSERT TO missing_tbl DO INSTEAD NOTHING trailing",
        )
        .expect_err("CREATE RULE with trailing tokens must fail");
    assert_eq!(error.sqlstate(), aiondb_core::SqlState::SyntaxError);
}

#[test]
fn compat_create_or_replace_rule_with_trailing_tokens_is_syntax_error() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let error = engine
        .execute_sql(
            &session,
            "CREATE OR REPLACE RULE r_tail2 AS ON INSERT TO missing_tbl DO INSTEAD NOTHING trailing",
        )
        .expect_err("CREATE OR REPLACE RULE with trailing tokens must fail");
    assert_eq!(error.sqlstate(), aiondb_core::SqlState::SyntaxError);
}

#[test]
fn compat_drop_rule_validates_existence_and_if_exists_notice() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let missing = engine
        .execute_sql(&session, "DROP RULE r_missing")
        .expect_err("dropping unknown rule without IF EXISTS should fail");
    assert_eq!(missing.sqlstate(), aiondb_core::SqlState::UndefinedObject);
    assert!(missing
        .report()
        .message
        .contains("rule \"r_missing\" does not exist"));

    let if_exists = engine
        .execute_sql(&session, "DROP RULE IF EXISTS r_missing")
        .expect("IF EXISTS should be accepted");
    assert_eq!(
        if_exists,
        vec![
            StatementResult::Notice {
                message: "rule \"r_missing\" does not exist, skipping".to_owned(),
            },
            StatementResult::Command {
                tag: "DROP RULE".to_owned(),
                rows_affected: 0,
            },
        ]
    );
}

#[test]
fn compat_drop_rule_if_exists_with_trailing_tokens_is_syntax_error() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let error = engine
        .execute_sql(&session, "DROP RULE IF EXISTS r_missing trailing")
        .expect_err("DROP RULE with trailing tokens must fail");
    assert_eq!(error.sqlstate(), aiondb_core::SqlState::SyntaxError);
    assert!(error.report().message.contains("syntax error in DROP RULE"));
}

#[test]
fn compat_drop_rule_on_target_with_trailing_tokens_is_syntax_error() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let error = engine
        .execute_sql(
            &session,
            "DROP RULE IF EXISTS r_missing ON missing_tbl trailing",
        )
        .expect_err("DROP RULE ON ... with trailing tokens must fail");
    assert_eq!(error.sqlstate(), aiondb_core::SqlState::SyntaxError);
    assert!(error.report().message.contains("syntax error in DROP RULE"));
}

#[test]
fn compat_alter_rule_non_rename_is_rejected_explicitly() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let error = engine
        .execute_sql(&session, "ALTER RULE r_missing ON missing_tbl")
        .expect_err("unsupported ALTER RULE form must fail explicitly");
    assert_eq!(error.sqlstate(), aiondb_core::SqlState::FeatureNotSupported);
    assert!(error
        .report()
        .message
        .contains("unsupported compatibility command: ALTER RULE"));
}

#[test]
fn compat_alter_rule_if_exists_is_rejected_explicitly() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let error = engine
        .execute_sql(
            &session,
            "ALTER RULE IF EXISTS r_missing ON missing_tbl RENAME TO r2",
        )
        .expect_err("ALTER RULE IF EXISTS must fail explicitly");
    assert_eq!(error.sqlstate(), aiondb_core::SqlState::FeatureNotSupported);
    assert!(error
        .report()
        .message
        .contains("unsupported compatibility command: ALTER RULE"));
}

#[test]
fn compat_alter_rule_rename_with_trailing_tokens_is_rejected_explicitly() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let error = engine
        .execute_sql(
            &session,
            "ALTER RULE r_missing ON missing_tbl RENAME TO r2 trailing",
        )
        .expect_err("ALTER RULE with trailing tokens must fail explicitly");
    assert_eq!(error.sqlstate(), aiondb_core::SqlState::FeatureNotSupported);
    assert!(error
        .report()
        .message
        .contains("unsupported compatibility command: ALTER RULE"));
}

#[test]
fn alter_foreign_table_is_rejected_explicitly() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let error = engine
        .execute_sql(
            &session,
            "ALTER FOREIGN TABLE IF EXISTS missing_ft_ifx OWNER TO alice",
        )
        .expect_err("ALTER FOREIGN TABLE compat path must fail explicitly");
    assert_eq!(error.sqlstate(), aiondb_core::SqlState::FeatureNotSupported);
    assert!(error
        .report()
        .message
        .contains("unsupported compatibility command: ALTER FOREIGN TABLE"));
}

#[test]
fn drop_foreign_table_is_rejected_explicitly() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let error = engine
        .execute_sql(&session, "DROP FOREIGN TABLE IF EXISTS missing_drop_ifx")
        .expect_err("DROP FOREIGN TABLE compat path must fail explicitly");
    assert_eq!(error.sqlstate(), aiondb_core::SqlState::FeatureNotSupported);
    assert!(error
        .report()
        .message
        .contains("unsupported compatibility command: DROP FOREIGN TABLE"));
}

#[test]
fn alter_policy_using_updates_predicate_for_owner() {
    // ALTER POLICY <name> ON <table> USING (...) is now a real
    // mutation against the in-session policy registry (and persisted
    // to the catalog by `persist_compat_policy_ddl`). The pre-existing
    // expectation that this rejected with FeatureNotSupported was
    // handled when ALTER POLICY USING/WITH CHECK shipped; see
    // `engine::tests::functions::alter_policy_using_update_changes_visibility_filter`
    // for the behavioural cross-check.
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE TABLE alter_policy_probe (id INT); \
             CREATE POLICY p1 ON alter_policy_probe USING (true)",
        )
        .expect("setup policy");

    engine
        .execute_sql(
            &session,
            "ALTER POLICY p1 ON alter_policy_probe USING (id > 0)",
        )
        .expect("ALTER POLICY USING is a real catalog mutation now");
}

#[test]
fn alter_policy_without_on_clause_is_syntax_error() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let error = engine
        .execute_sql(&session, "ALTER POLICY p_missing")
        .expect_err("ALTER POLICY without ON <table> must fail with syntax");
    assert_eq!(error.sqlstate(), aiondb_core::SqlState::SyntaxError);
}

#[test]
fn alter_policy_if_exists_is_syntax_error() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let error = engine
        .execute_sql(
            &session,
            "ALTER POLICY IF EXISTS p_missing ON alter_policy_if_exists_tbl RENAME TO p2",
        )
        .expect_err("ALTER POLICY IF EXISTS must fail with syntax error");
    assert_eq!(error.sqlstate(), aiondb_core::SqlState::SyntaxError);
    assert!(error
        .report()
        .message
        .contains("syntax error in ALTER POLICY"));
}

#[test]
fn alter_policy_rename_with_trailing_tokens_is_syntax_error() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE TABLE alter_policy_tail_tbl (id INT); \
             CREATE POLICY p_tail ON alter_policy_tail_tbl USING (true)",
        )
        .expect("setup policy");

    let error = engine
        .execute_sql(
            &session,
            "ALTER POLICY p_tail ON alter_policy_tail_tbl RENAME TO p_tail2 trailing",
        )
        .expect_err("ALTER POLICY RENAME with trailing tokens must fail");
    assert_eq!(error.sqlstate(), aiondb_core::SqlState::SyntaxError);
}

#[test]
fn alter_statistics_without_target_is_syntax_error() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let error = engine
        .execute_sql(&session, "ALTER STATISTICS")
        .expect_err("ALTER STATISTICS without target must fail with syntax");
    assert_eq!(error.sqlstate(), aiondb_core::SqlState::SyntaxError);
}

#[test]
fn alter_statistics_set_tablespace_is_rejected_explicitly() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE TABLE alter_stats_probe (a INT, b INT); \
             CREATE STATISTICS stx_probe ON a, b FROM alter_stats_probe",
        )
        .expect("setup statistics object");

    let error = engine
        .execute_sql(
            &session,
            "ALTER STATISTICS stx_probe SET TABLESPACE pg_default",
        )
        .expect_err("ALTER STATISTICS SET TABLESPACE must fail explicitly");
    assert_eq!(error.sqlstate(), aiondb_core::SqlState::FeatureNotSupported);
    assert!(error
        .report()
        .message
        .contains("unsupported compatibility command: ALTER STATISTICS"));
}

#[test]
fn alter_statistics_if_exists_missing_emits_notice() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let results = engine
        .execute_sql(
            &session,
            "ALTER STATISTICS IF EXISTS missing_stats_notice SET STATISTICS 0",
        )
        .expect("IF EXISTS on missing statistics should skip with notice");
    assert_eq!(
        results,
        vec![
            StatementResult::Notice {
                message: "statistics object \"missing_stats_notice\" does not exist, skipping"
                    .to_owned(),
            },
            StatementResult::Command {
                tag: "ALTER STATISTICS".to_owned(),
                rows_affected: 0,
            },
        ]
    );
}

#[test]
fn alter_statistics_if_exists_missing_with_trailing_tokens_is_syntax_error() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let error = engine
        .execute_sql(
            &session,
            "ALTER STATISTICS IF EXISTS missing_stats_notice SET STATISTICS 0 trailing",
        )
        .expect_err("trailing tokens must fail even when statistics object is missing");
    assert_eq!(error.sqlstate(), aiondb_core::SqlState::SyntaxError);
}

#[test]
fn alter_statistics_if_exists_missing_set_tablespace_is_rejected_explicitly() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let error = engine
        .execute_sql(
            &session,
            "ALTER STATISTICS IF EXISTS missing_stats_notice SET TABLESPACE pg_default",
        )
        .expect_err("unsupported ALTER STATISTICS form must not be skipped by IF EXISTS");
    assert_eq!(error.sqlstate(), aiondb_core::SqlState::FeatureNotSupported);
    assert!(error
        .report()
        .message
        .contains("unsupported compatibility command: ALTER STATISTICS"));
}

#[test]
fn drop_policy_without_on_clause_is_syntax_error() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let error = engine
        .execute_sql(&session, "DROP POLICY IF EXISTS p_missing")
        .expect_err("DROP POLICY without ON <table> must fail with syntax error");
    assert_eq!(error.sqlstate(), aiondb_core::SqlState::SyntaxError);
    assert!(error
        .report()
        .message
        .contains("syntax error in DROP POLICY"));
}

#[test]
fn drop_policy_if_exists_missing_emits_notice() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let results = engine
        .execute_sql(
            &session,
            "DROP POLICY IF EXISTS p_missing_drop_notice ON missing_tbl_drop_notice",
        )
        .expect("DROP POLICY IF EXISTS missing should skip with notice");
    assert_eq!(
        results,
        vec![
            StatementResult::Notice {
                message: "policy \"p_missing_drop_notice\" for table \"missing_tbl_drop_notice\" does not exist, skipping".to_owned(),
            },
            StatementResult::Command {
                tag: "DROP POLICY".to_owned(),
                rows_affected: 0,
            },
        ]
    );
}

#[test]
fn drop_policy_with_trailing_tokens_is_syntax_error() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE TABLE drop_policy_tail_tbl (id INT); \
             CREATE POLICY p_drop_tail ON drop_policy_tail_tbl USING (true)",
        )
        .expect("setup policy");

    let error = engine
        .execute_sql(
            &session,
            "DROP POLICY p_drop_tail ON drop_policy_tail_tbl trailing",
        )
        .expect_err("DROP POLICY with trailing tokens must fail");
    assert_eq!(error.sqlstate(), aiondb_core::SqlState::SyntaxError);
}

#[test]
fn alter_index_does_not_rename_table_when_name_matches() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(&session, "CREATE TABLE same_name_obj (id INT)")
        .expect("create table");

    let error = engine
        .execute_sql(
            &session,
            "ALTER INDEX same_name_obj RENAME TO same_name_obj_new",
        )
        .expect_err("ALTER INDEX on table name must fail");
    assert_eq!(error.sqlstate(), aiondb_core::SqlState::WrongObjectType);
    assert!(error.report().message.contains("is not an index"));

    engine
        .execute_sql(&session, "INSERT INTO same_name_obj VALUES (1)")
        .expect("table should not be renamed by ALTER INDEX failure");
}

#[test]
fn alter_index_if_exists_missing_emits_notice() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let results = engine
        .execute_sql(
            &session,
            "ALTER INDEX IF EXISTS missing_idx_notice RENAME TO new_idx",
        )
        .expect("IF EXISTS missing index should not fail");
    assert_eq!(
        results,
        vec![
            StatementResult::Notice {
                message: "index \"missing_idx_notice\" does not exist, skipping".to_owned(),
            },
            StatementResult::Command {
                tag: "ALTER INDEX".to_owned(),
                rows_affected: 0,
            },
        ]
    );
}

#[test]
fn alter_index_if_exists_missing_owner_to_is_rejected_explicitly() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let error = engine
        .execute_sql(
            &session,
            "ALTER INDEX IF EXISTS missing_idx_notice OWNER TO aiondb",
        )
        .expect_err("unsupported ALTER INDEX form must not be skipped by IF EXISTS");
    assert_eq!(error.sqlstate(), aiondb_core::SqlState::FeatureNotSupported);
    assert!(error
        .report()
        .message
        .contains("unsupported compatibility command: ALTER INDEX"));
}

#[test]
fn alter_index_if_exists_missing_with_trailing_tokens_is_syntax_error() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let error = engine
        .execute_sql(
            &session,
            "ALTER INDEX IF EXISTS missing_idx_notice RENAME TO new_idx trailing",
        )
        .expect_err("trailing tokens must fail even when target index is missing");
    assert_eq!(error.sqlstate(), aiondb_core::SqlState::SyntaxError);
}

#[test]
fn alter_index_if_exists_on_table_name_still_errors_wrong_type() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(&session, "CREATE TABLE idx_wrong_type_guard (id INT)")
        .expect("create table");

    let error = engine
        .execute_sql(
            &session,
            "ALTER INDEX IF EXISTS idx_wrong_type_guard RENAME TO idx_should_not_exist",
        )
        .expect_err("IF EXISTS should not hide wrong-object-type errors");
    assert_eq!(error.sqlstate(), aiondb_core::SqlState::WrongObjectType);
    assert!(error.report().message.contains("is not an index"));
}

#[test]
fn alter_index_without_name_is_syntax_error() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let error = engine
        .execute_sql(&session, "ALTER INDEX")
        .expect_err("ALTER INDEX without target name must fail");
    assert_eq!(error.sqlstate(), aiondb_core::SqlState::SyntaxError);
    assert!(error
        .report()
        .message
        .contains("syntax error in ALTER INDEX"));
}

#[test]
fn compat_execute_supports_prepared_cursor_commands() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE TABLE compat_exec_cursor (id INT);
             INSERT INTO compat_exec_cursor VALUES (1), (2)",
        )
        .expect("seed cursor table");
    engine.execute_sql(&session, "BEGIN").expect("begin");

    engine
        .execute_sql(
            &session,
            "PREPARE decl AS DECLARE c CURSOR FOR SELECT id FROM compat_exec_cursor ORDER BY id",
        )
        .expect("prepare declare cursor");
    let declare_results = engine
        .execute_sql(&session, "EXECUTE decl")
        .expect("execute prepared declare cursor");
    assert_eq!(
        declare_results,
        vec![StatementResult::Command {
            tag: "DECLARE CURSOR".to_owned(),
            rows_affected: 0,
        }]
    );

    engine
        .execute_sql(&session, "PREPARE fetch_stmt AS FETCH ALL IN c")
        .expect("prepare fetch cursor");
    let fetch_results = engine
        .execute_sql(&session, "EXECUTE fetch_stmt")
        .expect("execute prepared fetch cursor");
    let [StatementResult::Query { rows, .. }] = fetch_results.as_slice() else {
        panic!("expected fetch query result, got {fetch_results:?}");
    };
    assert_eq!(
        rows,
        &vec![
            aiondb_core::Row::new(vec![aiondb_core::Value::Int(1)]),
            aiondb_core::Row::new(vec![aiondb_core::Value::Int(2)]),
        ]
    );

    engine
        .execute_sql(&session, "PREPARE close_stmt AS CLOSE c")
        .expect("prepare close cursor");
    let close_results = engine
        .execute_sql(&session, "EXECUTE close_stmt")
        .expect("execute prepared close cursor");
    assert_eq!(
        close_results,
        vec![StatementResult::Command {
            tag: "CLOSE CURSOR".to_owned(),
            rows_affected: 0,
        }]
    );
    engine.execute_sql(&session, "COMMIT").expect("commit");
}

#[test]
fn compat_execute_emits_post_statement_compat_notices() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE TYPE casttesttype;
             CREATE CAST (text AS casttesttype) WITHOUT FUNCTION;
             CREATE FUNCTION int4_casttesttype(int4) RETURNS casttesttype LANGUAGE SQL AS
             $$ SELECT ('foo'::text || $1::text)::casttesttype; $$;
             CREATE CAST (int4 AS casttesttype) WITH FUNCTION int4_casttesttype(int4) AS IMPLICIT;",
        )
        .expect("create compat cast dependency");

    engine
        .execute_sql(
            &session,
            "PREPARE stmt AS DROP FUNCTION int4_casttesttype(int4) CASCADE",
        )
        .expect("prepare drop function cascade");

    let results = engine
        .execute_sql(&session, "EXECUTE stmt")
        .expect("execute prepared drop function cascade");
    assert!(
        matches!(
            results.first(),
            Some(StatementResult::Notice { message })
                if message == "drop cascades to cast from integer to casttesttype"
        ),
        "expected compat notice in results, got {results:?}"
    );
    assert!(
        matches!(
            results.last(),
            Some(StatementResult::Command { tag, rows_affected })
                if tag == "DROP FUNCTION" && *rows_affected == 0
        ),
        "expected DROP FUNCTION command result, got {results:?}"
    );
}
