use super::*;

#[test]
fn updates_rows_using_column_defaults() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let results = engine
        .execute_sql(
            &session,
            "CREATE SEQUENCE user_ids; \
             CREATE TABLE users (id BIGINT NOT NULL DEFAULT nextval('user_ids'), name TEXT NOT NULL DEFAULT 'anon'); \
             INSERT INTO users VALUES (42, 'custom'); \
             UPDATE users SET id = DEFAULT, name = DEFAULT; \
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
            StatementResult::Command {
                tag: "UPDATE".to_owned(),
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
fn drops_index_and_allows_recreation() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let results = engine
        .execute_sql(
            &session,
            "CREATE TABLE users (id INT); \
             CREATE INDEX users_id_idx ON users (id); \
             DROP INDEX users_id_idx; \
             CREATE INDEX users_id_idx ON users (id)",
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
                tag: "CREATE INDEX".to_owned(),
                rows_affected: 0,
            },
            StatementResult::Command {
                tag: "DROP INDEX".to_owned(),
                rows_affected: 0,
            },
            StatementResult::Command {
                tag: "CREATE INDEX".to_owned(),
                rows_affected: 0,
            },
        ]
    );
}

#[test]
fn drops_table_and_clears_dependent_indexes() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let results = engine
        .execute_sql(
            &session,
            "CREATE TABLE users (id INT); \
             CREATE INDEX users_id_idx ON users (id); \
             DROP TABLE users; \
             CREATE TABLE users (id INT); \
             CREATE INDEX users_id_idx ON users (id)",
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
                tag: "CREATE INDEX".to_owned(),
                rows_affected: 0,
            },
            StatementResult::Command {
                tag: "DROP TABLE".to_owned(),
                rows_affected: 0,
            },
            StatementResult::Command {
                tag: "CREATE TABLE".to_owned(),
                rows_affected: 0,
            },
            StatementResult::Command {
                tag: "CREATE INDEX".to_owned(),
                rows_affected: 0,
            },
        ]
    );
}

#[test]
fn create_table_duplicate_without_if_not_exists_errors() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(&session, "CREATE TABLE users (id INT)")
        .expect("initial create");

    let err = engine
        .execute_sql(&session, "CREATE TABLE users (id INT)")
        .expect_err("duplicate CREATE TABLE must fail");
    assert!(
        err.to_string().contains("already exists"),
        "unexpected error: {err}"
    );
}

#[test]
fn filters_select_without_from() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let results = engine
        .execute_sql(&session, "SELECT 1 AS one WHERE FALSE")
        .expect("execute");
    assert_eq!(
        results,
        vec![StatementResult::Query {
            columns: vec![ResultColumn {
                name: "one".to_owned(),
                data_type: aiondb_core::DataType::Int,
                text_type_modifier: None,
                nullable: false,
            }],
            rows: Vec::new(),
        }]
    );
}

#[test]
fn honors_logical_operator_precedence_in_where() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let results = engine
        .execute_sql(
            &session,
            "CREATE TABLE users (id INT, name TEXT); \
             INSERT INTO users VALUES (1, 'alice'), (2, 'bob'), (3, 'bob'); \
             SELECT id, name FROM users WHERE id >= 2 AND name = 'bob' OR id < 2",
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
fn honors_parenthesized_where_expression() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let results = engine
        .execute_sql(&session, "SELECT 1 AS one WHERE (TRUE OR FALSE) AND FALSE")
        .expect("execute");
    assert_eq!(
        results,
        vec![StatementResult::Query {
            columns: vec![ResultColumn {
                name: "one".to_owned(),
                data_type: aiondb_core::DataType::Int,
                text_type_modifier: None,
                nullable: false,
            }],
            rows: Vec::new(),
        }]
    );
}

#[test]
fn explain_analyze_select_reports_query_summary() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let results = engine
        .execute_sql(&session, "EXPLAIN ANALYZE SELECT 1 AS one")
        .expect("execute");
    let [StatementResult::Query { columns, rows }] = results.as_slice() else {
        panic!("expected explain query result");
    };

    assert_eq!(
        columns,
        &[ResultColumn {
            name: "QUERY PLAN".to_owned(),
            data_type: aiondb_core::DataType::Text,
            text_type_modifier: None,
            nullable: false,
        }]
    );

    let lines: Vec<&str> = rows
        .iter()
        .map(|row| {
            let [aiondb_core::Value::Text(line)] = row.values.as_slice() else {
                panic!("expected explain text row");
            };
            line.as_str()
        })
        .collect();

    assert!(lines.contains(&"Result"));
    assert!(lines.contains(&"Execution: Query"));
    assert!(lines.contains(&"Rows Returned: 1"));
    assert!(lines
        .iter()
        .any(|line| line.starts_with("Memory Used: ") && line.ends_with(" bytes")));
}

#[test]
fn explain_analyze_insert_executes_command() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(&session, "CREATE TABLE users (id INT)")
        .expect("create table");

    let results = engine
        .execute_sql(&session, "EXPLAIN ANALYZE INSERT INTO users VALUES (1)")
        .expect("execute");
    let [StatementResult::Query { rows, .. }] = results.as_slice() else {
        panic!("expected explain query result");
    };

    let lines: Vec<&str> = rows
        .iter()
        .map(|row| {
            let [aiondb_core::Value::Text(line)] = row.values.as_slice() else {
                panic!("expected explain text row");
            };
            line.as_str()
        })
        .collect();

    assert!(lines.iter().any(|line| line.starts_with("Insert on ")));
    assert!(lines.contains(&"Execution: Command (INSERT)"));
    assert!(lines.contains(&"Rows Affected: 1"));
    assert!(lines
        .iter()
        .any(|line| line.starts_with("Memory Used: ") && line.ends_with(" bytes")));

    let verify = engine
        .execute_sql(&session, "SELECT id FROM users")
        .expect("verify");
    assert_eq!(
        verify,
        vec![StatementResult::Query {
            columns: vec![ResultColumn {
                name: "id".to_owned(),
                data_type: aiondb_core::DataType::Int,
                text_type_modifier: None,
                nullable: true,
            }],
            rows: vec![aiondb_core::Row::new(vec![aiondb_core::Value::Int(1)])],
        }]
    );
}

// ═══════════════════════════════════════════════════════════════
//  DDL CONSTRAINT TESTS
// ═══════════════════════════════════════════════════════════════

#[test]
fn create_table_with_primary_key_succeeds() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let results = engine
        .execute_sql(&session, "CREATE TABLE t (id INT PRIMARY KEY, name TEXT)")
        .expect("create");
    assert_eq!(
        results,
        vec![StatementResult::Command {
            tag: "CREATE TABLE".to_owned(),
            rows_affected: 0,
        }]
    );

    // Verify we can still insert and query
    engine
        .execute_sql(&session, "INSERT INTO t VALUES (1, 'a')")
        .expect("insert");
    let rows = engine
        .execute_sql(&session, "SELECT id FROM t")
        .expect("select");
    assert!(matches!(
        &rows[0],
        StatementResult::Query { rows, .. } if rows.len() == 1
    ));
}

#[test]
fn create_table_with_composite_primary_key_succeeds() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE TABLE t (a INT, b INT, c TEXT, PRIMARY KEY (a, b))",
        )
        .expect("create");
    engine
        .execute_sql(&session, "INSERT INTO t VALUES (1, 2, 'hello')")
        .expect("insert");
    let rows = engine
        .execute_sql(&session, "SELECT c FROM t")
        .expect("select");
    assert!(matches!(
        &rows[0],
        StatementResult::Query { rows, .. } if rows.len() == 1
    ));
}

#[test]
fn create_table_with_unique_constraint_succeeds() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(&session, "CREATE TABLE t (id INT, email TEXT UNIQUE)")
        .expect("create");
    engine
        .execute_sql(&session, "INSERT INTO t VALUES (1, 'a@b.com')")
        .expect("insert");
    let rows = engine
        .execute_sql(&session, "SELECT email FROM t")
        .expect("select");
    assert!(matches!(
        &rows[0],
        StatementResult::Query { rows, .. } if rows.len() == 1
    ));
}

#[test]
fn create_table_with_check_constraint_succeeds() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE TABLE t (id INT, age INT, CHECK (age > 0))",
        )
        .expect("create");
    engine
        .execute_sql(&session, "INSERT INTO t VALUES (1, 25)")
        .expect("insert");
}

#[test]
fn create_table_with_foreign_key_succeeds() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(&session, "CREATE TABLE users (id INT PRIMARY KEY)")
        .expect("create users");
    engine
        .execute_sql(
            &session,
            "CREATE TABLE orders (id INT, user_id INT, FOREIGN KEY (user_id) REFERENCES users (id))",
        )
        .expect("create orders");
}

// ═══════════════════════════════════════════════════════════════
//  ALTER TABLE EXTENDED TESTS
// ═══════════════════════════════════════════════════════════════

#[test]
fn alter_table_rename_succeeds() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let results = engine
        .execute_sql(
            &session,
            "CREATE TABLE old_name (id INT); \
             INSERT INTO old_name VALUES (1); \
             ALTER TABLE old_name RENAME TO new_name; \
             SELECT id FROM new_name",
        )
        .expect("execute");

    assert_eq!(results.len(), 4);
    assert_eq!(
        results[2],
        StatementResult::Command {
            tag: "ALTER TABLE".to_owned(),
            rows_affected: 0,
        }
    );
    assert!(matches!(
        &results[3],
        StatementResult::Query { rows, .. } if rows.len() == 1
    ));

    // Old name should no longer be valid
    let err = engine
        .execute_sql(&session, "SELECT id FROM old_name")
        .unwrap_err();
    assert!(err.to_string().contains("does not exist"));
}

#[test]
fn alter_table_rename_column_succeeds() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let results = engine
        .execute_sql(
            &session,
            "CREATE TABLE t (old_col INT); \
             INSERT INTO t VALUES (42); \
             ALTER TABLE t RENAME COLUMN old_col TO new_col; \
             SELECT new_col FROM t",
        )
        .expect("execute");

    assert_eq!(results.len(), 4);
    assert_eq!(
        results[2],
        StatementResult::Command {
            tag: "ALTER TABLE".to_owned(),
            rows_affected: 0,
        }
    );
    assert!(matches!(
        &results[3],
        StatementResult::Query { rows, columns } if rows.len() == 1 && columns[0].name == "new_col"
    ));
}

#[test]
fn alter_table_set_default_succeeds() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let results = engine
        .execute_sql(
            &session,
            "CREATE TABLE t (id INT, name TEXT); \
             ALTER TABLE t ALTER COLUMN name SET DEFAULT 'hello'; \
             INSERT INTO t (id) VALUES (1); \
             SELECT id, name FROM t",
        )
        .expect("execute");

    assert_eq!(results.len(), 4);
    assert_eq!(
        results[1],
        StatementResult::Command {
            tag: "ALTER TABLE".to_owned(),
            rows_affected: 0,
        }
    );
    assert!(matches!(
        &results[3],
        StatementResult::Query { rows, .. } if rows.len() == 1
            && rows[0].values[1] == aiondb_core::Value::Text("hello".to_owned())
    ));
}

#[test]
fn alter_table_drop_default_succeeds() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let results = engine
        .execute_sql(
            &session,
            "CREATE TABLE t (id INT, name TEXT DEFAULT 'world'); \
             ALTER TABLE t ALTER COLUMN name DROP DEFAULT; \
             INSERT INTO t (id) VALUES (1); \
             SELECT id, name FROM t",
        )
        .expect("execute");

    assert_eq!(results.len(), 4);
    assert_eq!(
        results[1],
        StatementResult::Command {
            tag: "ALTER TABLE".to_owned(),
            rows_affected: 0,
        }
    );
    // After dropping default, inserted row should have NULL for name
    assert!(matches!(
        &results[3],
        StatementResult::Query { rows, .. } if rows.len() == 1
            && rows[0].values[1] == aiondb_core::Value::Null
    ));
}

#[test]
fn alter_table_set_not_null_succeeds() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let results = engine
        .execute_sql(
            &session,
            "CREATE TABLE t (id INT, name TEXT); \
             INSERT INTO t VALUES (1, 'alice'); \
             ALTER TABLE t ALTER COLUMN name SET NOT NULL",
        )
        .expect("execute");

    assert_eq!(results.len(), 3);
    assert_eq!(
        results[2],
        StatementResult::Command {
            tag: "ALTER TABLE".to_owned(),
            rows_affected: 0,
        }
    );

    let err = engine
        .execute_sql(&session, "UPDATE t SET name = NULL WHERE id = 1")
        .expect_err("UPDATE should respect ALTER COLUMN ... SET NOT NULL");
    assert!(format!("{err}").contains("violates not-null constraint"));
}

#[test]
fn alter_table_drop_not_null_succeeds() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let results = engine
        .execute_sql(
            &session,
            "CREATE TABLE t (id INT, name TEXT NOT NULL DEFAULT 'x'); \
             ALTER TABLE t ALTER COLUMN name DROP NOT NULL; \
             INSERT INTO t (id) VALUES (1); \
             SELECT id, name FROM t",
        )
        .expect("execute");

    assert_eq!(results.len(), 4);
    assert_eq!(
        results[1],
        StatementResult::Command {
            tag: "ALTER TABLE".to_owned(),
            rows_affected: 0,
        }
    );
}

#[test]
fn alter_table_rename_nonexistent_table_fails() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let err = engine
        .execute_sql(&session, "ALTER TABLE ghost RENAME TO other")
        .unwrap_err();
    assert!(err.to_string().contains("does not exist"));
}

#[test]
fn alter_table_rename_nonexistent_column_fails() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(&session, "CREATE TABLE t (id INT)")
        .expect("create");

    let err = engine
        .execute_sql(&session, "ALTER TABLE t RENAME COLUMN ghost TO new_name")
        .unwrap_err();
    assert!(err.to_string().contains("does not exist"));
}

#[test]
fn alter_table_set_default_nonexistent_column_fails() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(&session, "CREATE TABLE t (id INT)")
        .expect("create");

    let err = engine
        .execute_sql(&session, "ALTER TABLE t ALTER COLUMN ghost SET DEFAULT 42")
        .unwrap_err();
    assert!(err.to_string().contains("does not exist"));
}
