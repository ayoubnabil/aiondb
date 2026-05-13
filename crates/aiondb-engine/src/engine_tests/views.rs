use super::*;
use aiondb_catalog::QualifiedName;

#[test]
fn create_view_with_simple_select() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE TABLE users (id INT, name TEXT); \
             INSERT INTO users VALUES (1, 'alice'), (2, 'bob')",
        )
        .expect("setup");

    let results = engine
        .execute_sql(
            &session,
            "CREATE VIEW all_users AS SELECT id, name FROM users",
        )
        .expect("create view");
    assert_eq!(
        results,
        vec![StatementResult::Command {
            tag: "CREATE VIEW".to_owned(),
            rows_affected: 0,
        }]
    );
}

#[test]
fn select_star_from_view() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE TABLE users (id INT, name TEXT); \
             INSERT INTO users VALUES (1, 'alice'), (2, 'bob'); \
             CREATE VIEW all_users AS SELECT id, name FROM users",
        )
        .expect("setup");

    let results = engine
        .execute_sql(&session, "SELECT * FROM all_users")
        .expect("select from view");
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
        }]
    );
}

#[test]
fn select_with_where_from_view() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE TABLE users (id INT, name TEXT); \
             INSERT INTO users VALUES (1, 'alice'), (2, 'bob'), (3, 'carol'); \
             CREATE VIEW all_users AS SELECT id, name FROM users",
        )
        .expect("setup");

    let results = engine
        .execute_sql(&session, "SELECT * FROM all_users WHERE id > 1")
        .expect("select with filter");
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
        }]
    );
}

#[test]
fn drop_view() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE TABLE users (id INT, name TEXT); \
             CREATE VIEW all_users AS SELECT id, name FROM users",
        )
        .expect("setup");

    let results = engine
        .execute_sql(&session, "DROP VIEW all_users")
        .expect("drop view");
    assert_eq!(
        results,
        vec![StatementResult::Command {
            tag: "DROP VIEW".to_owned(),
            rows_affected: 0,
        }]
    );
}

#[test]
fn drop_view_resolves_unqualified_name_from_later_search_path_schema() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE SCHEMA analytics; \
             CREATE TABLE analytics.users (id INT, name TEXT); \
             CREATE VIEW analytics.all_users AS SELECT id, name FROM analytics.users; \
             SET search_path TO public, analytics",
        )
        .expect("setup search_path drop view");

    engine
        .execute_sql(&session, "DROP VIEW all_users")
        .expect("drop view via later search_path schema");

    let err = engine
        .execute_sql(&session, "SELECT * FROM analytics.all_users")
        .expect_err("view should be dropped");
    assert_eq!(err.report().sqlstate, aiondb_core::SqlState::UndefinedTable);
}

#[test]
fn drop_view_if_exists_resolves_unqualified_name_from_later_search_path_schema() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE SCHEMA analytics; \
             CREATE TABLE analytics.users (id INT, name TEXT); \
             CREATE VIEW analytics.all_users AS SELECT id, name FROM analytics.users; \
             SET search_path TO public, analytics; \
             DROP VIEW IF EXISTS all_users",
        )
        .expect("drop guarded view via later search_path schema");

    let err = engine
        .execute_sql(&session, "SELECT * FROM analytics.all_users")
        .expect_err("view should be dropped");
    assert_eq!(err.report().sqlstate, aiondb_core::SqlState::UndefinedTable);
}

#[test]
fn pg_get_viewdef_resolves_unqualified_view_name() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE TABLE users (id INT, name TEXT); \
             CREATE VIEW all_users AS SELECT id, name FROM users",
        )
        .expect("setup");

    let results = engine
        .execute_sql(&session, "SELECT pg_get_viewdef('all_users')")
        .expect("pg_get_viewdef");

    if let StatementResult::Query { rows, .. } = &results[0] {
        assert_eq!(rows.len(), 1);
        match &rows[0].values[0] {
            aiondb_core::Value::Text(sql) => {
                assert!(sql.contains("SELECT id, name FROM "));
                assert!(sql.contains("users"));
            }
            other => panic!("expected text, got {other:?}"),
        }
    } else {
        panic!("expected query result");
    }
}

#[test]
fn pg_get_viewdef_resolves_schema_qualified_view_name() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE TABLE users (id INT, name TEXT); \
             CREATE VIEW all_users AS SELECT id, name FROM users",
        )
        .expect("setup");

    let results = engine
        .execute_sql(&session, "SELECT pg_get_viewdef('public.all_users', true)")
        .expect("pg_get_viewdef");

    if let StatementResult::Query { rows, .. } = &results[0] {
        assert_eq!(rows.len(), 1);
        match &rows[0].values[0] {
            aiondb_core::Value::Text(sql) => {
                assert!(sql.contains("SELECT id, name FROM "));
                assert!(sql.contains("users"));
            }
            other => panic!("expected text, got {other:?}"),
        }
    } else {
        panic!("expected query result");
    }
}

#[test]
fn pg_get_viewdef_resolves_unqualified_view_name_via_search_path() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE SCHEMA analytics; \
             SET search_path TO analytics, public; \
             CREATE TABLE users (id INT, name TEXT); \
             CREATE VIEW all_users AS SELECT id, name FROM users",
        )
        .expect("setup");

    let results = engine
        .execute_sql(&session, "SELECT pg_get_viewdef('all_users')")
        .expect("pg_get_viewdef");

    if let StatementResult::Query { rows, .. } = &results[0] {
        assert_eq!(rows.len(), 1);
        match &rows[0].values[0] {
            aiondb_core::Value::Text(sql) => {
                assert!(sql.contains("SELECT id, name FROM "));
                assert!(sql.contains("users"));
            }
            other => panic!("expected text, got {other:?}"),
        }
    } else {
        panic!("expected query result");
    }
}

#[test]
fn pg_get_viewdef_resolves_unqualified_view_name_via_default_user_search_path() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE SCHEMA alice; \
             CREATE TABLE alice.users (id INT, name TEXT); \
             CREATE VIEW alice.all_users AS SELECT id, name FROM alice.users",
        )
        .expect("setup");

    let results = engine
        .execute_sql(&session, "SELECT pg_get_viewdef('all_users')")
        .expect("pg_get_viewdef");

    if let StatementResult::Query { rows, .. } = &results[0] {
        assert_eq!(rows.len(), 1);
        match &rows[0].values[0] {
            aiondb_core::Value::Text(sql) => {
                assert!(sql.contains("SELECT id, name FROM alice.users"));
            }
            other => panic!("expected text, got {other:?}"),
        }
    } else {
        panic!("expected query result");
    }
}

#[test]
fn compat_insert_rule_rejects_insert_select_with_feature_not_supported() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE TABLE base_table (id INT); \
             CREATE TABLE src_ids (id INT); \
             INSERT INTO src_ids VALUES (1), (2); \
             CREATE VIEW base_view AS SELECT id FROM base_table; \
             CREATE RULE base_view_insert AS \
                 ON INSERT TO base_view DO INSTEAD INSERT INTO base_table VALUES (new.id)",
        )
        .expect("setup");

    let error = engine
        .execute_sql(
            &session,
            "INSERT INTO base_view SELECT id FROM src_ids ORDER BY id",
        )
        .expect_err("INSERT ... SELECT through compat rule should fail cleanly");
    assert_eq!(error.sqlstate(), aiondb_core::SqlState::FeatureNotSupported);
    assert!(
        error
            .report()
            .message
            .contains("INSERT ... SELECT is not supported for compatibility rewrite rules"),
        "expected feature-not-supported message, got: {}",
        error.report().message
    );

    let rows = query_rows(&engine, &session, "SELECT id FROM base_table ORDER BY id");
    assert!(
        rows.is_empty(),
        "compat rule insert-select should not apply writes"
    );
}

#[test]
fn compat_insert_rule_resolves_view_via_search_path() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE SCHEMA analytics; \
             SET search_path TO analytics, public; \
             CREATE TABLE base_table (id INT); \
             CREATE VIEW base_view AS SELECT id FROM base_table; \
             CREATE RULE base_view_insert AS \
                 ON INSERT TO base_view DO INSTEAD INSERT INTO base_table VALUES (new.id)",
        )
        .expect("setup compat rule in search_path schema");

    engine
        .execute_sql(&session, "INSERT INTO base_view VALUES (41)")
        .expect("insert through compat rule");

    assert_eq!(
        query_rows(&engine, &session, "SELECT id FROM base_table"),
        vec![Row::new(vec![Value::Int(41)])]
    );
}

#[test]
fn compat_table_delete_rule_applies_inside_with_cte() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE TABLE y (a INT); \
             INSERT INTO y VALUES (11), (12), (13); \
             CREATE RULE y_rule AS ON DELETE TO y DO INSTEAD \
               INSERT INTO y VALUES(42) RETURNING *",
        )
        .expect("setup table rule");

    assert_eq!(
        query_rows(
            &engine,
            &session,
            "WITH t AS (DELETE FROM y RETURNING *) SELECT * FROM t",
        ),
        vec![Row::new(vec![Value::Int(42)])]
    );
}

#[test]
fn compat_temp_table_delete_rule_applies_inside_with_cte() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE TEMP TABLE y (a INT); \
             INSERT INTO y VALUES (11), (12), (13); \
             CREATE RULE y_rule AS ON DELETE TO y DO INSTEAD \
               INSERT INTO y VALUES(42) RETURNING *",
        )
        .expect("setup temp table rule");

    assert_eq!(
        query_rows(
            &engine,
            &session,
            "WITH t AS (DELETE FROM y RETURNING *) SELECT * FROM t",
        ),
        vec![Row::new(vec![Value::Int(42)])]
    );
}

#[test]
fn compat_temp_table_drop_rule_restores_delete_insert_with_flow() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE TEMP TABLE y (a INT); \
             INSERT INTO y VALUES (0), (1), (2), (3), (4); \
             CREATE RULE y_rule AS ON DELETE TO y DO INSTEAD \
               INSERT INTO y VALUES(42) RETURNING *; \
             DROP RULE y_rule ON y",
        )
        .expect("setup and drop temp table rule");

    assert_eq!(
        query_rows(
            &engine,
            &session,
            "WITH t AS ( \
               DELETE FROM y WHERE a <= 2 RETURNING * \
             ) \
             INSERT INTO y SELECT -a FROM t RETURNING *",
        ),
        vec![
            Row::new(vec![Value::Int(0)]),
            Row::new(vec![Value::Int(-1)]),
            Row::new(vec![Value::Int(-2)]),
        ]
    );
}

#[test]
fn compat_table_insert_rule_preserves_select_output_in_with_statement() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE TABLE bug6051_3 (i INT); \
             INSERT INTO bug6051_3 VALUES (1), (2), (3); \
             CREATE TABLE bug6051_2 (i INT); \
             INSERT INTO bug6051_2 VALUES (1), (2), (3); \
             CREATE RULE bug6051_3_ins AS ON INSERT TO bug6051_3 DO INSTEAD \
               SELECT i FROM bug6051_2",
        )
        .expect("setup insert-select table rule");

    assert_eq!(
        query_rows(
            &engine,
            &session,
            "WITH t1 AS (DELETE FROM bug6051_3 RETURNING *) \
             INSERT INTO bug6051_3 SELECT * FROM t1",
        ),
        vec![
            Row::new(vec![Value::Int(1)]),
            Row::new(vec![Value::Int(2)]),
            Row::new(vec![Value::Int(3)]),
            Row::new(vec![Value::Int(1)]),
            Row::new(vec![Value::Int(2)]),
            Row::new(vec![Value::Int(3)]),
            Row::new(vec![Value::Int(1)]),
            Row::new(vec![Value::Int(2)]),
            Row::new(vec![Value::Int(3)]),
        ]
    );
}

#[test]
fn compat_temp_table_insert_rule_preserves_select_output_in_with_statement() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE TEMP TABLE bug6051_3 (i INT); \
             INSERT INTO bug6051_3 VALUES (1), (2), (3); \
             CREATE TEMP TABLE bug6051_2 (i INT); \
             INSERT INTO bug6051_2 VALUES (1), (2), (3); \
             CREATE RULE bug6051_3_ins AS ON INSERT TO bug6051_3 DO INSTEAD \
               SELECT i FROM bug6051_2",
        )
        .expect("setup temp insert-select table rule");

    assert_eq!(
        query_rows(
            &engine,
            &session,
            "WITH t1 AS (DELETE FROM bug6051_3 RETURNING *) \
             INSERT INTO bug6051_3 SELECT * FROM t1",
        ),
        vec![
            Row::new(vec![Value::Int(1)]),
            Row::new(vec![Value::Int(2)]),
            Row::new(vec![Value::Int(3)]),
            Row::new(vec![Value::Int(1)]),
            Row::new(vec![Value::Int(2)]),
            Row::new(vec![Value::Int(3)]),
            Row::new(vec![Value::Int(1)]),
            Row::new(vec![Value::Int(2)]),
            Row::new(vec![Value::Int(3)]),
        ]
    );
}

#[test]
fn compat_temp_table_insert_rule_preserves_output_with_explicit_txn_local_settings() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE TEMP TABLE bug6051_3 (i INT); \
             INSERT INTO bug6051_3 VALUES (1), (2), (3); \
             CREATE TEMP TABLE bug6051_2 (i INT); \
             INSERT INTO bug6051_2 VALUES (1), (2), (3); \
             CREATE RULE bug6051_3_ins AS ON INSERT TO bug6051_3 DO INSTEAD \
               SELECT i FROM bug6051_2",
        )
        .expect("setup temp insert-select table rule");

    engine.execute_sql(&session, "BEGIN").expect("begin");
    engine
        .execute_sql(&session, "SET LOCAL debug_parallel_query = on")
        .expect("set local");
    assert_eq!(
        query_rows(
            &engine,
            &session,
            "WITH t1 AS (DELETE FROM bug6051_3 RETURNING *) \
             INSERT INTO bug6051_3 SELECT * FROM t1",
        ),
        vec![
            Row::new(vec![Value::Int(1)]),
            Row::new(vec![Value::Int(2)]),
            Row::new(vec![Value::Int(3)]),
            Row::new(vec![Value::Int(1)]),
            Row::new(vec![Value::Int(2)]),
            Row::new(vec![Value::Int(3)]),
            Row::new(vec![Value::Int(1)]),
            Row::new(vec![Value::Int(2)]),
            Row::new(vec![Value::Int(3)]),
        ]
    );
    engine.execute_sql(&session, "COMMIT").expect("commit");
}

#[test]
fn with_dml_delete_returning_into_insert_returning_keeps_cte_rows() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE TABLE y (a INT); INSERT INTO y VALUES (11), (12), (13)",
        )
        .expect("create y");
    engine
        .execute_sql(
            &session,
            "CREATE RULE y_rule AS ON DELETE TO y DO INSTEAD \
             INSERT INTO y VALUES(42) RETURNING *",
        )
        .expect("create y rule");
    engine
        .execute_sql(
            &session,
            "WITH t AS (DELETE FROM y RETURNING *) SELECT * FROM t",
        )
        .expect("apply delete rule");
    engine
        .execute_sql(&session, "DROP RULE y_rule ON y")
        .expect("drop y rule");
    engine
        .execute_sql(
            &session,
            "WITH RECURSIVE t(a) AS ( \
               SELECT 0 \
               UNION ALL \
               SELECT a + 1 FROM t WHERE a + 1 < 5 \
             ), t2 AS ( \
               INSERT INTO y SELECT * FROM t RETURNING * \
             ) \
             SELECT * FROM t2 JOIN y USING (a) ORDER BY a",
        )
        .expect("seed y with cte insert");

    let rows = query_rows(
        &engine,
        &session,
        "WITH t AS ( \
           DELETE FROM y WHERE a <= 10 RETURNING * \
         ) \
         INSERT INTO y SELECT -a FROM t RETURNING *",
    );
    assert_eq!(
        rows,
        vec![
            Row::new(vec![Value::Int(0)]),
            Row::new(vec![Value::Int(-1)]),
            Row::new(vec![Value::Int(-2)]),
            Row::new(vec![Value::Int(-3)]),
            Row::new(vec![Value::Int(-4)]),
        ]
    );
}

#[test]
fn with_outer_join_recursive_errors_do_not_break_following_with_dml_chain() {
    fn single_int_column(rows: Vec<Row>) -> Vec<i64> {
        rows.into_iter()
            .map(|row| match row.values.first() {
                Some(Value::Int(v)) => i64::from(*v),
                other => panic!("expected first column INT, got {other:?}"),
            })
            .collect()
    }

    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE TEMPORARY TABLE y (a INTEGER); \
             INSERT INTO y SELECT generate_series(1, 10)",
        )
        .expect("create temp y");

    for sql in [
        "WITH RECURSIVE x(n) AS (SELECT a FROM y WHERE a = 1 \
           UNION ALL \
           SELECT x.n+1 FROM y LEFT JOIN x ON x.n = y.a WHERE n < 10) \
         SELECT * FROM x",
        "WITH RECURSIVE x(n) AS (SELECT a FROM y WHERE a = 1 \
           UNION ALL \
           SELECT x.n+1 FROM x RIGHT JOIN y ON x.n = y.a WHERE n < 10) \
         SELECT * FROM x",
        "WITH RECURSIVE x(n) AS (SELECT a FROM y WHERE a = 1 \
           UNION ALL \
           SELECT x.n+1 FROM x FULL JOIN y ON x.n = y.a WHERE n < 10) \
         SELECT * FROM x",
    ] {
        let err = engine
            .execute_sql(&session, sql)
            .expect_err("outer join recursion should error");
        assert!(
            err.to_string()
                .contains("must not appear within an outer join"),
            "unexpected error: {err}"
        );
    }

    engine
        .execute_sql(
            &session,
            "WITH t AS ( \
               INSERT INTO y VALUES \
                 (11),(12),(13),(14),(15),(16),(17),(18),(19),(20) \
               RETURNING * \
             ) \
             SELECT * FROM t",
        )
        .expect("seed extra rows");
    engine
        .execute_sql(
            &session,
            "WITH t AS (UPDATE y SET a = a + 1 RETURNING *) SELECT * FROM t",
        )
        .expect("bump rows");
    engine
        .execute_sql(
            &session,
            "WITH t AS (DELETE FROM y WHERE a <= 10 RETURNING *) SELECT * FROM t",
        )
        .expect("delete <=10");

    engine
        .execute_sql(
            &session,
            "WITH RECURSIVE t AS ( \
               INSERT INTO y SELECT a + 5 FROM t2 WHERE a > 5 RETURNING * \
             ), t2 AS ( \
               UPDATE y SET a = a - 11 RETURNING * \
             ) \
             SELECT * FROM t UNION ALL SELECT * FROM t2",
        )
        .expect("forward reference with recursive + data-modifying CTE");
    engine
        .execute_sql(
            &session,
            "CREATE RULE y_rule AS ON DELETE TO y DO INSTEAD \
               INSERT INTO y VALUES(42) RETURNING *",
        )
        .expect("create delete rule");
    engine
        .execute_sql(
            &session,
            "WITH t AS (DELETE FROM y RETURNING *) SELECT * FROM t",
        )
        .expect("run delete rule");
    engine
        .execute_sql(&session, "DROP RULE y_rule ON y")
        .expect("drop delete rule");

    engine
        .execute_sql(
            &session,
            "WITH RECURSIVE t(a) AS ( \
               SELECT 0 \
               UNION ALL \
               SELECT a + 1 FROM t WHERE a + 1 < 5 \
             ), t2 AS ( \
               INSERT INTO y SELECT * FROM t RETURNING * \
             ) \
             SELECT * FROM t2 JOIN y USING (a)",
        )
        .expect("recursive t + insert t2");

    let post_recursive = single_int_column(query_rows(
        &engine,
        &session,
        "SELECT a FROM y ORDER BY a, ctid",
    ));
    let small_values = post_recursive.iter().filter(|v| **v <= 10).count();
    assert!(
        small_values >= 10,
        "expected many <=10 values after recursive CTE chain, got {post_recursive:?}"
    );

    let delete_insert_rows = single_int_column(query_rows(
        &engine,
        &session,
        "WITH t AS (DELETE FROM y WHERE a <= 10 RETURNING *) \
         INSERT INTO y SELECT -a FROM t RETURNING *",
    ));
    assert!(
        !delete_insert_rows.is_empty(),
        "expected DELETE..RETURNING to feed INSERT..RETURNING, got empty rows"
    );
}

#[test]
fn with_insert_on_conflict_returning_cte_joins_inserted_rows() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE TABLE y (a INT); \
             INSERT INTO y VALUES (0), (0); \
             CREATE TABLE withz AS \
               SELECT i AS k, (i || ' v')::text AS v FROM generate_series(1, 16, 3) i; \
             ALTER TABLE withz ADD UNIQUE (k)",
        )
        .expect("setup");

    let rows = query_rows(
        &engine,
        &session,
        "WITH t AS ( \
            INSERT INTO withz \
            SELECT i, 'insert' FROM generate_series(0, 16) i \
            ON CONFLICT (k) DO UPDATE SET v = withz.v || ', now update' \
            RETURNING * \
         ) \
         SELECT * FROM t JOIN y ON t.k = y.a ORDER BY a, k",
    );
    assert_eq!(
        rows,
        vec![
            Row::new(vec![
                Value::Int(0),
                Value::Text("insert".to_owned()),
                Value::Int(0),
            ]),
            Row::new(vec![
                Value::Int(0),
                Value::Text("insert".to_owned()),
                Value::Int(0),
            ]),
        ]
    );
}

#[test]
fn with_insert_on_conflict_returning_cte_joins_after_prior_with_dml_sequence() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE TABLE y (a INT); \
             INSERT INTO y VALUES \
               (1100),(1200),(1300),(1400),(1500),(4200), \
               (0),(-100),(-200),(-300),(-400),(-500),(-600),(-700),(-800),(-900),(-1000), \
               (0),(-100),(-200),(-300),(-400)",
        )
        .expect("seed y in same shape as with.sql before withz");

    engine
        .execute_sql(
            &session,
            "CREATE TABLE withz AS \
               SELECT i AS k, (i || ' v')::text AS v FROM generate_series(1, 16, 3) i; \
             ALTER TABLE withz ADD UNIQUE (k)",
        )
        .expect("setup withz");

    let rows = query_rows(
        &engine,
        &session,
        "WITH t AS ( \
            INSERT INTO withz SELECT i, 'insert' \
            FROM generate_series(0, 16) i \
            ON CONFLICT (k) DO UPDATE SET v = withz.v || ', now update' \
            RETURNING * \
         ) \
         SELECT * FROM t JOIN y ON t.k = y.a ORDER BY a, k",
    );
    assert_eq!(
        rows,
        vec![
            Row::new(vec![
                Value::Int(0),
                Value::Text("insert".to_owned()),
                Value::Int(0),
            ]),
            Row::new(vec![
                Value::Int(0),
                Value::Text("insert".to_owned()),
                Value::Int(0),
            ]),
        ]
    );
}

#[test]
fn with_insert_on_conflict_returning_cte_joins_after_set_local_commit() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE TABLE y (a INT); \
             INSERT INTO y VALUES (0), (0); \
             CREATE TABLE withz AS \
               SELECT i AS k, (i || ' v')::text AS v FROM generate_series(1, 16, 3) i; \
             ALTER TABLE withz ADD UNIQUE (k)",
        )
        .expect("setup");

    engine.execute_sql(&session, "BEGIN").expect("begin");
    engine
        .execute_sql(&session, "SET LOCAL debug_parallel_query = on")
        .expect("set local");
    engine.execute_sql(&session, "COMMIT").expect("commit");

    let rows = query_rows(
        &engine,
        &session,
        "WITH t AS ( \
            INSERT INTO withz \
            SELECT i, 'insert' FROM generate_series(0, 16) i \
            ON CONFLICT (k) DO UPDATE SET v = withz.v || ', now update' \
            RETURNING * \
         ) \
         SELECT * FROM t JOIN y ON t.k = y.a ORDER BY a, k",
    );
    assert_eq!(
        rows,
        vec![
            Row::new(vec![
                Value::Int(0),
                Value::Text("insert".to_owned()),
                Value::Int(0),
            ]),
            Row::new(vec![
                Value::Int(0),
                Value::Text("insert".to_owned()),
                Value::Int(0),
            ]),
        ]
    );
}

#[test]
fn with_insert_on_conflict_returning_cte_joins_temp_y_rows() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE TEMP TABLE y (a INT); \
             INSERT INTO y VALUES (0), (0); \
             CREATE TABLE withz AS \
               SELECT i AS k, (i || ' v')::text AS v FROM generate_series(1, 16, 3) i; \
             ALTER TABLE withz ADD UNIQUE (k)",
        )
        .expect("setup");

    let rows = query_rows(
        &engine,
        &session,
        "WITH t AS ( \
            INSERT INTO withz \
            SELECT i, 'insert' FROM generate_series(0, 16) i \
            ON CONFLICT (k) DO UPDATE SET v = withz.v || ', now update' \
            RETURNING * \
         ) \
         SELECT * FROM t JOIN y ON t.k = y.a ORDER BY a, k",
    );
    assert_eq!(
        rows,
        vec![
            Row::new(vec![
                Value::Int(0),
                Value::Text("insert".to_owned()),
                Value::Int(0),
            ]),
            Row::new(vec![
                Value::Int(0),
                Value::Text("insert".to_owned()),
                Value::Int(0),
            ]),
        ]
    );
}

#[test]
fn with_delete_insert_returning_cte_preserves_rows_on_temp_y_prestate() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE TEMP TABLE y (a INT); \
             INSERT INTO y VALUES \
               (0),(1),(2),(3),(4),(5),(6),(11),(7),(12),(8),(13),(9),(14),(10),(15),(42),(0),(1),(2),(3),(4)",
        )
        .expect("seed temp y");

    let rows = query_rows(
        &engine,
        &session,
        "WITH t AS ( \
            DELETE FROM y WHERE a <= 10 RETURNING * \
         ) \
         INSERT INTO y SELECT -a FROM t RETURNING *",
    );

    assert_eq!(
        rows,
        vec![
            Row::new(vec![Value::Int(0)]),
            Row::new(vec![Value::Int(-1)]),
            Row::new(vec![Value::Int(-2)]),
            Row::new(vec![Value::Int(-3)]),
            Row::new(vec![Value::Int(-4)]),
            Row::new(vec![Value::Int(-5)]),
            Row::new(vec![Value::Int(-6)]),
            Row::new(vec![Value::Int(-7)]),
            Row::new(vec![Value::Int(-8)]),
            Row::new(vec![Value::Int(-9)]),
            Row::new(vec![Value::Int(-10)]),
            Row::new(vec![Value::Int(0)]),
            Row::new(vec![Value::Int(-1)]),
            Row::new(vec![Value::Int(-2)]),
            Row::new(vec![Value::Int(-3)]),
            Row::new(vec![Value::Int(-4)]),
        ]
    );
}

#[test]
fn with_delete_insert_returning_rows_survive_recursive_outer_join_errors() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE TEMP TABLE y (a INT); \
             INSERT INTO y VALUES \
               (0),(1),(2),(3),(4),(5),(6),(11),(7),(12),(8),(13),(9),(14),(10),(15),(42),(0),(1),(2),(3),(4)",
        )
        .expect("seed temp y");

    for sql in [
        "WITH RECURSIVE x(n) AS ( \
           SELECT a FROM y WHERE a = 1 \
           UNION ALL \
           SELECT x.n + 1 \
           FROM y LEFT JOIN x ON x.n = y.a \
           WHERE n < 10 \
         ) \
         SELECT * FROM x",
        "WITH RECURSIVE x(n) AS ( \
           SELECT a FROM y WHERE a = 1 \
           UNION ALL \
           SELECT x.n + 1 \
           FROM y RIGHT JOIN x ON x.n = y.a \
           WHERE n < 10 \
         ) \
         SELECT * FROM x",
        "WITH RECURSIVE x(n) AS ( \
           SELECT a FROM y WHERE a = 1 \
           UNION ALL \
           SELECT x.n + 1 \
           FROM y FULL JOIN x ON x.n = y.a \
           WHERE n < 10 \
         ) \
         SELECT * FROM x",
    ] {
        let err = engine
            .execute_sql(&session, sql)
            .expect_err("recursive outer-join term must fail");
        assert_eq!(err.report().sqlstate, aiondb_core::SqlState::SyntaxError);
    }

    let rows = query_rows(
        &engine,
        &session,
        "WITH t AS ( \
            DELETE FROM y WHERE a <= 10 RETURNING * \
         ) \
         INSERT INTO y SELECT -a FROM t RETURNING *",
    );

    assert_eq!(
        rows,
        vec![
            Row::new(vec![Value::Int(0)]),
            Row::new(vec![Value::Int(-1)]),
            Row::new(vec![Value::Int(-2)]),
            Row::new(vec![Value::Int(-3)]),
            Row::new(vec![Value::Int(-4)]),
            Row::new(vec![Value::Int(-5)]),
            Row::new(vec![Value::Int(-6)]),
            Row::new(vec![Value::Int(-7)]),
            Row::new(vec![Value::Int(-8)]),
            Row::new(vec![Value::Int(-9)]),
            Row::new(vec![Value::Int(-10)]),
            Row::new(vec![Value::Int(0)]),
            Row::new(vec![Value::Int(-1)]),
            Row::new(vec![Value::Int(-2)]),
            Row::new(vec![Value::Int(-3)]),
            Row::new(vec![Value::Int(-4)]),
        ]
    );
}

#[test]
fn with_dml_bug6051_chain_preserves_rule_select_output_rows() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE TEMP TABLE bug6051 AS \
               SELECT i FROM generate_series(1,3) AS t(i); \
             CREATE TEMP TABLE bug6051_2 (i INT); \
             CREATE RULE bug6051_ins AS ON INSERT TO bug6051 DO INSTEAD \
               INSERT INTO bug6051_2 VALUES(NEW.i); \
             WITH t1 AS (DELETE FROM bug6051 RETURNING *) \
               INSERT INTO bug6051 SELECT * FROM t1; \
             CREATE TEMP TABLE bug6051_3 AS \
               SELECT a FROM generate_series(11,13) AS a; \
             CREATE RULE bug6051_3_ins AS ON INSERT TO bug6051_3 DO INSTEAD \
               SELECT i FROM bug6051_2",
        )
        .expect("setup bug6051 chain");

    assert_eq!(
        query_rows(&engine, &session, "SELECT * FROM bug6051_2 ORDER BY i"),
        vec![
            Row::new(vec![Value::Int(1)]),
            Row::new(vec![Value::Int(2)]),
            Row::new(vec![Value::Int(3)]),
        ]
    );

    engine.execute_sql(&session, "BEGIN").expect("begin");
    engine
        .execute_sql(&session, "SET LOCAL debug_parallel_query = on")
        .expect("set local");
    let rows = query_rows(
        &engine,
        &session,
        "WITH t1 AS (DELETE FROM bug6051_3 RETURNING *) \
         INSERT INTO bug6051_3 SELECT * FROM t1",
    );
    engine.execute_sql(&session, "COMMIT").expect("commit");

    assert_eq!(
        rows,
        vec![
            Row::new(vec![Value::Int(1)]),
            Row::new(vec![Value::Int(2)]),
            Row::new(vec![Value::Int(3)]),
            Row::new(vec![Value::Int(1)]),
            Row::new(vec![Value::Int(2)]),
            Row::new(vec![Value::Int(3)]),
            Row::new(vec![Value::Int(1)]),
            Row::new(vec![Value::Int(2)]),
            Row::new(vec![Value::Int(3)]),
        ]
    );
    assert!(
        query_rows(&engine, &session, "SELECT * FROM bug6051_3").is_empty(),
        "source CTE DELETE should run and leave bug6051_3 empty"
    );
}

#[test]
fn view_with_group_by() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE TABLE orders (product TEXT, amount INT); \
             INSERT INTO orders VALUES ('a', 10), ('b', 20), ('a', 30); \
             CREATE VIEW product_totals AS \
                 SELECT product, SUM(amount) AS total FROM orders GROUP BY product",
        )
        .expect("setup");

    let results = engine
        .execute_sql(&session, "SELECT * FROM product_totals ORDER BY product")
        .expect("select from group by view");

    assert_eq!(
        results,
        vec![StatementResult::Query {
            columns: vec![
                ResultColumn {
                    name: "product".to_owned(),
                    data_type: aiondb_core::DataType::Text,
                    text_type_modifier: None,
                    nullable: true,
                },
                ResultColumn {
                    // SUM(INT) returns INT in AionDB (not BigInt)
                    name: "total".to_owned(),
                    data_type: aiondb_core::DataType::Int,
                    text_type_modifier: None,
                    nullable: true,
                },
            ],
            rows: vec![
                aiondb_core::Row::new(vec![
                    aiondb_core::Value::Text("a".to_owned()),
                    aiondb_core::Value::Int(40),
                ]),
                aiondb_core::Row::new(vec![
                    aiondb_core::Value::Text("b".to_owned()),
                    aiondb_core::Value::Int(20),
                ]),
            ],
        }]
    );
}

#[test]
fn view_with_order_by_and_limit() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE TABLE items (id INT, name TEXT); \
             INSERT INTO items VALUES (3, 'carol'), (1, 'alice'), (2, 'bob'); \
             CREATE VIEW top_items AS \
                 SELECT id, name FROM items ORDER BY id LIMIT 2",
        )
        .expect("setup");

    let results = engine
        .execute_sql(&session, "SELECT * FROM top_items")
        .expect("select from order/limit view");
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
        }]
    );
}

#[test]
fn temp_view_resolves_against_pg_temp() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE TEMP TABLE undername (f1 TEXT, f2 INT); \
             INSERT INTO undername VALUES ('foo', 1), ('bar', 2); \
             CREATE TEMP VIEW overview AS SELECT f1 AS sqli, f2 FROM undername",
        )
        .expect("setup temp view");

    let results = engine
        .execute_sql(
            &session,
            "SELECT * FROM overview WHERE sqli = 'foo' ORDER BY sqli",
        )
        .expect("select from temp view");

    assert_eq!(
        results,
        vec![StatementResult::Query {
            columns: vec![
                ResultColumn {
                    name: "sqli".to_owned(),
                    data_type: aiondb_core::DataType::Text,
                    text_type_modifier: None,
                    nullable: true,
                },
                ResultColumn {
                    name: "f2".to_owned(),
                    data_type: aiondb_core::DataType::Int,
                    text_type_modifier: None,
                    nullable: true,
                },
            ],
            rows: vec![aiondb_core::Row::new(vec![
                aiondb_core::Value::Text("foo".to_owned()),
                aiondb_core::Value::Int(1),
            ])],
        }]
    );
}

#[test]
fn view_used_in_join() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE TABLE users (id INT, name TEXT); \
             INSERT INTO users VALUES (1, 'alice'), (2, 'bob'); \
             CREATE TABLE orders (user_id INT, product TEXT); \
             INSERT INTO orders VALUES (1, 'widget'), (2, 'gadget'); \
             CREATE VIEW user_names AS SELECT id, name FROM users",
        )
        .expect("setup");

    let results = engine
        .execute_sql(
            &session,
            "SELECT orders.product, user_names.name \
             FROM orders \
             INNER JOIN user_names ON orders.user_id = user_names.id \
             ORDER BY orders.product",
        )
        .expect("join with view");
    assert_eq!(
        results,
        vec![StatementResult::Query {
            columns: vec![
                ResultColumn {
                    name: "product".to_owned(),
                    data_type: aiondb_core::DataType::Text,
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
                    aiondb_core::Value::Text("gadget".to_owned()),
                    aiondb_core::Value::Text("bob".to_owned()),
                ]),
                aiondb_core::Row::new(vec![
                    aiondb_core::Value::Text("widget".to_owned()),
                    aiondb_core::Value::Text("alice".to_owned()),
                ]),
            ],
        }]
    );
}

#[test]
fn error_select_from_dropped_view() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE TABLE users (id INT, name TEXT); \
             CREATE VIEW all_users AS SELECT id, name FROM users; \
             DROP VIEW all_users",
        )
        .expect("setup and drop");

    let err = engine
        .execute_sql(&session, "SELECT * FROM all_users")
        .expect_err("should fail after drop");
    let msg = format!("{err}");
    assert!(
        msg.contains("does not exist"),
        "unexpected error message: {msg}"
    );
}

#[test]
fn create_view_with_same_name_twice_errors_without_or_replace() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE TABLE users (id INT, name TEXT); \
             CREATE VIEW all_users AS SELECT id, name FROM users",
        )
        .expect("first create");

    let err = engine
        .execute_sql(
            &session,
            "CREATE VIEW all_users AS SELECT id, name FROM users",
        )
        .expect_err("duplicate CREATE VIEW should fail without OR REPLACE");
    assert!(
        err.to_string().contains("already exists"),
        "unexpected error: {err}"
    );
}

#[test]
fn create_or_replace_view() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE TABLE users (id INT, name TEXT); \
             INSERT INTO users VALUES (1, 'alice'), (2, 'bob'); \
             CREATE VIEW all_users AS SELECT id, name FROM users",
        )
        .expect("setup");

    // CREATE OR REPLACE VIEW should succeed even though the view exists.
    engine
        .execute_sql(
            &session,
            "CREATE OR REPLACE VIEW all_users AS SELECT id FROM users",
        )
        .expect("CREATE OR REPLACE VIEW should succeed");

    // The replaced view should have only the id column.
    let results = engine
        .execute_sql(&session, "SELECT * FROM all_users")
        .expect("select from replaced view");
    assert_eq!(
        results,
        vec![StatementResult::Query {
            columns: vec![ResultColumn {
                name: "id".to_owned(),
                data_type: aiondb_core::DataType::Int,
                text_type_modifier: None,
                nullable: true,
            }],
            rows: vec![
                aiondb_core::Row::new(vec![aiondb_core::Value::Int(1)]),
                aiondb_core::Row::new(vec![aiondb_core::Value::Int(2)]),
            ],
        }]
    );
}

#[test]
fn create_view_with_column_aliases() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    // Create a view with column aliases on a table-backed query
    engine
        .execute_sql(
            &session,
            "CREATE TABLE data (x INT, y INT, z INT); \
             INSERT INTO data VALUES (1, 1, 10), (1, 2, 12), (2, 3, 15); \
             CREATE VIEW aliased(a, b, v) AS SELECT x, y, z FROM data",
        )
        .expect("create view with column aliases");

    // SELECT * should return the aliased column names
    let results = engine
        .execute_sql(&session, "SELECT * FROM aliased")
        .expect("select star from aliased view");
    assert_eq!(results.len(), 1);
    if let StatementResult::Query { columns, rows } = &results[0] {
        assert_eq!(columns[0].name, "a");
        assert_eq!(columns[1].name, "b");
        assert_eq!(columns[2].name, "v");
        assert_eq!(rows.len(), 3);
    } else {
        panic!("expected query result");
    }

    // SELECT specific aliased columns should work
    let results = engine
        .execute_sql(&session, "SELECT a, v FROM aliased")
        .expect("select aliased columns");
    assert_eq!(results.len(), 1);
    if let StatementResult::Query { columns, rows } = &results[0] {
        assert_eq!(columns[0].name, "a");
        assert_eq!(columns[1].name, "v");
        assert_eq!(rows.len(), 3);
        // First row: a=1, v=10
        assert_eq!(rows[0].values[0], aiondb_core::Value::Int(1));
        assert_eq!(rows[0].values[1], aiondb_core::Value::Int(10));
    } else {
        panic!("expected query result");
    }
}

#[test]
fn insert_into_simple_view() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE TABLE base_tbl (a INT PRIMARY KEY, b TEXT DEFAULT 'Unspecified'); \
             INSERT INTO base_tbl VALUES (1, 'Row 1'), (2, 'Row 2'); \
             CREATE VIEW rw_view1 AS SELECT * FROM base_tbl WHERE a > 0",
        )
        .expect("setup");

    // INSERT through the updatable view
    engine
        .execute_sql(&session, "INSERT INTO rw_view1 VALUES (3, 'Row 3')")
        .expect("INSERT into view should succeed");

    let results = engine
        .execute_sql(&session, "SELECT * FROM base_tbl ORDER BY a")
        .expect("select");
    if let StatementResult::Query { rows, .. } = &results[0] {
        assert_eq!(rows.len(), 3);
        assert_eq!(rows[2].values[0], aiondb_core::Value::Int(3));
    } else {
        panic!("expected query result");
    }
}

#[test]
fn insert_into_simple_view_created_via_later_search_path_schema() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE SCHEMA analytics; \
             CREATE TABLE analytics.base_tbl (a INT PRIMARY KEY, b TEXT DEFAULT 'Unspecified'); \
             INSERT INTO analytics.base_tbl VALUES (1, 'Row 1'), (2, 'Row 2'); \
             SET search_path TO public, analytics; \
             CREATE VIEW rw_view_path AS SELECT * FROM base_tbl WHERE a > 0",
        )
        .expect("setup simple view via later search_path schema");

    engine
        .execute_sql(&session, "INSERT INTO rw_view_path VALUES (3, 'Row 3')")
        .expect("INSERT through search_path-created view should succeed");

    let results = engine
        .execute_sql(&session, "SELECT * FROM analytics.base_tbl ORDER BY a")
        .expect("select");
    if let StatementResult::Query { rows, .. } = &results[0] {
        assert_eq!(rows.len(), 3);
        assert_eq!(rows[2].values[0], aiondb_core::Value::Int(3));
    } else {
        panic!("expected query result");
    }
}

#[test]
fn select_from_view_created_via_later_search_path_schema_uses_creation_search_path() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE SCHEMA analytics; \
             CREATE TABLE analytics.base_tbl (a INT PRIMARY KEY, b TEXT); \
             INSERT INTO analytics.base_tbl VALUES (1, 'Row 1'), (2, 'Row 2'); \
             SET search_path TO public, analytics; \
             CREATE VIEW rw_view_path_select AS SELECT * FROM base_tbl WHERE a < 10; \
             CREATE TABLE public.base_tbl (a INT PRIMARY KEY, b TEXT); \
             INSERT INTO public.base_tbl VALUES (100, 'wrong'); \
             SET search_path TO public",
        )
        .expect("setup search_path-created view with colliding base tables");

    let results = engine
        .execute_sql(&session, "SELECT a, b FROM rw_view_path_select ORDER BY a")
        .expect("SELECT through search_path-created view should use creation search_path");
    if let StatementResult::Query { rows, .. } = &results[0] {
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].values[0], aiondb_core::Value::Int(1));
        assert_eq!(
            rows[0].values[1],
            aiondb_core::Value::Text("Row 1".to_owned())
        );
        assert_eq!(rows[1].values[0], aiondb_core::Value::Int(2));
        assert_eq!(
            rows[1].values[1],
            aiondb_core::Value::Text("Row 2".to_owned())
        );
    } else {
        panic!("expected query result");
    }
}

#[test]
fn update_through_simple_view() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE TABLE base_tbl (a INT PRIMARY KEY, b TEXT DEFAULT 'Unspecified'); \
             INSERT INTO base_tbl VALUES (1, 'Row 1'), (2, 'Row 2'); \
             CREATE VIEW rw_view1 AS SELECT * FROM base_tbl WHERE a > 0",
        )
        .expect("setup");

    engine
        .execute_sql(&session, "UPDATE rw_view1 SET b = 'Updated' WHERE a = 1")
        .expect("UPDATE through view should succeed");

    let results = engine
        .execute_sql(&session, "SELECT b FROM base_tbl WHERE a = 1")
        .expect("select");
    if let StatementResult::Query { rows, .. } = &results[0] {
        assert_eq!(
            rows[0].values[0],
            aiondb_core::Value::Text("Updated".to_owned())
        );
    } else {
        panic!("expected query result");
    }
}

#[test]
fn update_through_simple_view_created_via_later_search_path_schema() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE SCHEMA analytics; \
             CREATE TABLE analytics.base_tbl (a INT PRIMARY KEY, b TEXT DEFAULT 'Unspecified'); \
             INSERT INTO analytics.base_tbl VALUES (1, 'Row 1'), (2, 'Row 2'); \
             SET search_path TO public, analytics; \
             CREATE VIEW rw_view_path AS SELECT * FROM base_tbl WHERE a > 0",
        )
        .expect("setup search_path-created simple view");

    engine
        .execute_sql(
            &session,
            "UPDATE rw_view_path SET b = 'Updated' WHERE a = 1",
        )
        .expect("UPDATE through search_path-created view should succeed");

    let results = engine
        .execute_sql(&session, "SELECT b FROM analytics.base_tbl WHERE a = 1")
        .expect("select");
    if let StatementResult::Query { rows, .. } = &results[0] {
        assert_eq!(rows.len(), 1);
        assert_eq!(
            rows[0].values[0],
            aiondb_core::Value::Text("Updated".to_owned())
        );
    } else {
        panic!("expected query result");
    }
}

#[test]
fn delete_through_simple_view() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE TABLE base_tbl (a INT PRIMARY KEY, b TEXT DEFAULT 'Unspecified'); \
             INSERT INTO base_tbl VALUES (1, 'Row 1'), (2, 'Row 2'); \
             CREATE VIEW rw_view1 AS SELECT * FROM base_tbl WHERE a > 0",
        )
        .expect("setup");

    engine
        .execute_sql(&session, "DELETE FROM rw_view1 WHERE a = 2")
        .expect("DELETE through view should succeed");

    let results = engine
        .execute_sql(&session, "SELECT * FROM base_tbl")
        .expect("select");
    if let StatementResult::Query { rows, .. } = &results[0] {
        assert_eq!(rows.len(), 1);
    } else {
        panic!("expected query result");
    }
}

#[test]
fn update_through_view_applies_view_qualifier() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE TABLE base_tbl (a INT PRIMARY KEY, b TEXT); \
             INSERT INTO base_tbl VALUES (-1, 'hidden'), (1, 'visible'); \
             CREATE VIEW rw_view1 AS SELECT * FROM base_tbl WHERE a > 0",
        )
        .expect("setup");
    let view = engine
        .catalog_reader
        .get_view(
            TxnId::default(),
            &QualifiedName::qualified("public", "rw_view1"),
        )
        .expect("get view")
        .expect("view descriptor");
    assert!(
        view.query_sql.contains("WHERE"),
        "stored view SQL should keep qualifier: {}",
        view.query_sql
    );
    let parsed_view = aiondb_parser::parse_sql(&view.query_sql).expect("parse stored view sql");
    let Some(aiondb_parser::Statement::Select(select)) = parsed_view.first() else {
        panic!("stored view sql is not a select: {}", view.query_sql);
    };
    assert!(
        select.selection.is_some(),
        "missing WHERE in parsed stored view sql"
    );

    engine
        .execute_sql(&session, "UPDATE rw_view1 SET b = 'changed'")
        .expect("update through view");

    let rows = query_rows(&engine, &session, "SELECT a, b FROM base_tbl ORDER BY a");
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].values[0], aiondb_core::Value::Int(-1));
    assert_eq!(
        rows[0].values[1],
        aiondb_core::Value::Text("hidden".to_owned())
    );
    assert_eq!(rows[1].values[0], aiondb_core::Value::Int(1));
    assert_eq!(
        rows[1].values[1],
        aiondb_core::Value::Text("changed".to_owned())
    );
}

#[test]
fn delete_through_view_applies_view_qualifier() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE TABLE base_tbl (a INT PRIMARY KEY, b TEXT); \
             INSERT INTO base_tbl VALUES (-1, 'hidden'), (1, 'visible'); \
             CREATE VIEW rw_view1 AS SELECT * FROM base_tbl WHERE a > 0",
        )
        .expect("setup");
    let view = engine
        .catalog_reader
        .get_view(
            TxnId::default(),
            &QualifiedName::qualified("public", "rw_view1"),
        )
        .expect("get view")
        .expect("view descriptor");
    assert!(
        view.query_sql.contains("WHERE"),
        "stored view SQL should keep qualifier: {}",
        view.query_sql
    );

    engine
        .execute_sql(&session, "DELETE FROM rw_view1")
        .expect("delete through view");

    let rows = query_rows(&engine, &session, "SELECT a, b FROM base_tbl ORDER BY a");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].values[0], aiondb_core::Value::Int(-1));
    assert_eq!(
        rows[0].values[1],
        aiondb_core::Value::Text("hidden".to_owned())
    );
}

#[test]
fn view_check_option_visible_in_information_schema_and_alterable() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE TABLE base_tbl (a INT, b INT); \
             CREATE VIEW rw_view1 AS SELECT * FROM base_tbl WHERE a < b WITH LOCAL CHECK OPTION",
        )
        .expect("setup");

    let rows = query_rows(
        &engine,
        &session,
        "SELECT check_option FROM information_schema.views WHERE table_name = 'rw_view1'",
    );
    assert_eq!(rows.len(), 1);
    assert_eq!(
        rows[0].values[0],
        aiondb_core::Value::Text("LOCAL".to_owned())
    );

    engine
        .execute_sql(
            &session,
            "ALTER VIEW rw_view1 SET (check_option = cascaded)",
        )
        .expect("alter check option");

    let rows = query_rows(
        &engine,
        &session,
        "SELECT check_option FROM information_schema.views WHERE table_name = 'rw_view1'",
    );
    assert_eq!(rows.len(), 1);
    assert_eq!(
        rows[0].values[0],
        aiondb_core::Value::Text("CASCADED".to_owned())
    );

    engine
        .execute_sql(&session, "ALTER VIEW rw_view1 RESET (check_option)")
        .expect("reset check option");

    let rows = query_rows(
        &engine,
        &session,
        "SELECT check_option FROM information_schema.views WHERE table_name = 'rw_view1'",
    );
    assert_eq!(rows.len(), 1);
    assert_eq!(
        rows[0].values[0],
        aiondb_core::Value::Text("NONE".to_owned())
    );
}

#[test]
fn drop_table_restrict_rejects_dependent_view() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE TABLE base_tbl (a INT PRIMARY KEY, b TEXT); \
             CREATE VIEW v1 AS SELECT * FROM base_tbl",
        )
        .expect("setup");

    // Default behavior (no CASCADE) = RESTRICT: must refuse.
    let err = engine
        .execute_sql(&session, "DROP TABLE base_tbl")
        .expect_err("DROP TABLE with dependent view should be refused");
    assert_eq!(
        err.sqlstate(),
        aiondb_core::SqlState::DependentObjectsStillExist
    );
    assert!(
        format!("{err}").contains("v1"),
        "error message should mention dependent view: {err}"
    );

    // View still queryable after failed drop.
    engine
        .execute_sql(&session, "SELECT * FROM v1")
        .expect("view still exists after rejected DROP");
}

#[test]
fn drop_table_restrict_explicit_keyword_rejects_dependent_view() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE TABLE base_tbl (a INT PRIMARY KEY); \
             CREATE VIEW v1 AS SELECT * FROM base_tbl",
        )
        .expect("setup");

    let err = engine
        .execute_sql(&session, "DROP TABLE base_tbl RESTRICT")
        .expect_err("DROP TABLE ... RESTRICT must refuse");
    assert_eq!(
        err.sqlstate(),
        aiondb_core::SqlState::DependentObjectsStillExist
    );
}

#[test]
fn drop_table_cascade_drops_dependent_views() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE TABLE base_tbl (a INT PRIMARY KEY, b TEXT); \
             CREATE VIEW v1 AS SELECT * FROM base_tbl; \
             DROP TABLE base_tbl CASCADE",
        )
        .expect("setup and drop");

    // v1 should have been dropped with the table
    let err = engine
        .execute_sql(&session, "SELECT * FROM v1")
        .unwrap_err();
    let msg = format!("{err}");
    assert!(
        msg.contains("does not exist"),
        "expected 'does not exist' error, got: {msg}"
    );
}

#[test]
fn view_with_aliases_dml() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE TABLE base_tbl (a INT PRIMARY KEY, b TEXT DEFAULT 'Unspecified'); \
             INSERT INTO base_tbl VALUES (1, 'Row 1'), (2, 'Row 2'); \
             CREATE VIEW rw_view1 AS SELECT b AS bb, a AS aa FROM base_tbl WHERE a > 0",
        )
        .expect("setup");

    // INSERT using view column aliases
    engine
        .execute_sql(
            &session,
            "INSERT INTO rw_view1 (aa, bb) VALUES (3, 'Row 3')",
        )
        .expect("INSERT with view aliases should succeed");

    let results = engine
        .execute_sql(&session, "SELECT * FROM base_tbl WHERE a = 3")
        .expect("select");
    if let StatementResult::Query { rows, .. } = &results[0] {
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].values[0], aiondb_core::Value::Int(3));
        assert_eq!(
            rows[0].values[1],
            aiondb_core::Value::Text("Row 3".to_owned())
        );
    } else {
        panic!("expected query result");
    }
}

#[test]
fn view_on_view_with_aliases() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE TABLE base_tbl (a INT PRIMARY KEY, b TEXT DEFAULT 'Unspecified'); \
             INSERT INTO base_tbl VALUES (1, 'Row 1'), (2, 'Row 2'), (15, 'Row 15'); \
             CREATE VIEW rw_view1 AS SELECT b AS bb, a AS aa FROM base_tbl WHERE a > 0",
        )
        .expect("setup");

    // Create a view on top of the view using aliased column names
    engine
        .execute_sql(
            &session,
            "CREATE VIEW rw_view2 AS SELECT aa AS aaa, bb AS bbb FROM rw_view1 WHERE aa < 10",
        )
        .expect("CREATE VIEW on top of view should succeed");

    let results = engine
        .execute_sql(&session, "SELECT * FROM rw_view2 ORDER BY aaa")
        .expect("select from view-on-view");
    if let StatementResult::Query { columns, rows } = &results[0] {
        assert_eq!(columns[0].name, "aaa");
        assert_eq!(columns[1].name, "bbb");
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].values[0], aiondb_core::Value::Int(1));
        assert_eq!(rows[1].values[0], aiondb_core::Value::Int(2));
    } else {
        panic!("expected query result");
    }

    // INSERT through view-on-view using the outermost aliases
    engine
        .execute_sql(
            &session,
            "INSERT INTO rw_view2 (aaa, bbb) VALUES (4, 'Row 4')",
        )
        .expect("INSERT through view-on-view should succeed");

    let results = engine
        .execute_sql(&session, "SELECT * FROM base_tbl WHERE a = 4")
        .expect("verify insert");
    if let StatementResult::Query { rows, .. } = &results[0] {
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].values[0], aiondb_core::Value::Int(4));
        assert_eq!(
            rows[0].values[1],
            aiondb_core::Value::Text("Row 4".to_owned())
        );
    } else {
        panic!("expected query result");
    }

    // UPDATE through view-on-view
    engine
        .execute_sql(
            &session,
            "UPDATE rw_view2 SET bbb = 'Updated' WHERE aaa = 1",
        )
        .expect("UPDATE through view-on-view should succeed");

    let results = engine
        .execute_sql(&session, "SELECT b FROM base_tbl WHERE a = 1")
        .expect("verify update");
    if let StatementResult::Query { rows, .. } = &results[0] {
        assert_eq!(
            rows[0].values[0],
            aiondb_core::Value::Text("Updated".to_owned())
        );
    } else {
        panic!("expected query result");
    }

    // DELETE through view-on-view
    engine
        .execute_sql(&session, "DELETE FROM rw_view2 WHERE aaa = 2")
        .expect("DELETE through view-on-view should succeed");

    let results = engine
        .execute_sql(&session, "SELECT * FROM base_tbl WHERE a = 2")
        .expect("verify delete");
    if let StatementResult::Query { rows, .. } = &results[0] {
        assert_eq!(rows.len(), 0);
    } else {
        panic!("expected query result");
    }
}

#[test]
fn view_not_expression_preserves_parentheses_semantics() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE TABLE t (pk INT PRIMARY KEY, col0 INT, col3 INT);
             INSERT INTO t VALUES (1, 100, 0), (2, 400, 0), (3, 900, 0), (4, 700, 200);",
        )
        .expect("setup");

    engine
        .execute_sql(
            &session,
            "CREATE VIEW v_not AS
             SELECT pk, col0
             FROM t
             WHERE NOT ((col0 >= 309) OR col0 > 879 OR col0 > 380 AND (col3 > 153) AND ((col0 > 638)));",
        )
        .expect("create view");

    let direct = engine
        .execute_sql(
            &session,
            "SELECT pk, col0
             FROM t
             WHERE NOT ((col0 >= 309) OR col0 > 879 OR col0 > 380 AND (col3 > 153) AND ((col0 > 638)))
             ORDER BY pk",
        )
        .expect("direct query");

    let from_view = engine
        .execute_sql(&session, "SELECT pk, col0 FROM v_not ORDER BY pk")
        .expect("query view");

    let StatementResult::Query {
        rows: direct_rows, ..
    } = &direct[0]
    else {
        panic!("expected direct query result");
    };
    let StatementResult::Query {
        rows: view_rows, ..
    } = &from_view[0]
    else {
        panic!("expected view query result");
    };

    assert_eq!(view_rows, direct_rows);
    assert_eq!(view_rows.len(), 1);
    assert_eq!(view_rows[0].values[0], aiondb_core::Value::Int(1));
    assert_eq!(view_rows[0].values[1], aiondb_core::Value::Int(100));
}

// ---------------------------------------------------------------------------
// Task #9 / #10: rule & operator error SQLSTATE conformance.
// ---------------------------------------------------------------------------

#[test]
fn drop_rule_without_if_exists_on_missing_rule_returns_undefined_object() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(&session, "CREATE TABLE rule_target (id INT);")
        .expect("setup table");

    let err = engine
        .execute_sql(&session, "DROP RULE nonexistent_rule ON rule_target")
        .expect_err("DROP RULE must error on missing rule");
    assert_eq!(
        err.sqlstate(),
        aiondb_core::SqlState::UndefinedObject,
        "expected UndefinedObject (42704), got {:?}",
        err,
    );
    let rendered = format!("{err}");
    assert!(
        rendered.contains("nonexistent_rule"),
        "error message must mention the rule name: {rendered}",
    );
}

#[test]
fn drop_rule_if_exists_on_missing_rule_emits_notice_and_succeeds() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(&session, "CREATE TABLE rule_target2 (id INT);")
        .expect("setup table");

    let results = engine
        .execute_sql(&session, "DROP RULE IF EXISTS missing_rule ON rule_target2")
        .expect("DROP RULE IF EXISTS must succeed silently");
    // Accepts either a NOTICE+Command pair or just a Command result with
    // matching tag.
    let has_command = results.iter().any(|r| {
        matches!(
            r,
            StatementResult::Command { tag, .. } if tag == "DROP RULE"
        )
    });
    assert!(
        has_command,
        "expected DROP RULE command tag in results: {results:?}"
    );
}

#[test]
fn drop_operator_if_exists_is_rejected_explicitly() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let error = engine
        .execute_sql(&session, "DROP OPERATOR IF EXISTS + (integer, integer)")
        .expect_err("DROP OPERATOR IF EXISTS compat path must fail explicitly");
    assert_eq!(error.sqlstate(), aiondb_core::SqlState::FeatureNotSupported);
    assert!(error
        .report()
        .message
        .contains("unsupported compatibility command: DROP OPERATOR"));
}
