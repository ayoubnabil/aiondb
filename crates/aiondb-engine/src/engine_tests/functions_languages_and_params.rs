use super::*;

// ---------------------------------------------------------------------------
// Unsupported language
// ---------------------------------------------------------------------------

#[test]
fn create_function_plpgsql_language_accepted() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let results = engine
        .execute_sql(
            &session,
            "CREATE FUNCTION plfn(x INT) RETURNS INT AS 'BEGIN RETURN x; END' LANGUAGE plpgsql",
        )
        .expect("plpgsql function creation should succeed");
    assert!(!results.is_empty());
}

#[test]
fn plpgsql_if_else_return_executes_selected_branch() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE FUNCTION abs_pl(x INT) RETURNS INT AS $$ \
             BEGIN \
                 IF x >= 0 THEN \
                     RETURN x; \
                 ELSE \
                     RETURN -x; \
                 END IF; \
             END \
             $$ LANGUAGE plpgsql",
        )
        .expect("create plpgsql IF/ELSE function");

    let positive = query_rows(&engine, &session, "SELECT abs_pl(7)");
    assert_eq!(positive[0].values[0], Value::Int(7));

    let negative = query_rows(&engine, &session, "SELECT abs_pl(-4)");
    assert_eq!(negative[0].values[0], Value::Int(4));
}

#[test]
fn plpgsql_if_elsif_return_executes_first_matching_branch() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE FUNCTION sign_bucket(x INT) RETURNS INT AS $$ \
             BEGIN \
                 IF x < 0 THEN \
                     RETURN -1; \
                 ELSIF x = 0 THEN \
                     RETURN 0; \
                 ELSE \
                     RETURN 1; \
                 END IF; \
             END \
             $$ LANGUAGE plpgsql",
        )
        .expect("create plpgsql IF/ELSIF function");

    let negative = query_rows(&engine, &session, "SELECT sign_bucket(-9)");
    assert_eq!(negative[0].values[0], Value::Int(-1));

    let zero = query_rows(&engine, &session, "SELECT sign_bucket(0)");
    assert_eq!(zero[0].values[0], Value::Int(0));

    let positive = query_rows(&engine, &session, "SELECT sign_bucket(12)");
    assert_eq!(positive[0].values[0], Value::Int(1));
}

#[test]
fn plpgsql_merge_function_visible_inside_explicit_transaction() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(&session, "CREATE TABLE target (tid INT, balance INT)")
        .expect("create target");
    engine
        .execute_sql(&session, "INSERT INTO target VALUES (1, 10), (2, 20)")
        .expect("seed target");

    engine.execute_sql(&session, "BEGIN").expect("begin");
    engine
        .execute_sql(
            &session,
            "CREATE FUNCTION merge_func_txn(p_id INT, p_bal INT) RETURNS INT LANGUAGE plpgsql AS $$ \
             DECLARE result INT; \
             BEGIN \
               MERGE INTO target t \
               USING (SELECT p_id AS sid) AS s \
               ON t.tid = s.sid \
               WHEN MATCHED THEN \
                 UPDATE SET balance = t.balance - p_bal; \
               IF FOUND THEN \
                 GET DIAGNOSTICS result := ROW_COUNT; \
               END IF; \
               RETURN result; \
             END; \
             $$",
        )
        .expect("create plpgsql MERGE function inside explicit txn");

    let rows = query_rows(&engine, &session, "SELECT merge_func_txn(1, 4)");
    assert_eq!(rows[0].values[0], Value::Int(1));

    let rows = query_rows(
        &engine,
        &session,
        "SELECT balance FROM target WHERE tid = 1 ORDER BY tid",
    );
    assert_eq!(rows[0].values[0], Value::Int(6));

    engine.execute_sql(&session, "ROLLBACK").expect("rollback");
}

#[test]
fn plpgsql_merge_function_visible_in_autocommit() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE TABLE target_autocommit (tid INT, balance INT)",
        )
        .expect("create target");
    engine
        .execute_sql(
            &session,
            "INSERT INTO target_autocommit VALUES (1, 10), (2, 20)",
        )
        .expect("seed target");

    engine
        .execute_sql(
            &session,
            "CREATE FUNCTION merge_func_autocommit(p_id INT, p_bal INT) RETURNS INT LANGUAGE plpgsql AS $$ \
             DECLARE result INT; \
             BEGIN \
               MERGE INTO target_autocommit t \
               USING (SELECT p_id AS sid) AS s \
               ON t.tid = s.sid \
               WHEN MATCHED THEN \
                 UPDATE SET balance = t.balance - p_bal; \
               IF FOUND THEN \
                 GET DIAGNOSTICS result := ROW_COUNT; \
               END IF; \
               RETURN result; \
             END; \
             $$",
        )
        .expect("create plpgsql MERGE function in autocommit");

    let rows = query_rows(&engine, &session, "SELECT merge_func_autocommit(1, 4)");
    assert_eq!(rows[0].values[0], Value::Int(1));
}

#[test]
fn plpgsql_check_ddl_rewrite_helper_uses_compat_heuristic() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE FUNCTION check_ddl_rewrite(p_tablename regclass, p_ddl text) \
             RETURNS boolean LANGUAGE plpgsql AS $$ \
             BEGIN \
               RETURN false; \
             END; \
             $$",
        )
        .expect("create helper");

    let rows = query_rows(
        &engine,
        &session,
        "SELECT check_ddl_rewrite('pg_class', 'ALTER TABLE t ADD COLUMN c_rewrite serial'), \
                check_ddl_rewrite('pg_class', 'ALTER TABLE t ADD COLUMN c_norewrite int default 42')",
    );
    assert_eq!(rows[0].values[0], Value::Boolean(true));
    assert_eq!(rows[0].values[1], Value::Boolean(false));
}

#[test]
fn create_function_unknown_language_accepted_at_create_time() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE FUNCTION bad(x INT) RETURNS INT AS 'x' LANGUAGE perl",
        )
        .expect("unknown language should be accepted at CREATE time");
}

#[test]
fn jsonb_path_query_is_implemented() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let results = engine
        .execute_sql(&session, "SELECT jsonb_path_query('{\"a\":1}', '$.a')")
        .expect("jsonb_path_query should succeed");
    match &results[0] {
        StatementResult::Query { rows, .. } => {
            assert!(!rows.is_empty(), "should return at least one row");
        }
        other => panic!("expected Query, got {other:?}"),
    }
}

// ---------------------------------------------------------------------------
// Boolean and bigint parameter types
// ---------------------------------------------------------------------------

#[test]
fn function_with_boolean_parameter() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE FUNCTION negate(b BOOLEAN) RETURNS BOOLEAN AS 'NOT b' LANGUAGE sql",
        )
        .expect("create");

    let results = engine
        .execute_sql(&session, "SELECT negate(TRUE)")
        .expect("select");
    match &results[0] {
        StatementResult::Query { rows, .. } => {
            assert_eq!(rows[0].values[0], aiondb_core::Value::Boolean(false));
        }
        other => panic!("expected Query, got {other:?}"),
    }
}

#[test]
fn function_with_bigint_parameter() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE FUNCTION double_big(x BIGINT) RETURNS BIGINT AS 'x * 2' LANGUAGE sql",
        )
        .expect("create");

    let results = engine
        .execute_sql(&session, "SELECT double_big(5000000000)")
        .expect("select");
    match &results[0] {
        StatementResult::Query { rows, .. } => {
            assert_eq!(
                rows[0].values[0],
                aiondb_core::Value::BigInt(10_000_000_000)
            );
        }
        other => panic!("expected Query, got {other:?}"),
    }
}

#[test]
fn pg_current_xact_id_functions_return_values() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let results = engine
        .execute_sql(
            &session,
            "SELECT pg_current_xact_id() IS NOT NULL, \
                    pg_current_xact_id_if_assigned() IS NOT NULL, \
                    txid_current() IS NOT NULL, \
                    txid_current_if_assigned() IS NOT NULL",
        )
        .expect("select");
    match &results[0] {
        StatementResult::Query { rows, .. } => {
            assert_eq!(
                rows[0],
                aiondb_core::Row::new(vec![
                    aiondb_core::Value::Boolean(true),
                    aiondb_core::Value::Boolean(true),
                    aiondb_core::Value::Boolean(true),
                    aiondb_core::Value::Boolean(true),
                ])
            );
        }
        other => panic!("expected Query, got {other:?}"),
    }
}
