use aiondb_core::Value;

use super::*;

// ---------------------------------------------------------------
// Tests
// ---------------------------------------------------------------

#[test]
fn vacuum_empty_table() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let results = engine
        .execute_sql(
            &session,
            "CREATE TABLE empty_v (id INT, name TEXT); \
             VACUUM empty_v",
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
                tag: "VACUUM".to_owned(),
                rows_affected: 0,
            },
        ]
    );
}

#[test]
fn vacuum_after_delete() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let results = engine
        .execute_sql(
            &session,
            "CREATE TABLE vac_del (id INT NOT NULL, label TEXT); \
             INSERT INTO vac_del VALUES (1, 'a'), (2, 'b'), (3, 'c'); \
             DELETE FROM vac_del WHERE id = 2; \
             VACUUM vac_del",
        )
        .expect("execute");

    // The VACUUM should report the dead tuple(s) removed.
    let vacuum_result = &results[3];
    assert_eq!(
        vacuum_result,
        &StatementResult::Command {
            tag: "VACUUM".to_owned(),
            rows_affected: 1,
        }
    );
}

#[test]
fn vacuum_all_dead() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE TABLE vac_all (id INT NOT NULL); \
             INSERT INTO vac_all VALUES (1), (2), (3); \
             DELETE FROM vac_all WHERE true",
        )
        .expect("setup");

    let results = engine
        .execute_sql(&session, "VACUUM vac_all")
        .expect("vacuum");

    assert_eq!(
        results,
        vec![StatementResult::Command {
            tag: "VACUUM".to_owned(),
            rows_affected: 3,
        }]
    );

    // Table should now be completely empty.
    let rows = query_rows(&engine, &session, "SELECT count(*) FROM vac_all");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].values[0], Value::BigInt(0));
}

#[test]
fn vacuum_preserves_live_rows() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE TABLE vac_live (id INT NOT NULL); \
             INSERT INTO vac_live VALUES (1), (2), (3), (4), (5); \
             DELETE FROM vac_live WHERE id IN (2, 4)",
        )
        .expect("setup");

    engine
        .execute_sql(&session, "VACUUM vac_live")
        .expect("vacuum");

    let rows = query_rows(&engine, &session, "SELECT count(*) FROM vac_live");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].values[0], Value::BigInt(3));
}

#[test]
fn vacuum_after_update() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE TABLE vac_upd (id INT NOT NULL, val TEXT); \
             INSERT INTO vac_upd VALUES (1, 'old'); \
             UPDATE vac_upd SET val = 'new' WHERE id = 1",
        )
        .expect("setup");

    let results = engine
        .execute_sql(&session, "VACUUM vac_upd")
        .expect("vacuum");

    // The UPDATE creates a dead version of the old row.
    assert_eq!(
        results,
        vec![StatementResult::Command {
            tag: "VACUUM".to_owned(),
            rows_affected: 1,
        }]
    );

    // The live row should still reflect the updated value.
    let rows = query_rows(&engine, &session, "SELECT val FROM vac_upd");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].values[0], Value::Text("new".to_owned()));
}

#[test]
fn vacuum_idempotent() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE TABLE vac_idem (id INT NOT NULL); \
             INSERT INTO vac_idem VALUES (1), (2); \
             DELETE FROM vac_idem WHERE id = 1",
        )
        .expect("setup");

    // First VACUUM cleans the dead tuple.
    let results1 = engine
        .execute_sql(&session, "VACUUM vac_idem")
        .expect("vacuum 1");
    assert_eq!(
        results1,
        vec![StatementResult::Command {
            tag: "VACUUM".to_owned(),
            rows_affected: 1,
        }]
    );

    // Second VACUUM finds nothing to clean.
    let results2 = engine
        .execute_sql(&session, "VACUUM vac_idem")
        .expect("vacuum 2");
    assert_eq!(
        results2,
        vec![StatementResult::Command {
            tag: "VACUUM".to_owned(),
            rows_affected: 0,
        }]
    );
}

#[test]
fn vacuum_nonexistent_table() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let error = engine
        .execute_sql(&session, "VACUUM no_such_table")
        .expect_err("should fail for non-existent table");

    assert_eq!(error.sqlstate(), aiondb_core::SqlState::UndefinedTable,);
}
