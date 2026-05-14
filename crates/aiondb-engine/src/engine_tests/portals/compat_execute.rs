use super::*;

#[test]
fn execute_portal_supports_compat_execute_query() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(&session, "PREPARE stmt(int) AS SELECT $1 + 1 AS v")
        .expect("prepare compat statement");

    let desc = engine
        .prepare(
            &session,
            "exec_stmt".to_owned(),
            "EXECUTE stmt(41)".to_owned(),
        )
        .expect("prepare sql execute");
    assert_eq!(
        desc.result_columns,
        vec![ResultColumn {
            name: "v".to_owned(),
            data_type: aiondb_core::DataType::Int,
            text_type_modifier: None,
            nullable: false,
        }]
    );

    engine
        .bind(
            &session,
            "exec_stmt_portal".to_owned(),
            "exec_stmt".to_owned(),
            vec![],
        )
        .expect("bind sql execute");
    let batch = engine
        .execute_portal(&session, "exec_stmt_portal", 0)
        .expect("execute sql execute portal");
    assert_eq!(
        batch.columns,
        vec![ResultColumn {
            name: "v".to_owned(),
            data_type: aiondb_core::DataType::Int,
            text_type_modifier: None,
            nullable: false,
        }]
    );
    assert_eq!(
        batch.rows,
        vec![aiondb_core::Row::new(vec![aiondb_core::Value::Int(42)])]
    );
    assert_eq!(batch.tag, "SELECT 1");
}

#[test]
fn execute_portal_supports_compat_explain_execute_query() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(&session, "PREPARE stmt(int) AS SELECT $1 + 1 AS v")
        .expect("prepare compat statement");

    let desc = engine
        .prepare(
            &session,
            "explain_exec_stmt".to_owned(),
            "EXPLAIN EXECUTE stmt(41)".to_owned(),
        )
        .expect("prepare explain execute");
    assert_eq!(
        desc.result_columns,
        vec![ResultColumn {
            name: "QUERY PLAN".to_owned(),
            data_type: aiondb_core::DataType::Text,
            text_type_modifier: None,
            nullable: false,
        }]
    );

    engine
        .bind(
            &session,
            "explain_exec_stmt_portal".to_owned(),
            "explain_exec_stmt".to_owned(),
            vec![],
        )
        .expect("bind explain execute");
    let batch = engine
        .execute_portal(&session, "explain_exec_stmt_portal", 0)
        .expect("execute explain execute portal");
    assert_eq!(
        batch.columns,
        vec![ResultColumn {
            name: "QUERY PLAN".to_owned(),
            data_type: aiondb_core::DataType::Text,
            text_type_modifier: None,
            nullable: false,
        }]
    );
    assert!(
        batch.rows.iter().any(|row| matches!(
            row.values.as_slice(),
            [aiondb_core::Value::Text(line)] if line == "Result"
        )),
        "expected EXPLAIN rows to contain Result, got {:?}",
        batch.rows
    );
    assert_eq!(batch.tag, "EXPLAIN");
}

#[test]
fn execute_portal_supports_compat_do_block_with_notice() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let desc = engine
        .prepare(
            &session,
            "do_stmt".to_owned(),
            "DO $$ BEGIN RAISE NOTICE '%', 'hello from portal'; END $$ LANGUAGE plpgsql".to_owned(),
        )
        .expect("prepare do block");
    assert!(desc.result_columns.is_empty());

    engine
        .bind(
            &session,
            "do_stmt_portal".to_owned(),
            "do_stmt".to_owned(),
            vec![],
        )
        .expect("bind do block");
    let batch = engine
        .execute_portal(&session, "do_stmt_portal", 0)
        .expect("execute do block portal");
    assert!(batch.columns.is_empty());
    assert!(batch.rows.is_empty());
    assert_eq!(batch.tag, "DO");
    assert_eq!(batch.rows_affected, 0);

    let notices = engine
        .drain_pending_notices(&session)
        .expect("drain pending notices");
    assert_eq!(notices, vec!["hello from portal".to_owned()]);
}

#[test]
fn execute_portal_tracks_compat_shell_types() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .prepare(
            &session,
            "create_shell_type".to_owned(),
            "CREATE TYPE shell_alias".to_owned(),
        )
        .expect("prepare create type");
    engine
        .bind(
            &session,
            "create_shell_type_portal".to_owned(),
            "create_shell_type".to_owned(),
            vec![],
        )
        .expect("bind create type");

    let batch = engine
        .execute_portal(&session, "create_shell_type_portal", 0)
        .expect("execute create type portal");
    assert_eq!(batch.tag, "CREATE TYPE");

    engine
        .with_session(&session, |record| {
            assert!(record.shell_types.contains("shell_alias"));
            assert!(record
                .compat_user_types
                .iter()
                .any(|entry| entry.name == "shell_alias"));
            Ok(())
        })
        .expect("inspect compat type state");
}

#[test]
fn execute_portal_supports_compat_create_cast_command() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(&session, "CREATE TYPE casttesttype")
        .expect("create shell type");

    engine
        .prepare(
            &session,
            "create_cast_stmt".to_owned(),
            "CREATE CAST (text AS casttesttype) WITHOUT FUNCTION".to_owned(),
        )
        .expect("prepare create cast");
    engine
        .bind(
            &session,
            "create_cast_portal".to_owned(),
            "create_cast_stmt".to_owned(),
            vec![],
        )
        .expect("bind create cast");
    let batch = engine
        .execute_portal(&session, "create_cast_portal", 0)
        .expect("execute create cast portal");
    assert_eq!(batch.tag, "CREATE CAST");

    engine
        .with_session(&session, |record| {
            assert!(record
                .compat_user_casts
                .iter()
                .any(|cast| { cast.source_type == "text" && cast.target_type == "casttesttype" }));
            Ok(())
        })
        .expect("inspect compat cast state");
}

#[test]
fn execute_portal_supports_compat_rule_create_and_insert_rewrite() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE TABLE base_table (id INT);
             CREATE VIEW base_view AS SELECT id FROM base_table",
        )
        .expect("setup base table and view");

    engine
        .prepare(
            &session,
            "create_rule_stmt".to_owned(),
            "CREATE RULE base_view_insert AS ON INSERT TO base_view DO INSTEAD INSERT INTO base_table VALUES (new.id)"
                .to_owned(),
        )
        .expect("prepare create rule");
    engine
        .bind(
            &session,
            "create_rule_portal".to_owned(),
            "create_rule_stmt".to_owned(),
            vec![],
        )
        .expect("bind create rule");
    let create_rule_batch = engine
        .execute_portal(&session, "create_rule_portal", 0)
        .expect("execute create rule portal");
    assert_eq!(create_rule_batch.tag, "CREATE RULE");

    engine
        .prepare(
            &session,
            "insert_view_stmt".to_owned(),
            "INSERT INTO base_view VALUES (7)".to_owned(),
        )
        .expect("prepare insert into view");
    engine
        .bind(
            &session,
            "insert_view_portal".to_owned(),
            "insert_view_stmt".to_owned(),
            vec![],
        )
        .expect("bind insert into view");
    let insert_batch = engine
        .execute_portal(&session, "insert_view_portal", 0)
        .expect("execute insert into view portal");
    assert_eq!(insert_batch.tag, "INSERT");
    assert_eq!(insert_batch.rows_affected, 1);

    assert_eq!(
        query_rows(&engine, &session, "SELECT id FROM base_table ORDER BY id"),
        vec![aiondb_core::Row::new(vec![aiondb_core::Value::Int(7)])]
    );
}

#[test]
fn execute_portal_emits_drop_function_cascade_compat_notices() {
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
        .prepare(
            &session,
            "drop_fn_cascade".to_owned(),
            "DROP FUNCTION int4_casttesttype(int4) CASCADE".to_owned(),
        )
        .expect("prepare drop function cascade");
    engine
        .bind(
            &session,
            "drop_fn_cascade_portal".to_owned(),
            "drop_fn_cascade".to_owned(),
            vec![],
        )
        .expect("bind drop function cascade");
    let batch = engine
        .execute_portal(&session, "drop_fn_cascade_portal", 0)
        .expect("execute drop function cascade portal");
    assert_eq!(batch.tag, "DROP FUNCTION");

    let notices = engine
        .drain_pending_notices(&session)
        .expect("drain pending notices");
    assert!(notices
        .iter()
        .any(|message| message == "drop cascades to cast from integer to casttesttype"));
}

#[test]
fn execute_portal_supports_ctas_execute_rewrite() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(&session, "PREPARE stmt(int) AS SELECT $1 + 1 AS v")
        .expect("prepare compat statement");

    engine
        .prepare(
            &session,
            "ctas_exec_stmt".to_owned(),
            "CREATE TABLE ctas_exec_portal AS EXECUTE stmt(NULL)".to_owned(),
        )
        .expect("prepare ctas execute");
    engine
        .bind(
            &session,
            "ctas_exec_portal_stmt".to_owned(),
            "ctas_exec_stmt".to_owned(),
            vec![],
        )
        .expect("bind ctas execute");

    let batch = engine
        .execute_portal(&session, "ctas_exec_portal_stmt", 0)
        .expect("execute ctas execute portal");
    assert_eq!(batch.tag, "CREATE TABLE");

    assert_eq!(
        query_rows(&engine, &session, "SELECT v FROM ctas_exec_portal"),
        vec![aiondb_core::Row::new(vec![aiondb_core::Value::Null])]
    );
}

#[test]
fn execute_portal_emits_compat_execute_post_statement_notices() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE TYPE casttesttype;
             CREATE CAST (text AS casttesttype) WITHOUT FUNCTION;
             CREATE FUNCTION int4_casttesttype(int4) RETURNS casttesttype LANGUAGE SQL AS
             $$ SELECT ('foo'::text || $1::text)::casttesttype; $$;
             CREATE CAST (int4 AS casttesttype) WITH FUNCTION int4_casttesttype(int4) AS IMPLICIT;
             PREPARE stmt AS DROP FUNCTION int4_casttesttype(int4) CASCADE",
        )
        .expect("create compat cast dependency");

    engine
        .prepare(&session, "exec_stmt".to_owned(), "EXECUTE stmt".to_owned())
        .expect("prepare execute stmt");
    engine
        .bind(
            &session,
            "exec_stmt_portal".to_owned(),
            "exec_stmt".to_owned(),
            vec![],
        )
        .expect("bind execute stmt");

    let batch = engine
        .execute_portal(&session, "exec_stmt_portal", 0)
        .expect("execute portal execute stmt");
    assert_eq!(batch.tag, "DROP FUNCTION");

    let notices = engine
        .drain_pending_notices(&session)
        .expect("drain pending notices");
    assert!(notices
        .iter()
        .any(|message| message == "drop cascades to cast from integer to casttesttype"));
}
