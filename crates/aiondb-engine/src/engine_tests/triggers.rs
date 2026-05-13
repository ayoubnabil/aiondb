use super::*;
use aiondb_core::{DataType, Value};

// ---------------------------------------------------------------------------
// CREATE TRIGGER / DROP TRIGGER basics
// ---------------------------------------------------------------------------

#[test]
fn create_trigger_on_existing_table() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(&session, "CREATE TABLE t1 (id INT, val TEXT)")
        .expect("create table");

    engine
        .execute_sql(
            &session,
            "CREATE FUNCTION trg_fn(x INT) RETURNS INT AS 'x' LANGUAGE sql",
        )
        .expect("create function");

    let results = engine
        .execute_sql(
            &session,
            "CREATE TRIGGER my_trigger BEFORE INSERT ON t1 FOR EACH ROW EXECUTE FUNCTION trg_fn()",
        )
        .expect("create trigger");
    assert_eq!(
        results,
        vec![StatementResult::Command {
            tag: "CREATE TRIGGER".to_owned(),
            rows_affected: 0,
        }]
    );
}

#[test]
fn create_trigger_nonexistent_table_errors() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE FUNCTION trg_fn(x INT) RETURNS INT AS 'x' LANGUAGE sql",
        )
        .expect("create function");

    let err = engine
        .execute_sql(
            &session,
            "CREATE TRIGGER my_trigger BEFORE INSERT ON no_such_table FOR EACH ROW EXECUTE FUNCTION trg_fn()",
        )
        .expect_err("should fail for nonexistent table");
    assert!(
        format!("{err:?}").contains("does not exist"),
        "expected 'does not exist' error, got: {err:?}"
    );
}

#[test]
fn create_trigger_nonexistent_function_errors() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(&session, "CREATE TABLE t1 (id INT)")
        .expect("create table");

    let err = engine
        .execute_sql(
            &session,
            "CREATE TRIGGER my_trigger BEFORE INSERT ON t1 FOR EACH ROW EXECUTE FUNCTION no_such_fn()",
        )
        .expect_err("should fail for nonexistent function");
    assert!(
        format!("{err:?}").contains("does not exist"),
        "expected 'does not exist' error, got: {err:?}"
    );
}

#[test]
fn drop_trigger_removes_trigger() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(&session, "CREATE TABLE t1 (id INT)")
        .expect("create table");

    engine
        .execute_sql(
            &session,
            "CREATE FUNCTION trg_fn(x INT) RETURNS INT AS 'x' LANGUAGE sql",
        )
        .expect("create function");

    engine
        .execute_sql(
            &session,
            "CREATE TRIGGER my_trigger BEFORE INSERT ON t1 FOR EACH ROW EXECUTE FUNCTION trg_fn()",
        )
        .expect("create trigger");

    let results = engine
        .execute_sql(&session, "DROP TRIGGER my_trigger ON t1")
        .expect("drop trigger");
    assert_eq!(
        results,
        vec![StatementResult::Command {
            tag: "DROP TRIGGER".to_owned(),
            rows_affected: 0,
        }]
    );
}

#[test]
fn drop_trigger_nonexistent_errors() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(&session, "CREATE TABLE t1 (id INT)")
        .expect("create table");

    let err = engine
        .execute_sql(&session, "DROP TRIGGER no_such_trigger ON t1")
        .expect_err("should fail for nonexistent trigger");
    assert!(
        format!("{err:?}").contains("does not exist"),
        "expected 'does not exist' error, got: {err:?}"
    );
}

#[test]
fn drop_missing_trigger_on_view_fails_explicitly() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(&session, "CREATE TABLE t_base (id INT)")
        .expect("create base table");
    engine
        .execute_sql(&session, "CREATE VIEW t_view AS SELECT id FROM t_base")
        .expect("create view");

    let err = engine
        .execute_sql(&session, "DROP TRIGGER missing_trigger ON t_view")
        .expect_err("missing trigger on view must fail explicitly");
    assert!(
        format!("{err:?}").contains("does not exist"),
        "expected 'does not exist' error, got: {err:?}"
    );
}

#[test]
fn create_instead_of_trigger_on_view_rejects_until_firing_is_supported() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(&session, "CREATE TABLE trigger_view_base (id INT)")
        .expect("create base table");
    engine
        .execute_sql(
            &session,
            "CREATE VIEW trigger_view AS SELECT id FROM trigger_view_base",
        )
        .expect("create view");
    engine
        .execute_sql(
            &session,
            "CREATE FUNCTION trigger_view_fn() RETURNS trigger AS 'regress' LANGUAGE C",
        )
        .expect("create trigger function");

    let err = engine
        .execute_sql(
            &session,
            "CREATE TRIGGER trigger_view_insert \
             INSTEAD OF INSERT ON trigger_view \
             FOR EACH ROW EXECUTE FUNCTION trigger_view_fn()",
        )
        .expect_err("view trigger must fail instead of being stored as a fake success");
    assert_eq!(err.sqlstate(), aiondb_core::SqlState::FeatureNotSupported);
    assert!(err.report().message.contains("trigger on view"));
}

#[test]
fn pg_trigger_exposes_tgargs_as_null_terminated_bytea() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(&session, "CREATE TABLE t_args (id INT)")
        .expect("create table");

    engine
        .execute_sql(
            &session,
            "CREATE FUNCTION trg_args_fn(x INT) RETURNS INT AS 'x' LANGUAGE sql",
        )
        .expect("create function");

    engine
        .execute_sql(
            &session,
            "CREATE TRIGGER trg_args BEFORE INSERT ON t_args FOR EACH ROW \
             EXECUTE FUNCTION trg_args_fn('alpha', 'beta')",
        )
        .expect("create trigger with args");

    let results = engine
        .execute_sql(
            &session,
            "SELECT tgargs FROM pg_catalog.pg_trigger WHERE tgname = 'trg_args'",
        )
        .expect("query pg_trigger");

    match &results[0] {
        StatementResult::Query { columns, rows } => {
            assert_eq!(columns.len(), 1);
            assert_eq!(columns[0].data_type, DataType::Blob);
            assert_eq!(rows.len(), 1);
            assert_eq!(rows[0].values[0], Value::Blob(b"alpha\0beta\0".to_vec()));
        }
        other => panic!("expected query result, got {other:?}"),
    }
}

// ---------------------------------------------------------------------------
// BEFORE INSERT trigger
// ---------------------------------------------------------------------------

#[test]
fn before_insert_trigger_fires() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(&session, "CREATE TABLE items (n INT)")
        .expect("create table");

    // Function that returns non-NULL (allowing the insert to proceed)
    engine
        .execute_sql(
            &session,
            "CREATE FUNCTION allow_insert(n INT) RETURNS INT AS 'n' LANGUAGE sql",
        )
        .expect("create function");

    engine
        .execute_sql(
            &session,
            "CREATE TRIGGER trg_allow BEFORE INSERT ON items FOR EACH ROW EXECUTE FUNCTION allow_insert()",
        )
        .expect("create trigger");

    engine
        .execute_sql(&session, "INSERT INTO items VALUES (1), (2), (3)")
        .expect("insert");

    let results = engine
        .execute_sql(&session, "SELECT n FROM items ORDER BY n")
        .expect("select");

    match &results[0] {
        StatementResult::Query { rows, .. } => {
            let values: Vec<i32> = rows
                .iter()
                .map(|r| match &r.values[0] {
                    aiondb_core::Value::Int(v) => *v,
                    other => panic!("expected Int, got {other:?}"),
                })
                .collect();
            assert_eq!(values, vec![1, 2, 3]);
        }
        other => panic!("expected Query, got {other:?}"),
    }
}

#[test]
fn before_insert_statement_trigger_fires_once_per_statement() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE TABLE items (n INT);
             CREATE TABLE trigger_audit (v INT);
             CREATE FUNCTION log_insert_stmt() RETURNS INT AS $$
             BEGIN
               INSERT INTO trigger_audit VALUES (1);
               RETURN 1;
             END;
             $$ LANGUAGE plpgsql;
             CREATE TRIGGER trg_stmt BEFORE INSERT ON items FOR EACH STATEMENT EXECUTE FUNCTION log_insert_stmt()",
        )
        .expect("setup statement-level trigger");

    engine
        .execute_sql(&session, "INSERT INTO items VALUES (1), (2), (3)")
        .expect("insert through statement trigger");

    let results = engine
        .execute_sql(&session, "SELECT count(*)::INT FROM trigger_audit")
        .expect("count statement trigger firings");

    match &results[0] {
        StatementResult::Query { rows, .. } => {
            assert_eq!(rows.len(), 1);
            assert_eq!(rows[0].values[0], aiondb_core::Value::Int(1));
        }
        other => panic!("expected Query, got {other:?}"),
    }
}

#[test]
fn trigger_function_body_resolves_nested_user_function_via_search_path() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE SCHEMA analytics;
             SET search_path TO analytics, public;
             CREATE TABLE items (n INT);
             CREATE FUNCTION analytics.helper(n INT) RETURNS INT AS 'n' LANGUAGE sql;
             CREATE FUNCTION analytics.allow_insert(n INT) RETURNS INT AS 'helper(n)' LANGUAGE sql;
             CREATE TRIGGER trg_allow BEFORE INSERT ON items FOR EACH ROW EXECUTE FUNCTION allow_insert()",
        )
        .expect("create trigger with nested helper body");

    engine
        .execute_sql(&session, "INSERT INTO items VALUES (1), (2)")
        .expect("insert through nested helper trigger");

    let results = engine
        .execute_sql(&session, "SELECT n FROM items ORDER BY n")
        .expect("select inserted rows");
    match &results[0] {
        StatementResult::Query { rows, .. } => {
            assert_eq!(rows.len(), 2);
            assert_eq!(rows[0].values[0], aiondb_core::Value::Int(1));
            assert_eq!(rows[1].values[0], aiondb_core::Value::Int(2));
        }
        other => panic!("expected Query, got {other:?}"),
    }
}

#[test]
fn trigger_function_body_resolves_nested_user_function_via_later_search_path_schema() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE SCHEMA missing_schema;
             CREATE SCHEMA analytics;
             CREATE TABLE analytics.items (n INT);
             CREATE FUNCTION analytics.helper(n INT) RETURNS INT AS 'n' LANGUAGE sql;
             CREATE FUNCTION analytics.allow_insert(n INT) RETURNS INT AS 'helper(n)' LANGUAGE sql;
             CREATE TRIGGER trg_allow BEFORE INSERT ON analytics.items FOR EACH ROW EXECUTE FUNCTION analytics.allow_insert();
             SET search_path TO missing_schema, analytics, public",
        )
        .expect("create trigger with nested helper body and later search_path schema");

    engine
        .execute_sql(&session, "INSERT INTO analytics.items VALUES (1), (2)")
        .expect("insert through nested helper trigger via later search_path schema");

    let results = engine
        .execute_sql(&session, "SELECT n FROM analytics.items ORDER BY n")
        .expect("select inserted rows");
    match &results[0] {
        StatementResult::Query { rows, .. } => {
            assert_eq!(rows.len(), 2);
            assert_eq!(rows[0].values[0], aiondb_core::Value::Int(1));
            assert_eq!(rows[1].values[0], aiondb_core::Value::Int(2));
        }
        other => panic!("expected Query, got {other:?}"),
    }
}

#[test]
fn before_insert_trigger_returning_null_skips_row() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(&session, "CREATE TABLE items (n INT)")
        .expect("create table");

    // Function that returns NULL, which should skip the row
    engine
        .execute_sql(
            &session,
            "CREATE FUNCTION skip_insert(n INT) RETURNS INT AS 'NULL' LANGUAGE sql",
        )
        .expect("create function");

    engine
        .execute_sql(
            &session,
            "CREATE TRIGGER trg_skip BEFORE INSERT ON items FOR EACH ROW EXECUTE FUNCTION skip_insert()",
        )
        .expect("create trigger");

    let results = engine
        .execute_sql(&session, "INSERT INTO items VALUES (1), (2)")
        .expect("insert");

    // No rows should have been inserted
    assert_eq!(
        results,
        vec![StatementResult::Command {
            tag: "INSERT".to_owned(),
            rows_affected: 0,
        }]
    );

    let results = engine
        .execute_sql(&session, "SELECT n FROM items")
        .expect("select");
    match &results[0] {
        StatementResult::Query { rows, .. } => {
            assert!(rows.is_empty(), "expected no rows, got {rows:?}");
        }
        other => panic!("expected Query, got {other:?}"),
    }
}

// ---------------------------------------------------------------------------
// AFTER INSERT trigger
// ---------------------------------------------------------------------------

#[test]
fn after_insert_trigger_fires() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE TABLE items (n INT); CREATE TABLE log (v INT)",
        )
        .expect("create tables");

    // The trigger function just evaluates to non-null.
    // We can't directly test side effects of AFTER triggers with the
    // current simple function model, but we can verify the insert
    // completes without error.
    engine
        .execute_sql(
            &session,
            "CREATE FUNCTION log_insert(n INT) RETURNS INT AS 'n' LANGUAGE sql",
        )
        .expect("create function");

    engine
        .execute_sql(
            &session,
            "CREATE TRIGGER trg_log AFTER INSERT ON items FOR EACH ROW EXECUTE FUNCTION log_insert()",
        )
        .expect("create trigger");

    engine
        .execute_sql(&session, "INSERT INTO items VALUES (42)")
        .expect("insert should succeed with AFTER trigger");

    let results = engine
        .execute_sql(&session, "SELECT n FROM items")
        .expect("select");
    match &results[0] {
        StatementResult::Query { rows, .. } => {
            assert_eq!(rows.len(), 1);
            assert_eq!(rows[0].values[0], aiondb_core::Value::Int(42));
        }
        other => panic!("expected Query, got {other:?}"),
    }
}

// ---------------------------------------------------------------------------
// BEFORE UPDATE trigger
// ---------------------------------------------------------------------------

#[test]
fn before_update_trigger_returning_null_skips_update() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(&session, "CREATE TABLE items (n INT)")
        .expect("create table");

    engine
        .execute_sql(&session, "INSERT INTO items VALUES (10), (20)")
        .expect("insert");

    // Function that returns NULL -> skip the update
    engine
        .execute_sql(
            &session,
            "CREATE FUNCTION block_update(n INT) RETURNS INT AS 'NULL' LANGUAGE sql",
        )
        .expect("create function");

    engine
        .execute_sql(
            &session,
            "CREATE TRIGGER trg_block BEFORE UPDATE ON items FOR EACH ROW EXECUTE FUNCTION block_update()",
        )
        .expect("create trigger");

    let results = engine
        .execute_sql(&session, "UPDATE items SET n = 99")
        .expect("update");

    // No rows should have been updated
    assert_eq!(
        results,
        vec![StatementResult::Command {
            tag: "UPDATE".to_owned(),
            rows_affected: 0,
        }]
    );

    // Values should remain unchanged
    let results = engine
        .execute_sql(&session, "SELECT n FROM items ORDER BY n")
        .expect("select");
    match &results[0] {
        StatementResult::Query { rows, .. } => {
            let values: Vec<i32> = rows
                .iter()
                .map(|r| match &r.values[0] {
                    aiondb_core::Value::Int(v) => *v,
                    other => panic!("expected Int, got {other:?}"),
                })
                .collect();
            assert_eq!(values, vec![10, 20]);
        }
        other => panic!("expected Query, got {other:?}"),
    }
}

// ---------------------------------------------------------------------------
// AFTER DELETE trigger
// ---------------------------------------------------------------------------

#[test]
fn after_delete_trigger_fires() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(&session, "CREATE TABLE items (n INT)")
        .expect("create table");

    engine
        .execute_sql(&session, "INSERT INTO items VALUES (1), (2), (3)")
        .expect("insert");

    engine
        .execute_sql(
            &session,
            "CREATE FUNCTION after_del(n INT) RETURNS INT AS 'n' LANGUAGE sql",
        )
        .expect("create function");

    engine
        .execute_sql(
            &session,
            "CREATE TRIGGER trg_after_del AFTER DELETE ON items FOR EACH ROW EXECUTE FUNCTION after_del()",
        )
        .expect("create trigger");

    let results = engine
        .execute_sql(&session, "DELETE FROM items WHERE n = 2")
        .expect("delete");

    assert_eq!(
        results,
        vec![StatementResult::Command {
            tag: "DELETE".to_owned(),
            rows_affected: 1,
        }]
    );

    let results = engine
        .execute_sql(&session, "SELECT n FROM items ORDER BY n")
        .expect("select");
    match &results[0] {
        StatementResult::Query { rows, .. } => {
            let values: Vec<i32> = rows
                .iter()
                .map(|r| match &r.values[0] {
                    aiondb_core::Value::Int(v) => *v,
                    other => panic!("expected Int, got {other:?}"),
                })
                .collect();
            assert_eq!(values, vec![1, 3]);
        }
        other => panic!("expected Query, got {other:?}"),
    }
}

// ---------------------------------------------------------------------------
// DROP TRIGGER removes effect
// ---------------------------------------------------------------------------

#[test]
fn drop_trigger_removes_trigger_effect() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(&session, "CREATE TABLE items (n INT)")
        .expect("create table");

    // Create a BEFORE INSERT trigger that blocks all inserts
    engine
        .execute_sql(
            &session,
            "CREATE FUNCTION block_fn(n INT) RETURNS INT AS 'NULL' LANGUAGE sql",
        )
        .expect("create function");

    engine
        .execute_sql(
            &session,
            "CREATE TRIGGER trg_block BEFORE INSERT ON items FOR EACH ROW EXECUTE FUNCTION block_fn()",
        )
        .expect("create trigger");

    // Insert should be blocked
    let results = engine
        .execute_sql(&session, "INSERT INTO items VALUES (1)")
        .expect("insert");
    assert_eq!(
        results,
        vec![StatementResult::Command {
            tag: "INSERT".to_owned(),
            rows_affected: 0,
        }]
    );

    // Drop the trigger
    engine
        .execute_sql(&session, "DROP TRIGGER trg_block ON items")
        .expect("drop trigger");

    // Insert should now succeed
    let results = engine
        .execute_sql(&session, "INSERT INTO items VALUES (2)")
        .expect("insert after drop");
    assert_eq!(
        results,
        vec![StatementResult::Command {
            tag: "INSERT".to_owned(),
            rows_affected: 1,
        }]
    );

    let results = engine
        .execute_sql(&session, "SELECT n FROM items")
        .expect("select");
    match &results[0] {
        StatementResult::Query { rows, .. } => {
            assert_eq!(rows.len(), 1);
            assert_eq!(rows[0].values[0], aiondb_core::Value::Int(2));
        }
        other => panic!("expected Query, got {other:?}"),
    }
}

#[test]
fn trigger_resolution_uses_schema_scoped_relation_key() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE SCHEMA analytics;
             CREATE TABLE public.items (n INT);
             CREATE TABLE analytics.items (n INT);
             CREATE FUNCTION block_fn(n INT) RETURNS INT AS 'NULL' LANGUAGE sql;
             SET search_path TO analytics, public;
             CREATE TRIGGER trg_block BEFORE INSERT ON items FOR EACH ROW EXECUTE FUNCTION block_fn()",
        )
        .expect("setup triggers on homonymous tables");

    let public_insert = engine
        .execute_sql(&session, "INSERT INTO public.items VALUES (1)")
        .expect("public insert");
    assert_eq!(
        public_insert,
        vec![StatementResult::Command {
            tag: "INSERT".to_owned(),
            rows_affected: 1,
        }]
    );

    let analytics_insert = engine
        .execute_sql(&session, "INSERT INTO analytics.items VALUES (2)")
        .expect("analytics insert");
    assert_eq!(
        analytics_insert,
        vec![StatementResult::Command {
            tag: "INSERT".to_owned(),
            rows_affected: 0,
        }]
    );
}

#[test]
fn triggers_with_same_name_can_exist_on_same_table_name_in_different_schemas() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE SCHEMA analytics;
             CREATE TABLE public.items (n INT);
             CREATE TABLE analytics.items (n INT);
             CREATE FUNCTION allow_fn(n INT) RETURNS INT AS 'n' LANGUAGE sql;
             CREATE TRIGGER trg_same BEFORE INSERT ON public.items FOR EACH ROW EXECUTE FUNCTION allow_fn();
             CREATE TRIGGER trg_same BEFORE INSERT ON analytics.items FOR EACH ROW EXECUTE FUNCTION allow_fn()",
        )
        .expect("same trigger name should be allowed on homonymous cross-schema tables");
}

#[test]
fn drop_trigger_resolves_unqualified_table_via_search_path() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE SCHEMA analytics;
             CREATE TABLE public.items (n INT);
             CREATE TABLE analytics.items (n INT);
             CREATE FUNCTION block_fn(n INT) RETURNS INT AS 'NULL' LANGUAGE sql;
             SET search_path TO analytics, public;
             CREATE TRIGGER trg_block BEFORE INSERT ON items FOR EACH ROW EXECUTE FUNCTION block_fn();
             DROP TRIGGER trg_block ON items",
        )
        .expect("drop trigger via search_path");

    let analytics_insert = engine
        .execute_sql(&session, "INSERT INTO analytics.items VALUES (2)")
        .expect("analytics insert after drop");
    assert_eq!(
        analytics_insert,
        vec![StatementResult::Command {
            tag: "INSERT".to_owned(),
            rows_affected: 1,
        }]
    );
}

#[test]
fn alter_trigger_rename_resolves_unqualified_table_via_search_path() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE SCHEMA analytics;
             CREATE TABLE analytics.items (n INT);
             CREATE FUNCTION block_fn(n INT) RETURNS INT AS 'NULL' LANGUAGE sql;
             SET search_path TO analytics, public;
             CREATE TRIGGER trg_block BEFORE INSERT ON items FOR EACH ROW EXECUTE FUNCTION block_fn();
             ALTER TRIGGER trg_block ON items RENAME TO trg_block_renamed;
             DROP TRIGGER trg_block_renamed ON items",
        )
        .expect("rename and drop trigger via search_path");

    let analytics_insert = engine
        .execute_sql(&session, "INSERT INTO analytics.items VALUES (2)")
        .expect("analytics insert after rename+drop");
    assert_eq!(
        analytics_insert,
        vec![StatementResult::Command {
            tag: "INSERT".to_owned(),
            rows_affected: 1,
        }]
    );
}

#[test]
fn trigger_executes_schema_qualified_function() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE SCHEMA analytics;
             CREATE TABLE items (n INT);
             CREATE FUNCTION analytics.block_fn(n INT) RETURNS INT AS 'NULL' LANGUAGE sql;
             CREATE TRIGGER trg_block BEFORE INSERT ON items FOR EACH ROW EXECUTE FUNCTION analytics.block_fn()",
        )
        .expect("create schema-qualified trigger function");

    let insert = engine
        .execute_sql(&session, "INSERT INTO items VALUES (1)")
        .expect("insert should be vetoed by trigger");
    assert_eq!(
        insert,
        vec![StatementResult::Command {
            tag: "INSERT".to_owned(),
            rows_affected: 0,
        }]
    );
}

#[test]
fn create_trigger_resolves_unqualified_function_via_search_path() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE SCHEMA analytics;
             CREATE TABLE analytics.items (n INT);
             CREATE FUNCTION analytics.block_fn(n INT) RETURNS INT AS 'NULL' LANGUAGE sql;
             SET search_path TO analytics, public;
             CREATE TRIGGER trg_block BEFORE INSERT ON items FOR EACH ROW EXECUTE FUNCTION block_fn()",
        )
        .expect("create trigger via search_path");

    let insert = engine
        .execute_sql(&session, "INSERT INTO analytics.items VALUES (1)")
        .expect("insert should be vetoed by trigger");
    assert_eq!(
        insert,
        vec![StatementResult::Command {
            tag: "INSERT".to_owned(),
            rows_affected: 0,
        }]
    );
}

#[test]
fn before_row_trigger_tuple_return_flows_to_next_trigger() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(&session, "CREATE TABLE trigtest (f1 INT, f2 TEXT)")
        .expect("create table");

    engine
        .execute_sql(
            &session,
            "CREATE FUNCTION trigger_return_old() RETURNS trigger AS 'regress' LANGUAGE C",
        )
        .expect("create C trigger helper");

    engine
        .execute_sql(
            &session,
            "CREATE FUNCTION f1_times_10() RETURNS trigger AS $$ \
             BEGIN NEW.f1 := NEW.f1 * 10; RETURN NEW; END \
             $$ LANGUAGE plpgsql",
        )
        .expect("create plpgsql trigger helper");

    engine
        .execute_sql(
            &session,
            "CREATE TRIGGER trigger_return_old \
             BEFORE INSERT OR DELETE OR UPDATE ON trigtest \
             FOR EACH ROW EXECUTE FUNCTION trigger_return_old()",
        )
        .expect("create trigger_return_old trigger");
    engine
        .execute_sql(
            &session,
            "CREATE TRIGGER trigger_alpha \
             BEFORE INSERT OR UPDATE ON trigtest \
             FOR EACH ROW EXECUTE FUNCTION f1_times_10()",
        )
        .expect("create alpha trigger");
    engine
        .execute_sql(
            &session,
            "CREATE TRIGGER trigger_zed \
             BEFORE INSERT OR UPDATE ON trigtest \
             FOR EACH ROW EXECUTE FUNCTION f1_times_10()",
        )
        .expect("create zed trigger");

    engine
        .execute_sql(&session, "INSERT INTO trigtest VALUES (1, 'foo')")
        .expect("insert row");
    let after_insert = engine
        .execute_sql(&session, "SELECT f1 FROM trigtest")
        .expect("select after insert");
    match &after_insert[0] {
        StatementResult::Query { rows, .. } => {
            assert_eq!(rows.len(), 1);
            assert_eq!(rows[0].values[0], Value::Int(100));
        }
        other => panic!("expected Query, got {other:?}"),
    }

    engine
        .execute_sql(&session, "UPDATE trigtest SET f2 = f2 || 'bar'")
        .expect("update row");
    let after_update = engine
        .execute_sql(&session, "SELECT f1 FROM trigtest")
        .expect("select after update");
    match &after_update[0] {
        StatementResult::Query { rows, .. } => {
            assert_eq!(rows.len(), 1);
            assert_eq!(rows[0].values[0], Value::Int(1000));
        }
        other => panic!("expected Query, got {other:?}"),
    }
}

#[test]
fn suppress_redundant_updates_trigger_skips_identical_updates() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE TABLE min_updates_test (f1 TEXT, f2 INT, f3 INT);
             INSERT INTO min_updates_test VALUES ('a', 1, 2)",
        )
        .expect("setup table");

    engine
        .execute_sql(
            &session,
            "CREATE FUNCTION suppress_redundant_updates_trigger() \
             RETURNS trigger AS 'regress' LANGUAGE C",
        )
        .expect("create helper function");
    engine
        .execute_sql(
            &session,
            "CREATE TRIGGER z_min_update
             BEFORE UPDATE ON min_updates_test
             FOR EACH ROW EXECUTE FUNCTION suppress_redundant_updates_trigger()",
        )
        .expect("create trigger");

    let no_op_update = engine
        .execute_sql(&session, "UPDATE min_updates_test SET f1 = f1")
        .expect("no-op update");
    assert_eq!(
        no_op_update,
        vec![StatementResult::Command {
            tag: "UPDATE".to_owned(),
            rows_affected: 0,
        }]
    );

    let changed_update = engine
        .execute_sql(&session, "UPDATE min_updates_test SET f2 = f2 + 1")
        .expect("changed update");
    assert_eq!(
        changed_update,
        vec![StatementResult::Command {
            tag: "UPDATE".to_owned(),
            rows_affected: 1,
        }]
    );

    let rows = engine
        .execute_sql(&session, "SELECT f1, f2, f3 FROM min_updates_test")
        .expect("select final rows");
    match &rows[0] {
        StatementResult::Query { rows, .. } => {
            assert_eq!(rows.len(), 1);
            assert_eq!(rows[0].values[0], Value::Text("a".to_owned()));
            assert_eq!(rows[0].values[1], Value::Int(2));
            assert_eq!(rows[0].values[2], Value::Int(2));
        }
        other => panic!("expected Query, got {other:?}"),
    }
}
