use super::*;
use aiondb_core::{SqlState, Value};

#[test]
fn compatibility_stub_command_fails_instead_of_succeeding_as_noop() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let error = engine
        .execute_sql(&session, "LOCK TABLE pg_class IN ACCESS SHARE MODE")
        .expect_err("LOCK should fail instead of reporting a fake success");
    let report = error.report();
    assert_eq!(report.sqlstate, SqlState::FeatureNotSupported);
    assert!(
        report.message.contains("LOCK"),
        "unexpected message: {}",
        report.message
    );
}

#[test]
fn database_compatibility_commands_execute_with_minimal_pg_semantics() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let create = engine
        .execute_sql(
            &session,
            "CREATE DATABASE regression_tbd ENCODING utf8 LC_COLLATE 'C' LC_CTYPE 'C' TEMPLATE template0",
        )
        .expect("create database should execute");
    assert!(matches!(
        create.first(),
        Some(StatementResult::Command { tag, .. }) if tag == "CREATE DATABASE"
    ));

    let alter_rename = engine
        .execute_sql(
            &session,
            "ALTER DATABASE regression_tbd RENAME TO regression_utf8",
        )
        .expect("alter database rename should execute");
    assert!(matches!(
        alter_rename.first(),
        Some(StatementResult::Command { tag, .. }) if tag == "ALTER DATABASE"
    ));

    engine
        .execute_sql(
            &session,
            "ALTER DATABASE regression_utf8 SET TABLESPACE regress_tblspace",
        )
        .expect("alter database set tablespace");
    engine
        .execute_sql(&session, "ALTER DATABASE regression_utf8 RESET TABLESPACE")
        .expect("alter database reset tablespace");
    engine
        .execute_sql(
            &session,
            "ALTER DATABASE regression_utf8 CONNECTION_LIMIT 123",
        )
        .expect("alter database connection limit");

    let mut new_db_params = startup_params();
    new_db_params.database = "regression_utf8".to_owned();
    let mut dropped_db_params = startup_params();
    dropped_db_params.database = "regression_utf8".to_owned();
    let (new_db_session, _) = engine
        .startup(new_db_params)
        .expect("startup on created database");
    engine
        .terminate(new_db_session)
        .expect("terminate created-database session");

    let drop = engine
        .execute_sql(&session, "DROP DATABASE regression_utf8")
        .expect("drop database should execute");
    assert!(matches!(
        drop.first(),
        Some(StatementResult::Command { tag, .. }) if tag == "DROP DATABASE"
    ));

    let err = engine
        .startup(dropped_db_params)
        .expect_err("dropped database should no longer be connectable");
    assert_eq!(err.sqlstate(), SqlState::InvalidCatalogName);
}

#[test]
fn drop_database_cascades_physical_schema_contents() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (admin, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(&admin, "CREATE DATABASE dropdb_cascade_probe")
        .expect("create database");

    let mut params = startup_params();
    params.database = "dropdb_cascade_probe".to_owned();
    let (probe, _) = engine.startup(params).expect("startup probe database");
    engine
        .execute_sql(
            &probe,
            "CREATE TABLE probe_items (id INT PRIMARY KEY); \
             CREATE VIEW probe_view AS SELECT id FROM probe_items; \
             CREATE SEQUENCE probe_seq",
        )
        .expect("create schema objects in probe database");
    engine.terminate(probe).expect("terminate probe session");

    let drop = engine
        .execute_sql(&admin, "DROP DATABASE dropdb_cascade_probe")
        .expect("drop database should cascade physical schema");
    assert!(matches!(
        drop.first(),
        Some(StatementResult::Command { tag, .. }) if tag == "DROP DATABASE"
    ));

    let schema = engine
        .catalog_reader
        .get_schema(
            aiondb_core::TxnId::new(0),
            &aiondb_catalog::QualifiedName::unqualified("db_dropdb_cascade_probe"),
        )
        .expect("catalog lookup");
    assert!(schema.is_none(), "physical schema should be gone");
}

#[test]
fn alter_database_routes_through_cluster_catalog() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(&session, "CREATE DATABASE adr0014_alter OWNER aiondb")
        .expect("create database");

    let pre = engine
        .cluster_catalog()
        .get_database_by_name("adr0014_alter")
        .expect("cluster lookup")
        .expect("database registered");
    let db_id = pre.id;

    engine
        .execute_sql(&session, "ALTER DATABASE adr0014_alter OWNER TO newowner")
        .expect("alter database owner");
    engine
        .execute_sql(&session, "ALTER DATABASE adr0014_alter CONNECTION LIMIT 17")
        .expect("alter database connection limit (two-token spelling)");
    engine
        .execute_sql(
            &session,
            "ALTER DATABASE adr0014_alter ALLOW_CONNECTIONS false",
        )
        .expect("alter database allow_connections");
    engine
        .execute_sql(&session, "ALTER DATABASE adr0014_alter IS_TEMPLATE true")
        .expect("alter database is_template");

    let after = engine
        .cluster_catalog()
        .get_database_by_id(db_id)
        .expect("cluster lookup")
        .expect("database still registered");
    assert_eq!(after.owner, "newowner");
    assert_eq!(after.connection_limit, Some(17));
    assert!(!after.allow_connections);
    assert!(after.is_template);

    engine
        .execute_sql(
            &session,
            "ALTER DATABASE adr0014_alter RENAME TO adr0014_alter_renamed",
        )
        .expect("alter database rename");
    assert!(engine
        .cluster_catalog()
        .get_database_by_name("adr0014_alter")
        .unwrap()
        .is_none());
    let renamed = engine
        .cluster_catalog()
        .get_database_by_name("adr0014_alter_renamed")
        .unwrap()
        .expect("renamed database present");
    assert_eq!(renamed.id, db_id);
}

#[test]
fn alter_database_unsupported_option_fails_explicitly() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE DATABASE adr0014_unsupported_opt OWNER aiondb",
        )
        .expect("create database");

    let err = engine
        .execute_sql(
            &session,
            "ALTER DATABASE adr0014_unsupported_opt SET work_mem = '64MB'",
        )
        .expect_err("unsupported ALTER DATABASE option must fail explicitly");
    assert_eq!(err.sqlstate(), SqlState::FeatureNotSupported);
    assert!(
        err.report().message.contains("ALTER DATABASE"),
        "unexpected message: {}",
        err.report().message
    );
}

#[test]
fn alter_database_rename_requires_physical_schema_and_fails_explicitly_on_drift() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let err = engine
        .execute_sql(&session, "ALTER DATABASE test RENAME TO test_renamed")
        .expect_err("rename should fail explicitly when physical schema is missing");
    assert_eq!(err.sqlstate(), SqlState::InvalidSchemaName);
    assert!(
        err.report()
            .message
            .contains("physical schema \"db_test\" does not exist"),
        "unexpected message: {}",
        err.report().message
    );
}

#[test]
fn reindex_is_rejected_explicitly_instead_of_reporting_success() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(&session, "CREATE TABLE reindex_probe (id INT)")
        .expect("create probe table");

    let table_err = engine
        .execute_sql(&session, "REINDEX TABLE reindex_probe")
        .expect_err("REINDEX TABLE must fail explicitly");
    assert_eq!(table_err.sqlstate(), SqlState::FeatureNotSupported);
    assert!(
        table_err.report().message.contains("REINDEX"),
        "unexpected message: {}",
        table_err.report().message
    );

    let schema_err = engine
        .execute_sql(&session, "REINDEX SCHEMA public")
        .expect_err("REINDEX SCHEMA must fail explicitly");
    assert_eq!(schema_err.sqlstate(), SqlState::FeatureNotSupported);
    assert!(
        schema_err.report().message.contains("REINDEX"),
        "unexpected message: {}",
        schema_err.report().message
    );
}

#[test]
fn update_pg_catalog_pg_class_fails_explicitly_instead_of_reporting_success() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let err = engine
        .execute_sql(
            &session,
            "UPDATE pg_catalog.pg_class SET relname = 'renamed' WHERE relname = 'pg_class'",
        )
        .expect_err("UPDATE on virtual pg_catalog.pg_class must fail explicitly");
    assert_eq!(err.sqlstate(), SqlState::FeatureNotSupported);
    assert!(
        err.report().message.contains("pg_catalog.pg_class"),
        "unexpected message: {}",
        err.report().message
    );
}

#[test]
fn cluster_is_rejected_explicitly_instead_of_reporting_success() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(&session, "CREATE TABLE cluster_probe (id INT)")
        .expect("create probe table");
    engine
        .execute_sql(
            &session,
            "CREATE INDEX cluster_probe_idx ON cluster_probe(id)",
        )
        .expect("create probe index");

    let table_err = engine
        .execute_sql(&session, "CLUSTER cluster_probe")
        .expect_err("CLUSTER table form must fail explicitly");
    assert_eq!(table_err.sqlstate(), SqlState::FeatureNotSupported);
    assert!(
        table_err.report().message.contains("CLUSTER"),
        "unexpected message: {}",
        table_err.report().message
    );

    let using_err = engine
        .execute_sql(&session, "CLUSTER cluster_probe USING cluster_probe_idx")
        .expect_err("CLUSTER USING form must fail explicitly");
    assert_eq!(using_err.sqlstate(), SqlState::FeatureNotSupported);
    assert!(
        using_err.report().message.contains("CLUSTER"),
        "unexpected message: {}",
        using_err.report().message
    );
}

#[test]
fn refresh_materialized_view_reloads_rows_and_with_no_data_clears_population_state() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE TABLE refresh_src (id INT);
             INSERT INTO refresh_src VALUES (1);
             CREATE MATERIALIZED VIEW refresh_probe AS SELECT id FROM refresh_src;",
        )
        .expect("create refresh probe");

    engine
        .execute_sql(&session, "INSERT INTO refresh_src VALUES (2)")
        .expect("insert fresh source row");
    engine
        .execute_sql(&session, "REFRESH MATERIALIZED VIEW refresh_probe")
        .expect("refresh materialized view");

    let rows = query_rows(
        &engine,
        &session,
        "SELECT id FROM refresh_probe ORDER BY id",
    );
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].values[0], Value::Int(1));
    assert_eq!(rows[1].values[0], Value::Int(2));

    engine
        .execute_sql(
            &session,
            "REFRESH MATERIALIZED VIEW refresh_probe WITH NO DATA",
        )
        .expect("refresh materialized view with no data");
    let rows = query_rows(&engine, &session, "SELECT count(*) FROM refresh_probe");
    assert_eq!(rows[0].values[0], Value::BigInt(0));
    let rows = query_rows(
        &engine,
        &session,
        "SELECT relispopulated FROM pg_catalog.pg_class WHERE relname = 'refresh_probe'",
    );
    assert_eq!(rows[0].values[0], Value::Boolean(false));
}

#[test]
fn advisory_lock_supports_session_and_transaction_lock_lifecycle() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let rows = query_rows(
        &engine,
        &session,
        "SELECT pg_advisory_unlock(100), pg_advisory_unlock_shared(200)",
    );
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].values[0], Value::Boolean(false));
    assert_eq!(rows[0].values[1], Value::Boolean(false));

    engine
        .execute_sql(
            &session,
            "SELECT pg_advisory_lock(100), pg_advisory_lock_shared(200), \
                    pg_advisory_xact_lock(300), pg_advisory_xact_lock_shared(400)",
        )
        .expect("acquire advisory locks");

    let rows = query_rows(
        &engine,
        &session,
        "SELECT pg_advisory_unlock(100), pg_advisory_unlock(100), \
                pg_advisory_unlock_shared(200), pg_advisory_unlock_shared(200)",
    );
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].values[0], Value::Boolean(true));
    assert_eq!(rows[0].values[1], Value::Boolean(false));
    assert_eq!(rows[0].values[2], Value::Boolean(true));
    assert_eq!(rows[0].values[3], Value::Boolean(false));

    engine
        .execute_sql(&session, "BEGIN; SELECT pg_advisory_xact_lock(500); COMMIT")
        .expect("xact advisory lock");
    let rows = query_rows(&engine, &session, "SELECT pg_advisory_unlock(500)");
    assert_eq!(rows[0].values[0], Value::Boolean(false));

    engine
        .execute_sql(
            &session,
            "SELECT pg_advisory_lock(600), pg_advisory_lock_shared(700)",
        )
        .expect("acquire session locks before unlock_all");
    engine
        .execute_sql(&session, "SELECT pg_advisory_unlock_all()")
        .expect("unlock all session locks");
    let rows = query_rows(
        &engine,
        &session,
        "SELECT pg_advisory_unlock(600), pg_advisory_unlock_shared(700)",
    );
    assert_eq!(rows[0].values[0], Value::Boolean(false));
    assert_eq!(rows[0].values[1], Value::Boolean(false));
}

#[test]
fn advisory_try_lock_reflects_cross_session_conflicts() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session_a, _) = engine.startup(startup_params()).expect("startup A");
    let (session_b, _) = engine.startup(startup_params()).expect("startup B");

    engine
        .execute_sql(&session_a, "SELECT pg_advisory_lock(77)")
        .expect("session A acquires advisory lock");

    let rows = query_rows(
        &engine,
        &session_b,
        "SELECT pg_try_advisory_lock(77), pg_try_advisory_lock_shared(77)",
    );
    assert_eq!(rows[0].values[0], Value::Boolean(false));
    assert_eq!(rows[0].values[1], Value::Boolean(false));

    engine
        .execute_sql(&session_a, "SELECT pg_advisory_unlock(77)")
        .expect("session A releases advisory lock");
    let rows = query_rows(
        &engine,
        &session_b,
        "SELECT pg_try_advisory_lock(77), pg_try_advisory_lock_shared(77)",
    );
    assert_eq!(rows[0].values[0], Value::Boolean(true));
    assert_eq!(rows[0].values[1], Value::Boolean(true));
}

#[test]
fn update_pg_database_fails_fast_instead_of_succeeding_as_a_noop() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let err = engine
        .execute_sql(
            &session,
            "UPDATE pg_database \
             SET datacl = array_fill(makeaclitem(10, 10, 'USAGE', false), ARRAY[5e5::int]) \
             WHERE datname = 'regression_utf8'",
        )
        .expect_err("UPDATE pg_database should not report a fake success");
    let report = err.report();
    assert_eq!(report.sqlstate, SqlState::FeatureNotSupported);
    assert!(
        report.message.contains("UPDATE pg_catalog.pg_database"),
        "unexpected message: {}",
        report.message
    );
}
