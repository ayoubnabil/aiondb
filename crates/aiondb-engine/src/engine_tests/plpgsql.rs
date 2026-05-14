use super::*;

#[test]
fn do_block_if_elsif_else_executes_matching_branch() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let results = engine
        .execute_sql(
            &session,
            "DO $$
DECLARE
  x int := 2;
  y int := 0;
BEGIN
  IF x = 1 THEN
    y := 10;
  ELSIF x = 2 THEN
    y := 20;
  ELSE
    y := 30;
  END IF;
  RAISE NOTICE '%', y;
END
$$ LANGUAGE plpgsql;",
        )
        .expect("do block with IF/ELSIF/ELSE should succeed");

    assert_eq!(results.len(), 2);
    assert!(matches!(
        &results[0],
        StatementResult::Notice { message } if message == "20"
    ));
    assert!(matches!(
        &results[1],
        StatementResult::Command { tag, rows_affected } if tag == "DO" && *rows_affected == 0
    ));
}

#[test]
fn do_block_unknown_variable_reports_undefined_object() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let error = engine
        .execute_sql(
            &session,
            "DO $$
BEGIN
  missing_var := 1;
END
$$ LANGUAGE plpgsql;",
        )
        .expect_err("unknown DO variable should fail");

    assert_eq!(error.sqlstate(), aiondb_core::SqlState::UndefinedObject);
    assert!(error.report().message.contains("unknown DO variable"));
}

#[test]
fn do_block_declared_initializer_is_cast_to_declared_type() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let results = engine
        .execute_sql(
            &session,
            "DO $$
DECLARE
  x int := '42';
BEGIN
  x := x + 1;
  RAISE NOTICE '%', x;
END
$$ LANGUAGE plpgsql;",
        )
        .expect("typed PL/pgSQL variable initializer should cast before use");

    assert!(matches!(
        &results[0],
        StatementResult::Notice { message } if message == "43"
    ));
}

#[test]
fn do_block_can_be_followed_by_select_in_same_sql_batch() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let results = engine
        .execute_sql(
            &session,
            "DO $$ BEGIN RAISE NOTICE '%', 'batched'; END $$ LANGUAGE plpgsql; SELECT 1;",
        )
        .expect("DO + SELECT batch should succeed");

    assert_eq!(results.len(), 3);
    assert!(matches!(
        &results[0], StatementResult::Notice { message } if message == "batched"
    ));
    assert!(matches!(
        &results[1],
        StatementResult::Command { tag, rows_affected } if tag == "DO" && *rows_affected == 0
    ));
    assert!(matches!(
        &results[2],
        StatementResult::Query { rows, .. }
            if rows.len() == 1 && rows[0].values == vec![Value::Int(1)]
    ));
}

#[test]
fn hash_join_batches_compat_query_keeps_explicit_transaction_active() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(&session, "CREATE TABLE tx_plpgsql_probe (id INT)")
        .expect("create table");
    engine
        .execute_sql(
            &session,
            "INSERT INTO tx_plpgsql_probe VALUES (1), (2), (3)",
        )
        .expect("seed rows");

    engine.execute_sql(&session, "BEGIN").expect("begin");
    engine
        .execute_sql(&session, "SAVEPOINT settings")
        .expect("savepoint settings");
    engine
        .execute_sql(
            &session,
            "SELECT original > 1 AS initially_multibatch, final > original AS increased_batches \
             FROM hash_join_batches($$ \
               SELECT count(*) FROM tx_plpgsql_probe r JOIN tx_plpgsql_probe s USING (id); \
             $$)",
        )
        .expect("hash_join_batches compat query should succeed");
    engine
        .execute_sql(&session, "ROLLBACK TO SAVEPOINT settings")
        .expect("transaction should remain active after hash_join_batches query");
    engine.execute_sql(&session, "ROLLBACK").expect("rollback");
}

#[test]
fn explain_parallel_sort_stats_compat_query_executes_without_srf_parse_error() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(&session, "CREATE TABLE tenk1 (ten INT)")
        .expect("create tenk1");
    engine
        .execute_sql(&session, "INSERT INTO tenk1 SELECT generate_series(1, 200)")
        .expect("seed tenk1");

    let results = engine
        .execute_sql(&session, "SELECT * FROM explain_parallel_sort_stats()")
        .expect("compat query should bypass SRF parse rejection");
    let StatementResult::Query { rows, .. } = &results[0] else {
        panic!("expected query result");
    };
    assert!(
        !rows.is_empty(),
        "compat explain_parallel_sort_stats should return plan rows"
    );
}
