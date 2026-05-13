use super::*;

#[test]
fn lock_table_default_mode_inside_txn_succeeds() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");
    let _ = engine.execute_sql(&session, "CREATE ROLE alice SUPERUSER LOGIN");

    engine
        .execute_sql(&session, "CREATE TABLE t (id INT);")
        .expect("create");

    engine.execute_sql(&session, "BEGIN;").expect("BEGIN");
    let results = engine
        .execute_sql(&session, "LOCK TABLE t;")
        .expect("LOCK TABLE default");
    assert!(
        matches!(&results[0], StatementResult::Command { tag, .. } if tag == "LOCK TABLE"),
        "expected LOCK TABLE tag, got {:?}",
        results[0]
    );
    engine.execute_sql(&session, "COMMIT;").expect("COMMIT");
}

#[test]
fn lock_table_explicit_mode_inside_txn_succeeds() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");
    let _ = engine.execute_sql(&session, "CREATE ROLE alice SUPERUSER LOGIN");

    engine
        .execute_sql(&session, "CREATE TABLE t (id INT);")
        .expect("create");

    engine.execute_sql(&session, "BEGIN;").expect("BEGIN");
    engine
        .execute_sql(&session, "LOCK TABLE t IN ACCESS SHARE MODE;")
        .expect("LOCK TABLE ACCESS SHARE");
    engine
        .execute_sql(&session, "LOCK TABLE t IN EXCLUSIVE MODE;")
        .expect("LOCK TABLE EXCLUSIVE");
    engine
        .execute_sql(&session, "LOCK TABLE t IN SHARE UPDATE EXCLUSIVE MODE;")
        .expect("LOCK TABLE SHARE UPDATE EXCLUSIVE");
    engine.execute_sql(&session, "COMMIT;").expect("COMMIT");
}

#[test]
fn lock_table_multiple_tables_in_one_statement() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");
    let _ = engine.execute_sql(&session, "CREATE ROLE alice SUPERUSER LOGIN");

    engine
        .execute_sql(&session, "CREATE TABLE a (id INT);")
        .expect("create a");
    engine
        .execute_sql(&session, "CREATE TABLE b (id INT);")
        .expect("create b");
    engine
        .execute_sql(&session, "CREATE TABLE c (id INT);")
        .expect("create c");

    engine.execute_sql(&session, "BEGIN;").expect("BEGIN");
    engine
        .execute_sql(&session, "LOCK TABLE a, b, c IN SHARE MODE;")
        .expect("LOCK multiple");
    engine.execute_sql(&session, "COMMIT;").expect("COMMIT");
}

#[test]
fn lock_on_missing_table_errors() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");
    let _ = engine.execute_sql(&session, "CREATE ROLE alice SUPERUSER LOGIN");

    engine.execute_sql(&session, "BEGIN;").expect("BEGIN");
    let err = engine
        .execute_sql(&session, "LOCK TABLE does_not_exist;")
        .expect_err("unknown table");
    let message = err.report().message.to_ascii_lowercase();
    assert!(
        message.contains("does_not_exist") || message.contains("does not exist"),
        "expected undefined-table error, got: {}",
        err.report().message
    );
    let _ = engine.execute_sql(&session, "ROLLBACK;");
}

#[test]
fn lock_nowait_conflict_errors_immediately() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (owner, _) = engine.startup(startup_params()).expect("startup owner");
    let _ = engine.execute_sql(&owner, "CREATE ROLE alice SUPERUSER LOGIN");
    let (contender, _) = engine.startup(startup_params()).expect("startup contender");

    engine
        .execute_sql(&owner, "CREATE TABLE t (id INT);")
        .expect("create");

    engine.execute_sql(&owner, "BEGIN;").expect("BEGIN owner");
    engine
        .execute_sql(&owner, "LOCK TABLE t IN ACCESS EXCLUSIVE MODE;")
        .expect("owner holds exclusive");

    engine
        .execute_sql(&contender, "BEGIN;")
        .expect("BEGIN contender");
    let err = engine
        .execute_sql(&contender, "LOCK TABLE t IN ACCESS EXCLUSIVE MODE NOWAIT;")
        .expect_err("NOWAIT conflict should error");
    assert!(
        !err.report().message.is_empty(),
        "expected non-empty lock-conflict error",
    );

    engine
        .execute_sql(&contender, "ROLLBACK;")
        .expect("rollback contender");
    engine.execute_sql(&owner, "COMMIT;").expect("commit owner");
}
