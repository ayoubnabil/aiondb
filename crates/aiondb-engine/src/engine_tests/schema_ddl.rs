use std::sync::Arc;

use aiondb_catalog::QualifiedName;
use aiondb_core::{SqlState, TxnId};
use aiondb_security::AllowAllAuthorizer;

use super::*;

fn durable_data_dir(name: &str) -> std::path::PathBuf {
    let dir = crate::test_support::unique_temp_path("engine-tests-schema-ddl", name);
    let _ = std::fs::remove_dir_all(&dir);
    dir
}

#[test]
fn prepared_select_star_description_refreshes_after_table_ddl() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE TABLE trigtest (a integer, b integer, c integer);
             INSERT INTO trigtest VALUES (1, 2, 3)",
        )
        .expect("create test table");

    let initial = engine
        .prepare(
            &session,
            "select_trigtest".to_owned(),
            "SELECT * FROM trigtest".to_owned(),
        )
        .expect("prepare select star");
    assert_eq!(column_names(&initial.result_columns), ["a", "b", "c"]);

    engine
        .execute_sql(
            &session,
            "ALTER TABLE trigtest ADD COLUMN d integer DEFAULT 42 NOT NULL",
        )
        .expect("add column");
    let after_add = engine
        .describe_statement(&session, "select_trigtest")
        .expect("describe after add");
    assert_eq!(
        column_names(&after_add.result_columns),
        ["a", "b", "c", "d"]
    );

    engine
        .execute_sql(&session, "ALTER TABLE trigtest DROP COLUMN b")
        .expect("drop column");
    let after_drop = engine
        .describe_statement(&session, "select_trigtest")
        .expect("describe after drop");
    assert_eq!(column_names(&after_drop.result_columns), ["a", "c", "d"]);
}

#[test]
fn drop_column_rewrites_existing_rows_and_preserves_surviving_indexes() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE TABLE ddl_rewrite_probe (id INT PRIMARY KEY, doomed TEXT, slug TEXT UNIQUE);
             INSERT INTO ddl_rewrite_probe VALUES
                (1, 'legacy', 'alpha'),
                (2, 'legacy', 'beta')",
        )
        .expect("create indexed probe");
    engine
        .execute_sql(&session, "ALTER TABLE ddl_rewrite_probe DROP COLUMN doomed")
        .expect("drop indexed-adjacent column");

    let results = engine
        .execute_sql(
            &session,
            "SELECT id, slug FROM ddl_rewrite_probe ORDER BY id",
        )
        .expect("select rewritten rows");
    let [StatementResult::Query { rows, .. }] = results.as_slice() else {
        panic!("expected one query result, got {results:?}");
    };
    assert_eq!(
        rows.as_slice(),
        [
            aiondb_core::Row {
                values: vec![
                    aiondb_core::Value::Int(1),
                    aiondb_core::Value::Text("alpha".to_owned()),
                ],
            },
            aiondb_core::Row {
                values: vec![
                    aiondb_core::Value::Int(2),
                    aiondb_core::Value::Text("beta".to_owned()),
                ],
            },
        ]
    );

    let duplicate = engine
        .execute_sql(
            &session,
            "INSERT INTO ddl_rewrite_probe (id, slug) VALUES (3, 'alpha')",
        )
        .expect_err("unique index should survive column rewrite");
    assert_eq!(duplicate.sqlstate(), SqlState::UniqueViolation);
}

#[test]
fn drop_column_with_transactional_pattern_indexes_commits_on_durable_storage() {
    let data_dir = durable_data_dir("drop-column-pattern-indexes");
    std::fs::create_dir_all(&data_dir).expect("create data dir");
    let engine = EngineBuilder::new_durable(data_dir.clone())
        .expect("durable builder")
        .with_authorizer(Arc::new(AllowAllAuthorizer))
        .with_allow_ephemeral_users(true)
        .build()
        .expect("durable engine");
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE TABLE ddl_drop_idx_probe (
                id INT PRIMARY KEY,
                slug VARCHAR(80) UNIQUE,
                headline VARCHAR(120),
                category VARCHAR(40)
             );
             CREATE INDEX ddl_drop_idx_probe_category_idx
                ON ddl_drop_idx_probe (category);
             CREATE INDEX ddl_drop_idx_probe_category_like_idx
                ON ddl_drop_idx_probe (category varchar_pattern_ops);
             INSERT INTO ddl_drop_idx_probe VALUES
                (1, 'alpha', 'One', 'general'),
                (2, 'beta', 'Two', 'general')",
        )
        .expect("seed durable table");

    engine
        .execute_sql(
            &session,
            "BEGIN;
             ALTER TABLE ddl_drop_idx_probe DROP COLUMN category;
             COMMIT",
        )
        .expect("drop column should commit cleanly");

    let results = engine
        .execute_sql(
            &session,
            "SELECT id, slug, headline FROM ddl_drop_idx_probe ORDER BY id",
        )
        .expect("select after drop column");
    let [StatementResult::Query { rows, .. }] = results.as_slice() else {
        panic!("expected query result, got {results:?}");
    };
    assert_eq!(
        rows.len(),
        2,
        "rows should survive transactional drop column"
    );

    let restarted = EngineBuilder::new_durable(data_dir.clone())
        .expect("reopen durable builder")
        .with_authorizer(Arc::new(AllowAllAuthorizer))
        .with_allow_ephemeral_users(true)
        .build()
        .expect("reopen durable engine");
    let (restart_session, _) = restarted
        .startup(startup_params())
        .expect("restart startup");
    let results = restarted
        .execute_sql(
            &restart_session,
            "SELECT id, slug, headline FROM ddl_drop_idx_probe ORDER BY id",
        )
        .expect("select after restart");
    let [StatementResult::Query { rows, .. }] = results.as_slice() else {
        panic!("expected query result after restart, got {results:?}");
    };
    assert_eq!(rows.len(), 2, "rows should survive restart");

    let _ = std::fs::remove_dir_all(&data_dir);
}

#[test]
fn transactional_drop_and_readd_same_column_name_rewrites_rows_against_current_descriptor() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE TABLE ddl_drop_add_same_name (
                id SERIAL PRIMARY KEY,
                email VARCHAR(190) UNIQUE NOT NULL,
                name VARCHAR(120) NOT NULL
             );
             INSERT INTO ddl_drop_add_same_name (email, name)
             VALUES ('alice@example.com', 'Alice')",
        )
        .expect("seed table");

    engine
        .execute_sql(
            &session,
            "BEGIN;
             ALTER TABLE ddl_drop_add_same_name DROP COLUMN name;
             ALTER TABLE ddl_drop_add_same_name ADD COLUMN name VARCHAR(140) NOT NULL;
             ROLLBACK",
        )
        .expect("drop+add same-name column should not trip row-width validation");
}

#[test]
fn transactional_multiple_alter_steps_rewrite_pending_rows_before_next_alter() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE TABLE ddl_multi_alter_probe (
                id SERIAL PRIMARY KEY,
                email VARCHAR(190) UNIQUE NOT NULL,
                name VARCHAR(120) NOT NULL
             );
             INSERT INTO ddl_multi_alter_probe (email, name)
             VALUES ('alice@example.com', 'Alice')",
        )
        .expect("seed table");

    engine
        .execute_sql(
            &session,
            "BEGIN;
             ALTER TABLE ddl_multi_alter_probe ADD COLUMN summary VARCHAR(60);
             ALTER TABLE ddl_multi_alter_probe DROP COLUMN name;
             ALTER TABLE ddl_multi_alter_probe ADD COLUMN name VARCHAR(140) NOT NULL;
             ROLLBACK",
        )
        .expect("multi-step alter should rewrite pending rows before next alter");
}

fn column_names(columns: &[ResultColumn]) -> Vec<&str> {
    columns.iter().map(|column| column.name.as_str()).collect()
}

#[test]
fn drop_schema_cascade_removes_schema_objects() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE SCHEMA analytics;
             CREATE TABLE analytics.events (id INT PRIMARY KEY);
             CREATE SEQUENCE analytics.seq;
             CREATE VIEW analytics.event_ids AS SELECT id FROM analytics.events;
             DROP SCHEMA analytics CASCADE",
        )
        .expect("drop schema cascade");

    assert!(engine
        .catalog_reader
        .get_schema(TxnId::default(), &QualifiedName::unqualified("analytics"))
        .unwrap()
        .is_none());
    assert!(engine
        .catalog_reader
        .get_table(
            TxnId::default(),
            &QualifiedName::qualified("analytics", "events"),
        )
        .unwrap()
        .is_none());
    assert!(engine
        .catalog_reader
        .get_sequence(
            TxnId::default(),
            &QualifiedName::qualified("analytics", "seq"),
        )
        .unwrap()
        .is_none());
    assert!(engine
        .catalog_reader
        .get_view(
            TxnId::default(),
            &QualifiedName::qualified("analytics", "event_ids"),
        )
        .unwrap()
        .is_none());
}

#[test]
fn drop_schema_without_cascade_rejects_nonempty_schema() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE SCHEMA analytics; CREATE TABLE analytics.events (id INT PRIMARY KEY)",
        )
        .expect("create schema contents");

    let error = engine
        .execute_sql(&session, "DROP SCHEMA analytics")
        .expect_err("non-empty schema should require cascade");
    assert_eq!(error.sqlstate(), SqlState::DependentObjectsStillExist);
    assert!(engine
        .catalog_reader
        .get_schema(TxnId::default(), &QualifiedName::unqualified("analytics"))
        .unwrap()
        .is_some());
}

#[test]
fn create_schema_authorization_variants_are_accepted() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE SCHEMA AUTHORIZATION app_owner; CREATE SCHEMA reporting AUTHORIZATION admin",
        )
        .expect("create schema authorization variants");

    assert!(engine
        .catalog_reader
        .get_schema(TxnId::default(), &QualifiedName::unqualified("app_owner"))
        .unwrap()
        .is_some());
    assert!(engine
        .catalog_reader
        .get_schema(TxnId::default(), &QualifiedName::unqualified("reporting"))
        .unwrap()
        .is_some());
}

#[test]
fn create_schema_public_reports_duplicate_schema_sqlstate() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let error = engine
        .execute_sql(&session, r#"CREATE SCHEMA "public""#)
        .expect_err("public schema should already exist");

    assert_eq!(error.sqlstate(), SqlState::DuplicateSchema);
    assert_eq!(error.sqlstate().code(), "42P06");
}

#[test]
fn create_schema_inline_body_executes_in_created_schema() {
    // Creating a role activates the role-management gate, so the inline
    // CREATE SCHEMA AUTHORIZATION must run from a session whose identity
    // maps to a superuser role. Re-authenticate as the role being used so
    // the strict `role_system_active` check accepts the schema creation.
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE ROLE regress_create_schema_role SUPERUSER LOGIN",
        )
        .expect("create role");

    let mut admin_params = startup_params();
    admin_params.credential = Credential::Anonymous {
        user: "regress_create_schema_role".to_owned(),
    };
    let (admin_session, _) = engine.startup(admin_params).expect("admin startup");

    engine
        .execute_sql(
            &admin_session,
            "CREATE SCHEMA AUTHORIZATION regress_create_schema_role
               CREATE TABLE regress_create_schema_role.tab (id int)",
        )
        .expect("create schema with inline body");

    assert!(engine
        .catalog_reader
        .get_table(
            TxnId::default(),
            &QualifiedName::qualified("regress_create_schema_role", "tab"),
        )
        .unwrap()
        .is_some());
}

#[test]
fn create_schema_inline_body_supports_multiple_statements() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE SCHEMA analytics \
               CREATE TABLE events (id int) \
               CREATE VIEW event_ids AS SELECT id FROM events",
        )
        .expect("create schema with multiple inline statements");

    assert!(engine
        .catalog_reader
        .get_table(
            TxnId::default(),
            &QualifiedName::qualified("analytics", "events"),
        )
        .unwrap()
        .is_some());
    assert!(engine
        .catalog_reader
        .get_view(
            TxnId::default(),
            &QualifiedName::qualified("analytics", "event_ids"),
        )
        .unwrap()
        .is_some());
}

#[test]
fn create_schema_inline_body_rejects_mismatched_schema_names() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let error = engine
        .execute_sql(
            &session,
            "CREATE SCHEMA AUTHORIZATION regress_create_schema_role
               CREATE TABLE schema_not_existing.tab (id int)",
        )
        .expect_err("schema-qualified body should reject mismatched schema");

    assert_eq!(error.sqlstate(), SqlState::InvalidSchemaName);
    assert!(
        error
            .report()
            .message
            .contains("CREATE specifies a schema (schema_not_existing) different from the one being created (regress_create_schema_role)"),
        "unexpected error: {}",
        error.report().message
    );
    assert!(engine
        .catalog_reader
        .get_schema(
            TxnId::default(),
            &QualifiedName::unqualified("regress_create_schema_role"),
        )
        .unwrap()
        .is_none());
}

#[test]
fn create_schema_authorization_current_role_uses_effective_role_name() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");
    let current_role = "alice";

    let error = engine
        .execute_sql(
            &session,
            "CREATE SCHEMA AUTHORIZATION CURRENT_ROLE
               CREATE TABLE schema_not_existing.tab (id int)",
        )
        .expect_err("schema-qualified body should reject mismatched schema");

    assert_eq!(error.sqlstate(), SqlState::InvalidSchemaName);
    assert!(
        error.report().message.contains(
            "CREATE specifies a schema (schema_not_existing) different from the one being created (alice)"
        ),
        "unexpected error: {}",
        error.report().message
    );

    engine
        .execute_sql(
            &session,
            "CREATE SCHEMA AUTHORIZATION CURRENT_ROLE
               CREATE TABLE alice.tab (id int)",
        )
        .expect("create schema with current role name");

    assert!(engine
        .catalog_reader
        .get_table(
            TxnId::default(),
            &QualifiedName::qualified(current_role, "tab"),
        )
        .unwrap()
        .is_some());
}

#[test]
fn create_typed_table_tracks_composite_type_metadata() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE TYPE pair_type AS (id INT, name TEXT); \
             CREATE TABLE typed_items OF pair_type",
        )
        .expect("create typed table");

    let table = engine
        .catalog_reader
        .get_table(TxnId::default(), &QualifiedName::unqualified("typed_items"))
        .unwrap()
        .expect("typed table should exist");
    assert_eq!(table.columns.len(), 2);
    assert_eq!(table.columns[0].name, "id");
    assert_eq!(table.columns[1].name, "name");
    assert_eq!(
        engine
            .catalog_reader
            .get_table_type_name(TxnId::default(), table.table_id)
            .unwrap(),
        Some("pair_type".to_owned())
    );
}

#[test]
fn alter_typed_table_is_rejected() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE TYPE pair_type AS (id INT, name TEXT); \
             CREATE TABLE typed_items OF pair_type",
        )
        .expect("create typed table");

    let error = engine
        .execute_sql(&session, "ALTER TABLE typed_items ADD COLUMN extra INT")
        .expect_err("typed table alter should fail");
    assert_eq!(error.sqlstate(), SqlState::FeatureNotSupported);
    assert!(format!("{error}").contains("typed tables"));
}
