#![allow(clippy::similar_names)]

use super::*;

fn unique_replica_data_dir(name: &str) -> std::path::PathBuf {
    crate::test_support::unique_temp_path("engine-tests-replica-pgstat", name)
}

fn text_col(row: &Row, idx: usize) -> &str {
    match &row.values[idx] {
        Value::Text(value) => value,
        other => panic!("expected text at column {idx}, got {other:?}"),
    }
}

fn int_col(row: &Row, idx: usize) -> i32 {
    match row.values[idx] {
        Value::Int(value) => value,
        ref other => panic!("expected int at column {idx}, got {other:?}"),
    }
}

#[test]
fn supported_pg_session_helpers_return_compat_values() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let rows = query_rows(
        &engine,
        &session,
        "SELECT pg_client_encoding(), version(), current_schema(), current_catalog(), pg_is_in_recovery(), pg_is_wal_replay_paused()",
    );
    assert_eq!(rows.len(), 1);
    assert_eq!(text_col(&rows[0], 0), "UTF8");
    assert_eq!(text_col(&rows[0], 1), "PostgreSQL 16.0 (AionDB)");
    assert_eq!(text_col(&rows[0], 2), "public");
    assert_eq!(text_col(&rows[0], 3), "default");
    assert!(!bool_col(&rows[0], 4));
    assert!(!bool_col(&rows[0], 5));
}

#[test]
fn pg_stat_wal_receiver_returns_replica_progress_row() {
    let data_dir = unique_replica_data_dir("row");
    let mut runtime = aiondb_config::RuntimeConfig::default();
    runtime.storage.data_dir = data_dir.clone();
    runtime.replication.role = aiondb_config::ReplicationRole::Replica;
    runtime.replication.primary_conninfo = Some("host=127.0.0.1 port=5432".to_owned());

    let engine = EngineBuilder::new_with_config(data_dir.clone(), runtime)
        .expect("replica builder")
        .with_authorizer(std::sync::Arc::new(aiondb_security::AllowAllAuthorizer))
        .with_allow_ephemeral_users(true)
        .build()
        .expect("replica engine");
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let manager = engine
        .replication_manager()
        .expect("replication manager should exist");
    manager
        .receive_replication_message(&aiondb_wal::replication::ReplicationMessage::WalData {
            start_lsn: aiondb_wal::Lsn::new(1),
            end_lsn: aiondb_wal::Lsn::new(1),
            data: aiondb_wal::codec::encode_entry(&aiondb_wal::record::WalEntry {
                lsn: aiondb_wal::Lsn::new(1),
                prev_lsn: aiondb_wal::Lsn::ZERO,
                database_id: aiondb_wal::record::WalEntry::LEGACY_DATABASE_ID,
                record: aiondb_wal::WalRecord::Checkpoint {
                    last_committed_lsn: aiondb_wal::Lsn::ZERO,
                },
            })
            .expect("encode wal entry"),
        })
        .expect("receive wal data");
    manager
        .receive_replication_message(&aiondb_wal::replication::ReplicationMessage::Keepalive {
            wal_end: aiondb_wal::Lsn::new(1),
            timestamp_us: 1_700_000_000_000_000,
            reply_requested: false,
        })
        .expect("receive keepalive");
    manager.flush_replica_wal().expect("flush replica wal");
    manager.set_replica_apply_lsn(aiondb_wal::Lsn::new(1));

    let rows = query_rows(&engine, &session, "SELECT * FROM pg_stat_wal_receiver");
    assert_eq!(rows.len(), 1);
    assert_eq!(text_col(&rows[0], 1), "streaming");
    assert_eq!(text_col(&rows[0], 2), "0/00000001");
    assert_eq!(text_col(&rows[0], 4), "0/00000001");
    assert_eq!(text_col(&rows[0], 5), "0/00000001");
    assert_eq!(text_col(&rows[0], 9), "0/00000001");
    assert_eq!(text_col(&rows[0], 12), "127.0.0.1");
    assert_eq!(int_col(&rows[0], 13), 5432);
    assert_eq!(text_col(&rows[0], 14), "host=127.0.0.1 port=5432");
    assert!(matches!(rows[0].values[7], Value::TimestampTz(_)));
    assert!(matches!(rows[0].values[8], Value::TimestampTz(_)));
    assert!(matches!(rows[0].values[10], Value::TimestampTz(_)));

    let _ = std::fs::remove_dir_all(data_dir);
}

#[test]
fn pg_stat_wal_receiver_is_empty_outside_replica_mode() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let rows = query_rows(
        &engine,
        &session,
        "SELECT * FROM pg_catalog.pg_stat_wal_receiver",
    );
    assert!(rows.is_empty());
}

#[test]
fn current_schema_and_current_schemas_follow_effective_search_path() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE SCHEMA analytics;
             SET search_path TO analytics, public",
        )
        .expect("prepare search_path");

    let rows = query_rows(
        &engine,
        &session,
        "SELECT current_schema(), current_catalog(), current_database(), \
                current_schemas(false), current_schemas(true)",
    );
    assert_eq!(rows.len(), 1);
    assert_eq!(text_col(&rows[0], 0), "analytics");
    assert_eq!(text_col(&rows[0], 1), "default");
    assert_eq!(text_col(&rows[0], 2), "default");
    assert_eq!(text_array_col(&rows[0], 3), vec!["analytics", "public"]);
    assert_eq!(
        text_array_col(&rows[0], 4),
        vec!["pg_catalog", "analytics", "public"]
    );
}

#[test]
fn non_default_database_uses_isolated_physical_schema_but_exposes_public_catalog() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (admin, _) = engine.startup(startup_params()).expect("startup admin");

    engine
        .execute_sql(&admin, "CREATE DATABASE shadow_iso")
        .expect("create shadow database");

    let mut shadow_params = startup_params();
    shadow_params.database = "shadow_iso".to_owned();
    let (shadow, _) = engine.startup(shadow_params).expect("startup shadow");

    engine
        .execute_sql(
            &shadow,
            "CREATE TABLE iso_items (id INT PRIMARY KEY); INSERT INTO iso_items VALUES (1)",
        )
        .expect("create isolated table");

    let default_rows = query_rows(
        &engine,
        &admin,
        "SELECT table_schema, table_name \
         FROM information_schema.tables \
         WHERE table_schema = 'public' AND table_name = 'iso_items'",
    );
    assert!(default_rows.is_empty());

    let shadow_rows = query_rows(
        &engine,
        &shadow,
        "SELECT table_schema, table_name \
         FROM information_schema.tables \
         WHERE table_schema = 'public' AND table_name = 'iso_items'",
    );
    assert_eq!(shadow_rows.len(), 1);
    assert_eq!(text_col(&shadow_rows[0], 0), "public");
    assert_eq!(text_col(&shadow_rows[0], 1), "iso_items");

    let schema_rows = query_rows(
        &engine,
        &shadow,
        "SELECT current_schema(), current_schemas(false), current_schemas(true)",
    );
    assert_eq!(schema_rows.len(), 1);
    assert_eq!(text_col(&schema_rows[0], 0), "public");
    assert_eq!(text_array_col(&schema_rows[0], 1), vec!["public"]);
    assert_eq!(
        text_array_col(&schema_rows[0], 2),
        vec!["pg_catalog", "public"]
    );

    let regclass_rows = query_rows(
        &engine,
        &shadow,
        "SELECT (('\"' || table_schema || '\".\"' || table_name || '\"')::regclass)::text \
         FROM information_schema.tables \
         WHERE table_schema = 'public' AND table_name = 'iso_items'",
    );
    assert_eq!(regclass_rows.len(), 1);
    assert!(
        text_col(&regclass_rows[0], 0).ends_with(".iso_items")
            || text_col(&regclass_rows[0], 0) == "iso_items"
    );
    assert!(
        !text_col(&regclass_rows[0], 0).contains("db_"),
        "regclass output should not expose physical schema name"
    );

    let three_part_regclass_rows = query_rows(
        &engine,
        &shadow,
        "SELECT (('\"' || current_database() || '\".\"' || table_schema || '\".\"' || table_name || '\"')::regclass)::text \
         FROM information_schema.tables \
         WHERE table_schema = 'public' AND table_name = 'iso_items'",
    );
    assert_eq!(three_part_regclass_rows.len(), 1);
    assert!(
        text_col(&three_part_regclass_rows[0], 0).ends_with(".iso_items")
            || text_col(&three_part_regclass_rows[0], 0) == "iso_items"
    );

    let data_rows = query_rows(&engine, &shadow, "SELECT id FROM iso_items");
    assert_eq!(data_rows.len(), 1);
    assert_eq!(int_col(&data_rows[0], 0), 1);

    engine
        .execute_sql(&shadow, "INSERT INTO public.iso_items VALUES (2)")
        .expect("insert through visible public alias in non-default database");
    let public_rows = query_rows(
        &engine,
        &shadow,
        "SELECT id FROM public.iso_items ORDER BY id",
    );
    assert_eq!(public_rows.len(), 2);
    assert_eq!(int_col(&public_rows[0], 0), 1);
    assert_eq!(int_col(&public_rows[1], 0), 2);
}

#[test]
fn drop_schema_public_cascade_in_non_default_database_recreates_empty_public_alias() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (admin, _) = engine.startup(startup_params()).expect("startup admin");

    engine
        .execute_sql(&admin, "CREATE DATABASE shadow_reset")
        .expect("create shadow database");

    let mut shadow_params = startup_params();
    shadow_params.database = "shadow_reset".to_owned();
    let (shadow, _) = engine.startup(shadow_params).expect("startup shadow");

    engine
        .execute_sql(
            &shadow,
            "CREATE TABLE alpha (id INT PRIMARY KEY);
             CREATE TABLE beta (id INT PRIMARY KEY)",
        )
        .expect("create shadow tables");

    engine
        .execute_sql(&shadow, "DROP SCHEMA public CASCADE; CREATE SCHEMA public")
        .expect("reset public alias");

    let rows = query_rows(
        &engine,
        &shadow,
        "SELECT table_name
         FROM information_schema.tables
         WHERE table_schema = 'public'
         ORDER BY table_name",
    );
    assert!(
        rows.is_empty(),
        "public should be empty after reset, got {rows:?}"
    );

    engine
        .execute_sql(&shadow, "CREATE TABLE gamma (id INT PRIMARY KEY)")
        .expect("recreate table after public reset");
    let rows = query_rows(
        &engine,
        &shadow,
        "SELECT table_name
         FROM information_schema.tables
         WHERE table_schema = 'public'
         ORDER BY table_name",
    );
    assert_eq!(rows.len(), 1);
    assert_eq!(text_col(&rows[0], 0), "gamma");
}

#[test]
fn plan_cache_key_respects_search_path_for_unqualified_relations() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE SCHEMA analytics; \
             CREATE TABLE public.items_cache_test (id INT); \
             CREATE TABLE analytics.items_cache_test (id INT); \
             INSERT INTO public.items_cache_test VALUES (1); \
             INSERT INTO analytics.items_cache_test VALUES (2)",
        )
        .expect("setup search_path cache test");

    engine
        .execute_sql(&session, "SET search_path TO public")
        .expect("set search_path public");
    let rows = query_rows(&engine, &session, "SELECT id FROM items_cache_test");
    assert_eq!(int_col(&rows[0], 0), 1);

    engine
        .execute_sql(&session, "SET search_path TO analytics")
        .expect("set search_path analytics");
    let rows = query_rows(&engine, &session, "SELECT id FROM items_cache_test");
    assert_eq!(int_col(&rows[0], 0), 2);
}

#[test]
fn set_local_search_path_rebinds_unqualified_relations_within_transaction() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE SCHEMA analytics; \
             CREATE TABLE public.items_local_path_test (id INT); \
             CREATE TABLE analytics.items_local_path_test (id INT); \
             INSERT INTO public.items_local_path_test VALUES (1); \
             INSERT INTO analytics.items_local_path_test VALUES (2); \
             SET search_path TO public",
        )
        .expect("setup local search_path cache test");

    let rows = query_rows(&engine, &session, "SELECT id FROM items_local_path_test");
    assert_eq!(int_col(&rows[0], 0), 1);

    engine.execute_sql(&session, "BEGIN").expect("begin");
    engine
        .execute_sql(&session, "SET LOCAL search_path TO analytics")
        .expect("set local search_path");
    let rows = query_rows(&engine, &session, "SELECT id FROM items_local_path_test");
    assert_eq!(int_col(&rows[0], 0), 2);
    engine.execute_sql(&session, "ROLLBACK").expect("rollback");

    let rows = query_rows(&engine, &session, "SELECT id FROM items_local_path_test");
    assert_eq!(int_col(&rows[0], 0), 1);
}

#[test]
fn select_resolves_unqualified_relation_from_later_search_path_schema() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE SCHEMA analytics; \
             CREATE TABLE analytics.items_search_path_select (id INT); \
             INSERT INTO analytics.items_search_path_select VALUES (7); \
             SET search_path TO public, analytics",
        )
        .expect("setup search_path select relation");

    let rows = query_rows(&engine, &session, "SELECT id FROM items_search_path_select");
    assert_eq!(rows.len(), 1);
    assert_eq!(int_col(&rows[0], 0), 7);
}

#[test]
fn select_resolves_unqualified_view_from_later_search_path_schema() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE SCHEMA analytics; \
             CREATE TABLE analytics.items_search_path_view (id INT); \
             INSERT INTO analytics.items_search_path_view VALUES (11); \
             CREATE VIEW analytics.items_search_path_view_v AS \
                 SELECT id FROM analytics.items_search_path_view; \
             SET search_path TO public, analytics",
        )
        .expect("setup search_path view relation");

    let rows = query_rows(&engine, &session, "SELECT id FROM items_search_path_view_v");
    assert_eq!(rows.len(), 1);
    assert_eq!(int_col(&rows[0], 0), 11);
}

#[test]
fn dml_targets_resolve_unqualified_relation_from_later_search_path_schema() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE SCHEMA analytics; \
             CREATE TABLE analytics.items_search_path_dml (id INT); \
             SET search_path TO public, analytics; \
             INSERT INTO items_search_path_dml VALUES (1); \
             UPDATE items_search_path_dml SET id = 2; \
             DELETE FROM items_search_path_dml WHERE id = 1; \
             INSERT INTO items_search_path_dml VALUES (3)",
        )
        .expect("run search_path dml against later schema relation");

    let rows = query_rows(
        &engine,
        &session,
        "SELECT id FROM analytics.items_search_path_dml ORDER BY id",
    );
    assert_eq!(rows.len(), 2);
    assert_eq!(int_col(&rows[0], 0), 2);
    assert_eq!(int_col(&rows[1], 0), 3);
}

#[test]
fn ddl_existing_object_lookups_resolve_later_search_path_schema() {
    let catalog = Arc::new(aiondb_catalog_store::CatalogStore::default());
    let storage = Arc::new(aiondb_storage_engine::InMemoryStorage::new_without_wal());
    let engine = build_engine_with_store(catalog.clone(), storage);
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE SCHEMA analytics; \
             CREATE TABLE analytics.ddl_path_items (id INT); \
             SET search_path TO public, analytics",
        )
        .expect("setup search_path ddl relation");

    engine
        .execute_sql(
            &session,
            "ALTER TABLE ddl_path_items ADD COLUMN note TEXT; \
             CREATE INDEX ddl_path_items_idx ON ddl_path_items (id); \
             ANALYZE ddl_path_items; \
             VACUUM ddl_path_items; \
             INSERT INTO ddl_path_items (id, note) VALUES (1, 'x'); \
             TRUNCATE ddl_path_items",
        )
        .expect("run ddl/object lookups through later search_path schema");

    let analytics_schema_id = aiondb_catalog::CatalogReader::get_schema(
        &*catalog,
        aiondb_core::TxnId::default(),
        &aiondb_catalog::QualifiedName::unqualified("analytics"),
    )
    .expect("read analytics schema")
    .expect("analytics schema exists")
    .schema_id;
    let table_id = aiondb_catalog::CatalogReader::get_table(
        &*catalog,
        aiondb_core::TxnId::default(),
        &aiondb_catalog::QualifiedName::qualified("analytics", "ddl_path_items"),
    )
    .expect("read ddl_path_items")
    .expect("ddl_path_items exists")
    .table_id;
    let indexes = aiondb_catalog::CatalogReader::list_indexes(
        &*catalog,
        aiondb_core::TxnId::default(),
        table_id,
    )
    .expect("list ddl_path_items indexes");
    assert_eq!(indexes.len(), 1);
    assert_eq!(indexes[0].schema_id, analytics_schema_id);
    assert_eq!(indexes[0].name.name, "ddl_path_items_idx");

    let rows = query_rows(
        &engine,
        &session,
        "SELECT id, note FROM analytics.ddl_path_items",
    );
    assert!(rows.is_empty());

    engine
        .execute_sql(
            &session,
            "DROP INDEX ddl_path_items_idx; DROP TABLE ddl_path_items",
        )
        .expect("drop objects through later search_path schema");

    let err = engine
        .execute_sql(&session, "SELECT * FROM analytics.ddl_path_items")
        .expect_err("table should be dropped");
    assert_eq!(err.report().sqlstate, SqlState::UndefinedTable);
}

#[test]
fn create_table_inherits_and_fk_resolve_later_search_path_schema() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE SCHEMA analytics; \
             CREATE TABLE analytics.parents (parent_note TEXT); \
             CREATE TABLE analytics.referenced (id INT PRIMARY KEY); \
             INSERT INTO analytics.referenced VALUES (10); \
             SET search_path TO public, analytics; \
             CREATE TABLE children ( \
                 id INT PRIMARY KEY, \
                 ref_id INT REFERENCES referenced(id) \
             ) INHERITS (parents); \
             INSERT INTO children (id, ref_id, parent_note) VALUES (1, 10, 'ok')",
        )
        .expect("create inherited/fk table via later search_path schema");

    let rows = query_rows(
        &engine,
        &session,
        "SELECT id, ref_id, parent_note FROM public.children",
    );
    assert_eq!(rows.len(), 1);
    assert_eq!(int_col(&rows[0], 0), 1);
    assert_eq!(int_col(&rows[0], 1), 10);
    assert_eq!(text_col(&rows[0], 2), "ok");
}

#[test]
fn drop_sequence_resolves_later_search_path_schema() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE SCHEMA analytics; \
             CREATE SEQUENCE analytics.seq_path_test; \
             SET search_path TO public, analytics; \
             DROP SEQUENCE seq_path_test",
        )
        .expect("drop sequence through later search_path schema");

    let err = engine
        .execute_sql(&session, "SELECT nextval('analytics.seq_path_test')")
        .expect_err("sequence should be dropped");
    assert_eq!(err.report().sqlstate, SqlState::UndefinedObject);
}

#[test]
fn drop_if_exists_guards_resolve_later_search_path_schema_objects() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE SCHEMA analytics; \
             CREATE TABLE analytics.guard_items (id INT); \
             CREATE VIEW analytics.guard_view AS SELECT 1 AS x; \
             CREATE SEQUENCE analytics.guard_seq; \
             SET search_path TO public, analytics; \
             CREATE INDEX guard_items_idx ON guard_items (id); \
             DROP VIEW IF EXISTS guard_view; \
             DROP INDEX IF EXISTS guard_items_idx; \
             DROP TABLE IF EXISTS guard_items; \
             DROP SEQUENCE IF EXISTS guard_seq",
        )
        .expect("drop guarded objects through later search_path schema");

    assert_eq!(
        engine
            .execute_sql(&session, "SELECT * FROM analytics.guard_view")
            .expect_err("view should be dropped")
            .report()
            .sqlstate,
        SqlState::UndefinedTable
    );
    assert_eq!(
        engine
            .execute_sql(&session, "SELECT * FROM analytics.guard_items")
            .expect_err("table should be dropped")
            .report()
            .sqlstate,
        SqlState::UndefinedTable
    );
    assert_eq!(
        engine
            .execute_sql(&session, "SELECT nextval('analytics.guard_seq')")
            .expect_err("sequence should be dropped")
            .report()
            .sqlstate,
        SqlState::UndefinedObject
    );
}

#[test]
fn create_index_if_not_exists_resolves_unqualified_index_in_later_search_path_schema() {
    let catalog = Arc::new(aiondb_catalog_store::CatalogStore::default());
    let storage = Arc::new(aiondb_storage_engine::InMemoryStorage::new_without_wal());
    let engine = build_engine_with_store(catalog.clone(), storage);
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE SCHEMA analytics; \
             CREATE TABLE analytics.guard_index_items (id INT); \
             SET search_path TO public, analytics; \
             CREATE INDEX guard_index_items_idx ON guard_index_items (id); \
             CREATE INDEX IF NOT EXISTS guard_index_items_idx ON guard_index_items (id)",
        )
        .expect("create guarded index through later search_path schema");

    let table_id = aiondb_catalog::CatalogReader::get_table(
        &*catalog,
        aiondb_core::TxnId::default(),
        &aiondb_catalog::QualifiedName::qualified("analytics", "guard_index_items"),
    )
    .expect("read guard_index_items")
    .expect("guard_index_items exists")
    .table_id;
    let indexes = aiondb_catalog::CatalogReader::list_indexes(
        &*catalog,
        aiondb_core::TxnId::default(),
        table_id,
    )
    .expect("list guard_index_items indexes");
    assert_eq!(indexes.len(), 1);
    assert_eq!(indexes[0].name.name, "guard_index_items_idx");
}

#[test]
fn supported_reg_lookup_helpers_return_compat_values() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let rows = query_rows(
        &engine,
        &session,
        "SELECT to_regclass('pg_class'), to_regtype('int4'), to_regnamespace('information_schema'), to_regtype('does_not_exist')",
    );
    assert_eq!(rows.len(), 1);
    assert_eq!(text_col(&rows[0], 0), "pg_catalog.pg_class");
    assert_eq!(text_col(&rows[0], 1), "integer");
    assert_eq!(text_col(&rows[0], 2), "information_schema");
    assert!(matches!(rows[0].values[3], Value::Null));
}

#[test]
fn regclass_cast_and_relation_size_resolve_user_indexes_in_pg_class_queries() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(&session, "SET statement_timeout = 300000")
        .expect("raise statement timeout for relation size compat setup");

    engine
        .execute_sql(
            &session,
            "SET min_parallel_index_scan_size TO '128kB';
             CREATE TABLE parallel_vacuum_table (a INT, b INT);
             INSERT INTO parallel_vacuum_table
             SELECT g, g % 10
             FROM generate_series(1, 10000) AS g;
             CREATE INDEX regular_sized_index ON parallel_vacuum_table (a);
             CREATE INDEX typically_sized_index ON parallel_vacuum_table (a);
             CREATE INDEX vacuum_in_leader_small_index ON parallel_vacuum_table (b);",
        )
        .expect("create table and indexes for relation size compat checks");

    let regclass_out = query_rows(
        &engine,
        &session,
        "SELECT 'regular_sized_index'::regclass::text",
    );
    assert_eq!(regclass_out.len(), 1);
    assert_eq!(text_col(&regclass_out[0], 0), "regular_sized_index");

    let exists_rows = query_rows(
        &engine,
        &session,
        "SELECT EXISTS (
             SELECT 1
             FROM pg_class
             WHERE oid = 'vacuum_in_leader_small_index'::regclass
               AND pg_relation_size(oid) >= 0
         )",
    );
    assert_eq!(exists_rows.len(), 1);
    assert!(bool_col(&exists_rows[0], 0));

    let count_rows = query_rows(
        &engine,
        &session,
        "SELECT count(*)
         FROM pg_class
         WHERE oid IN ('regular_sized_index'::regclass, 'typically_sized_index'::regclass)
           AND pg_relation_size(oid) >= 0",
    );
    assert_eq!(count_rows.len(), 1);
    assert_eq!(bigint_col(&count_rows[0], 0), 2);
}

#[test]
fn pg_catalog_numeric_literals_preserve_numeric_type_and_equality() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(&session, "CREATE TABLE numeric_literal_pg (id INT)")
        .expect("create table");

    let results = engine
        .execute_sql(
            &session,
            "SELECT 1.5 AS n \
             FROM pg_catalog.pg_class \
             WHERE 1.0 = 1.00 AND relname = 'numeric_literal_pg'",
        )
        .expect("numeric literal projection should succeed");

    let (columns, rows) = match &results[0] {
        StatementResult::Query { columns, rows } => (columns, rows),
        other => panic!("expected Query, got {other:?}"),
    };
    assert_eq!(columns.len(), 1);
    assert_eq!(columns[0].data_type, DataType::Numeric);
    assert_eq!(rows.len(), 1);
    assert!(matches!(
        &rows[0].values[0],
        Value::Numeric(value) if value.to_string() == "1.5"
    ));
}

#[test]
fn pg_get_serial_sequence_resolves_owned_sequence_defaults() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE TABLE things (id INT GENERATED BY DEFAULT AS IDENTITY, name TEXT)",
        )
        .expect("create table");

    let rows = query_rows(
        &engine,
        &session,
        "SELECT pg_get_serial_sequence('things', 'id')",
    );
    assert_eq!(rows.len(), 1);
    assert_eq!(text_col(&rows[0], 0), "public.things_id_seq");
}

#[test]
fn pg_get_serial_sequence_respects_search_path_for_unqualified_table_names() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE SCHEMA analytics;
             SET search_path TO analytics, public;
             CREATE SEQUENCE thing_ids;
             CREATE TABLE things (
               id BIGINT NOT NULL DEFAULT nextval('thing_ids')
             );",
        )
        .expect("create schema-local sequence-backed table");

    let rows = query_rows(
        &engine,
        &session,
        "SELECT pg_get_serial_sequence('things', 'id')",
    );
    assert_eq!(rows.len(), 1);
    assert_eq!(text_col(&rows[0], 0), "analytics.thing_ids");
}

#[test]
fn sequence_session_helpers_follow_postgres_semantics() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(&session, "CREATE SEQUENCE seq_semantics")
        .expect("create sequence");

    let currval_err = engine
        .execute_sql(&session, "SELECT currval('seq_semantics')")
        .expect_err("currval should require prior session state");
    assert_eq!(
        currval_err.sqlstate(),
        SqlState::ObjectNotInPrerequisiteState
    );
    assert_eq!(
        currval_err.report().message,
        "currval of sequence \"seq_semantics\" is not yet defined in this session"
    );

    let lastval_err = engine
        .execute_sql(&session, "SELECT lastval()")
        .expect_err("lastval should require prior session state");
    assert_eq!(
        lastval_err.sqlstate(),
        SqlState::ObjectNotInPrerequisiteState
    );
    assert_eq!(
        lastval_err.report().message,
        "lastval is not yet defined in this session"
    );

    let nextval = query_rows(&engine, &session, "SELECT nextval('seq_semantics')");
    assert_eq!(nextval.len(), 1);
    assert_eq!(bigint_col(&nextval[0], 0), 1);

    let after_nextval = query_rows(
        &engine,
        &session,
        "SELECT currval('seq_semantics'), lastval()",
    );
    assert_eq!(after_nextval.len(), 1);
    assert_eq!(bigint_col(&after_nextval[0], 0), 1);
    assert_eq!(bigint_col(&after_nextval[0], 1), 1);

    let setval_called = query_rows(&engine, &session, "SELECT setval('seq_semantics', 20)");
    assert_eq!(setval_called.len(), 1);
    assert_eq!(bigint_col(&setval_called[0], 0), 20);

    let after_setval_called = query_rows(
        &engine,
        &session,
        "SELECT currval('seq_semantics'), lastval()",
    );
    assert_eq!(after_setval_called.len(), 1);
    assert_eq!(bigint_col(&after_setval_called[0], 0), 20);
    assert_eq!(bigint_col(&after_setval_called[0], 1), 1);

    let setval_not_called = query_rows(
        &engine,
        &session,
        "SELECT setval('seq_semantics', 30, false)",
    );
    assert_eq!(setval_not_called.len(), 1);
    assert_eq!(bigint_col(&setval_not_called[0], 0), 30);

    let after_setval_not_called = query_rows(
        &engine,
        &session,
        "SELECT currval('seq_semantics'), lastval()",
    );
    assert_eq!(after_setval_not_called.len(), 1);
    assert_eq!(bigint_col(&after_setval_not_called[0], 0), 20);
    assert_eq!(bigint_col(&after_setval_not_called[0], 1), 1);

    let next_after_setval_false = query_rows(&engine, &session, "SELECT nextval('seq_semantics')");
    assert_eq!(next_after_setval_false.len(), 1);
    assert_eq!(bigint_col(&next_after_setval_false[0], 0), 30);

    let after_second_nextval = query_rows(
        &engine,
        &session,
        "SELECT currval('seq_semantics'), lastval()",
    );
    assert_eq!(after_second_nextval.len(), 1);
    assert_eq!(bigint_col(&after_second_nextval[0], 0), 30);
    assert_eq!(bigint_col(&after_second_nextval[0], 1), 30);
}

#[test]
fn sequence_session_helpers_are_isolated_and_discard_resets_state() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session_a, _) = engine.startup(startup_params()).expect("startup session a");
    let (session_b, _) = engine.startup(startup_params()).expect("startup session b");

    engine
        .execute_sql(&session_a, "CREATE SEQUENCE shared_seq")
        .expect("create sequence");
    let rows = query_rows(&engine, &session_a, "SELECT nextval('shared_seq')");
    assert_eq!(rows.len(), 1);
    assert_eq!(bigint_col(&rows[0], 0), 1);

    let session_b_err = engine
        .execute_sql(&session_b, "SELECT currval('shared_seq')")
        .expect_err("currval should be session-local");
    assert_eq!(
        session_b_err.sqlstate(),
        SqlState::ObjectNotInPrerequisiteState
    );

    engine
        .execute_sql(&session_a, "DISCARD SEQUENCES")
        .expect("discard sequences should execute");

    let session_a_err = engine
        .execute_sql(&session_a, "SELECT lastval()")
        .expect_err("discard should clear lastval state");
    assert_eq!(
        session_a_err.sqlstate(),
        SqlState::ObjectNotInPrerequisiteState
    );
}

#[test]
fn discard_all_fails_fast_instead_of_succeeding_as_a_stub() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let error = engine
        .execute_sql(&session, "DISCARD ALL")
        .expect_err("DISCARD ALL should fail instead of claiming success");
    assert_eq!(error.sqlstate(), SqlState::FeatureNotSupported);
    assert!(error.report().message.contains("DISCARD"));
}

#[test]
fn discard_temp_fails_fast_instead_of_succeeding_as_a_stub() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let error = engine
        .execute_sql(&session, "DISCARD TEMP")
        .expect_err("DISCARD TEMP should fail instead of claiming success");
    assert_eq!(error.sqlstate(), SqlState::FeatureNotSupported);
    assert!(error.report().message.contains("DISCARD TEMP"));
}

#[test]
fn comment_on_updates_session_comment_registry() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(&session, "CREATE TABLE t (id INT)")
        .expect("create table");

    engine
        .execute_sql(&session, "COMMENT ON TABLE t IS 'hello'")
        .expect("set comment");
    engine
        .with_session(&session, |record| {
            assert!(record
                .comments
                .iter()
                .any(|((object_type, _), comment)| object_type == "TABLE" && comment == "hello"));
            Ok(())
        })
        .expect("inspect session comment state");

    engine
        .execute_sql(&session, "COMMENT ON TABLE t IS NULL")
        .expect("clear comment");
    engine
        .with_session(&session, |record| {
            assert!(!record
                .comments
                .iter()
                .any(|((object_type, _), _)| object_type == "TABLE"));
            Ok(())
        })
        .expect("inspect comment cleared");
}

#[test]
fn comment_on_missing_table_fails_explicitly() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let err = engine
        .execute_sql(&session, "COMMENT ON TABLE missing_tbl_for_comment IS 'x'")
        .expect_err("comment on missing table must fail explicitly");
    assert_eq!(err.sqlstate(), SqlState::UndefinedTable);
    assert!(
        err.report()
            .message
            .contains("relation \"missing_tbl_for_comment\" does not exist"),
        "unexpected message: {}",
        err.report().message
    );
}

#[test]
fn obj_description_returns_table_comment_via_regclass_oid() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(&session, "CREATE TABLE t_obj_desc (id INT)")
        .expect("create table");
    engine
        .execute_sql(
            &session,
            "COMMENT ON TABLE t_obj_desc IS 'hello obj description'",
        )
        .expect("set table comment");

    let rows = query_rows(
        &engine,
        &session,
        "SELECT obj_description('t_obj_desc'::regclass, 'pg_class')",
    );
    assert_eq!(rows.len(), 1);
    assert_eq!(text_col(&rows[0], 0), "hello obj description");
}

#[test]
fn col_description_returns_column_comment_via_regclass_oid_and_attnum() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(&session, "CREATE TABLE t_col_desc (id INT)")
        .expect("create table");
    engine
        .execute_sql(
            &session,
            "COMMENT ON COLUMN t_col_desc.id IS 'hello column description'",
        )
        .expect("set column comment");

    let rows = query_rows(
        &engine,
        &session,
        "SELECT col_description('t_col_desc'::regclass, 1)",
    );
    assert_eq!(rows.len(), 1);
    assert_eq!(text_col(&rows[0], 0), "hello column description");
}

#[test]
fn schema_qualified_comment_on_table_and_column_resolve_relation_names() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE SCHEMA app; CREATE TABLE app.t_qualified_comment (id INT)",
        )
        .expect("create schema-qualified table");
    engine
        .execute_sql(
            &session,
            "COMMENT ON TABLE app.t_qualified_comment IS 'qualified table comment'",
        )
        .expect("set qualified table comment");
    engine
        .execute_sql(
            &session,
            "COMMENT ON COLUMN app.t_qualified_comment.id IS 'qualified column comment'",
        )
        .expect("set qualified column comment");

    let rows = query_rows(
        &engine,
        &session,
        "SELECT \
            obj_description('app.t_qualified_comment'::regclass, 'pg_class'), \
            col_description('app.t_qualified_comment'::regclass, 1)",
    );
    assert_eq!(rows.len(), 1);
    assert_eq!(text_col(&rows[0], 0), "qualified table comment");
    assert_eq!(text_col(&rows[0], 1), "qualified column comment");
}

#[test]
fn comments_are_visible_across_sessions_in_same_engine() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(&session, "CREATE TABLE t_comment_shared (id INT)")
        .expect("create table");
    engine
        .execute_sql(
            &session,
            "COMMENT ON TABLE t_comment_shared IS 'shared table comment'",
        )
        .expect("set table comment");
    engine
        .execute_sql(
            &session,
            "COMMENT ON COLUMN t_comment_shared.id IS 'shared column comment'",
        )
        .expect("set column comment");

    let (second_session, _) = engine.startup(startup_params()).expect("second startup");
    let rows = query_rows(
        &engine,
        &second_session,
        "SELECT \
            obj_description('t_comment_shared'::regclass, 'pg_class'), \
            col_description('t_comment_shared'::regclass, 1)",
    );
    assert_eq!(rows.len(), 1);
    assert_eq!(text_col(&rows[0], 0), "shared table comment");
    assert_eq!(text_col(&rows[0], 1), "shared column comment");
}

#[test]
fn comments_are_hydrated_from_catalog_in_fresh_engine() {
    let catalog = Arc::new(aiondb_catalog_store::CatalogStore::default());
    let storage = Arc::new(aiondb_storage_engine::InMemoryStorage::new_without_wal());

    {
        let engine = build_engine_with_store(catalog.clone(), storage.clone());
        let (session, _) = engine.startup(startup_params()).expect("startup");

        engine
            .execute_sql(&session, "CREATE TABLE t_comment_catalog (id INT)")
            .expect("create table");
        engine
            .execute_sql(
                &session,
                "COMMENT ON TABLE t_comment_catalog IS 'catalog table comment'",
            )
            .expect("set table comment");
        engine
            .execute_sql(
                &session,
                "COMMENT ON COLUMN t_comment_catalog.id IS 'catalog column comment'",
            )
            .expect("set column comment");
    }

    let restarted = build_engine_with_store(catalog, storage);
    let (session, _) = restarted
        .startup(startup_params())
        .expect("restart startup");
    let rows = query_rows(
        &restarted,
        &session,
        "SELECT \
            obj_description('t_comment_catalog'::regclass, 'pg_class'), \
            col_description('t_comment_catalog'::regclass, 1)",
    );
    assert_eq!(rows.len(), 1);
    assert_eq!(text_col(&rows[0], 0), "catalog table comment");
    assert_eq!(text_col(&rows[0], 1), "catalog column comment");
}

#[test]
fn comment_on_constraint_missing_table_fails_explicitly() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let err = engine
        .execute_sql(
            &session,
            "COMMENT ON CONSTRAINT missing_constraint ON missing_constraint_table IS 'x'",
        )
        .expect_err("comment on constraint for missing table must fail explicitly");
    assert_eq!(err.sqlstate(), SqlState::UndefinedTable);
    assert!(
        err.report()
            .message
            .contains("relation \"missing_constraint_table\" does not exist"),
        "unexpected message: {}",
        err.report().message
    );
}

#[test]
fn comment_on_missing_role_fails_explicitly() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let err = engine
        .execute_sql(&session, "COMMENT ON ROLE missing_role_for_comment IS 'x'")
        .expect_err("comment on missing role must fail explicitly");
    assert_eq!(err.sqlstate(), SqlState::UndefinedObject);
    assert!(
        err.report()
            .message
            .contains("role \"missing_role_for_comment\" does not exist"),
        "unexpected message: {}",
        err.report().message
    );
}

#[test]
fn comment_on_missing_database_fails_explicitly() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let err = engine
        .execute_sql(
            &session,
            "COMMENT ON DATABASE missing_db_for_comment IS 'x'",
        )
        .expect_err("comment on missing database must fail explicitly");
    assert_eq!(err.sqlstate(), SqlState::InvalidCatalogName);
    assert!(
        err.report()
            .message
            .contains("database \"missing_db_for_comment\" does not exist"),
        "unexpected message: {}",
        err.report().message
    );
}

#[test]
fn comment_on_missing_schema_fails_explicitly() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let err = engine
        .execute_sql(
            &session,
            "COMMENT ON SCHEMA missing_schema_for_comment IS 'x'",
        )
        .expect_err("comment on missing schema must fail explicitly");
    assert_eq!(err.sqlstate(), SqlState::InvalidSchemaName);
    assert!(
        err.report()
            .message
            .contains("schema \"missing_schema_for_comment\" does not exist"),
        "unexpected message: {}",
        err.report().message
    );
}

#[test]
fn comment_on_unsupported_object_type_fails_explicitly() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let err = engine
        .execute_sql(&session, "COMMENT ON TYPE unsupported_comment_type IS 'x'")
        .expect_err("unsupported COMMENT ON target must fail explicitly");
    assert_eq!(err.sqlstate(), SqlState::FeatureNotSupported);
    assert!(
        err.report().message.contains("COMMENT ON TYPE"),
        "unexpected message: {}",
        err.report().message
    );
}

#[test]
fn comment_on_missing_index_fails_explicitly() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let err = engine
        .execute_sql(&session, "COMMENT ON INDEX idx_missing IS 'x'")
        .expect_err("comment on missing index must fail explicitly");
    assert_eq!(err.sqlstate(), SqlState::UndefinedObject);
    assert!(
        err.report()
            .message
            .contains("index \"idx_missing\" does not exist"),
        "unexpected message: {}",
        err.report().message
    );
}

#[test]
fn comment_on_existing_index_succeeds() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(&session, "CREATE TABLE idx_comment_t (id INT)")
        .expect("create table");
    engine
        .execute_sql(
            &session,
            "CREATE INDEX idx_comment_t_id_idx ON idx_comment_t(id)",
        )
        .expect("create index");

    engine
        .execute_sql(&session, "COMMENT ON INDEX idx_comment_t_id_idx IS 'hello'")
        .expect("comment on existing index");
}

#[test]
fn comment_on_column_for_compat_foreign_table_fails_explicitly() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .with_session_mut(&session, |record| {
            record.compat_misc_objects.insert(
                ("CREATE FOREIGN TABLE".to_owned(), "ft_compat".to_owned()),
                "ft_compat".to_owned(),
            );
            Ok(())
        })
        .expect("seed compat foreign table marker");

    let err = engine
        .execute_sql(&session, "COMMENT ON COLUMN ft_compat.c1 IS 'x'")
        .expect_err("compat foreign-table column comment must fail explicitly");
    assert_eq!(err.sqlstate(), SqlState::FeatureNotSupported);
    assert!(
        err.report().message.contains("COMMENT ON COLUMN"),
        "unexpected message: {}",
        err.report().message
    );
}

#[test]
fn security_label_updates_session_label_registry() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(&session, "CREATE TABLE t (id INT)")
        .expect("create table");

    engine
        .execute_sql(
            &session,
            "SECURITY LABEL FOR selinux ON TABLE t IS 'system_u:object_r:sepgsql_table_t:s0'",
        )
        .expect("set security label");
    engine
        .with_session(&session, |record| {
            assert_eq!(
                record
                    .security_labels
                    .get(&("TABLE".to_owned(), "t".to_owned()))
                    .cloned(),
                Some((
                    Some("selinux".to_owned()),
                    "system_u:object_r:sepgsql_table_t:s0".to_owned(),
                ))
            );
            Ok(())
        })
        .expect("inspect session security label state");

    engine
        .execute_sql(&session, "SECURITY LABEL ON TABLE t IS NULL")
        .expect("clear security label");
    engine
        .with_session(&session, |record| {
            assert!(!record
                .security_labels
                .contains_key(&("TABLE".to_owned(), "t".to_owned())));
            Ok(())
        })
        .expect("inspect security label cleared");
}

#[test]
fn security_label_on_missing_table_fails_explicitly() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let err = engine
        .execute_sql(
            &session,
            "SECURITY LABEL ON TABLE missing_security_label_tbl IS 'x'",
        )
        .expect_err("security label on missing table must fail explicitly");
    assert_eq!(err.sqlstate(), SqlState::UndefinedTable);
    assert!(
        err.report()
            .message
            .contains("relation \"missing_security_label_tbl\" does not exist"),
        "unexpected message: {}",
        err.report().message
    );
}

#[test]
fn security_label_on_missing_index_fails_explicitly() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let err = engine
        .execute_sql(
            &session,
            "SECURITY LABEL ON INDEX missing_security_label_idx IS 'x'",
        )
        .expect_err("security label on missing index must fail explicitly");
    assert_eq!(err.sqlstate(), SqlState::UndefinedObject);
    assert!(
        err.report()
            .message
            .contains("index \"missing_security_label_idx\" does not exist"),
        "unexpected message: {}",
        err.report().message
    );
}

#[test]
fn security_label_on_missing_role_fails_explicitly() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let err = engine
        .execute_sql(
            &session,
            "SECURITY LABEL ON ROLE missing_role_for_label IS 'x'",
        )
        .expect_err("security label on missing role must fail explicitly");
    assert_eq!(err.sqlstate(), SqlState::UndefinedObject);
    assert!(
        err.report()
            .message
            .contains("role \"missing_role_for_label\" does not exist"),
        "unexpected message: {}",
        err.report().message
    );
}

#[test]
fn security_label_on_missing_database_fails_explicitly() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let err = engine
        .execute_sql(
            &session,
            "SECURITY LABEL ON DATABASE missing_db_for_label IS 'x'",
        )
        .expect_err("security label on missing database must fail explicitly");
    assert_eq!(err.sqlstate(), SqlState::InvalidCatalogName);
    assert!(
        err.report()
            .message
            .contains("database \"missing_db_for_label\" does not exist"),
        "unexpected message: {}",
        err.report().message
    );
}

#[test]
fn security_label_on_unsupported_object_type_fails_explicitly() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let err = engine
        .execute_sql(
            &session,
            "SECURITY LABEL ON TYPE unsupported_security_label_type IS 'x'",
        )
        .expect_err("unsupported SECURITY LABEL target must fail explicitly");
    assert_eq!(err.sqlstate(), SqlState::FeatureNotSupported);
    assert!(
        err.report().message.contains("SECURITY LABEL ON TYPE"),
        "unexpected message: {}",
        err.report().message
    );
}

#[test]
fn pg_read_file_helpers_return_compat_values() {
    let data_dir = unique_temp_dir("pg-read-file-helpers");
    fs::create_dir_all(&data_dir).expect("create data dir");

    let mut runtime = RuntimeConfig::default();
    runtime.storage.backend = StorageBackend::InMemory;
    runtime.storage.data_dir = data_dir.clone();
    let engine = EngineBuilder::for_testing()
        .with_runtime_config(runtime)
        .build()
        .unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");
    let admin_session = bootstrap_admin_session(&engine, &session);
    ensure_alice_role(&engine, &admin_session);
    engine
        .execute_sql(
            &admin_session,
            "GRANT EXECUTE ON FUNCTION pg_read_file(text) TO alice;
             GRANT EXECUTE ON FUNCTION pg_read_file(text, bool) TO alice;
             GRANT EXECUTE ON FUNCTION pg_read_file(text, int, int) TO alice;
             GRANT EXECUTE ON FUNCTION pg_read_binary_file(text, int, int) TO alice;",
        )
        .expect("grant file helper execute");

    let rows = query_rows(
        &engine,
        &session,
        "SELECT \
            length(pg_read_file('postmaster.pid')) > 20, \
            length(pg_read_file('postmaster.pid', 1, 20)), \
            pg_read_file('does not exist', true) IS NULL, \
            octet_length(pg_read_binary_file('postmaster.pid', 1, 20))",
    );
    assert_eq!(rows.len(), 1);
    assert!(bool_col(&rows[0], 0));
    assert_eq!(int_col(&rows[0], 1), 20);
    assert!(bool_col(&rows[0], 2));
    assert_eq!(int_col(&rows[0], 3), 20);

    let _ = fs::remove_dir_all(&data_dir);
}

#[test]
fn pg_read_file_helpers_report_postgres_like_errors() {
    let data_dir = unique_temp_dir("pg-read-file-errors");
    fs::create_dir_all(&data_dir).expect("create data dir");

    let mut runtime = RuntimeConfig::default();
    runtime.storage.backend = StorageBackend::InMemory;
    runtime.storage.data_dir = data_dir.clone();
    let engine = EngineBuilder::for_testing()
        .with_runtime_config(runtime)
        .build()
        .unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");
    let admin_session = bootstrap_admin_session(&engine, &session);
    ensure_alice_role(&engine, &admin_session);
    engine
        .execute_sql(
            &admin_session,
            "GRANT EXECUTE ON FUNCTION pg_read_file(text) TO alice;
             GRANT EXECUTE ON FUNCTION pg_read_binary_file(text, int, int, bool) TO alice;",
        )
        .expect("grant file helper execute");

    let missing = engine
        .execute_sql(&session, "SELECT pg_read_file('does not exist')")
        .expect_err("missing file should fail");
    assert_eq!(
        missing.report().message,
        "could not open file \"does not exist\" for reading: No such file or directory"
    );

    let invalid = engine
        .execute_sql(
            &session,
            "SELECT pg_read_binary_file('does not exist', 0, -1, true)",
        )
        .expect_err("negative length should fail");
    assert_eq!(
        invalid.report().message,
        "requested length cannot be negative"
    );

    let _ = fs::remove_dir_all(&data_dir);
}

#[test]
fn pg_read_file_requires_execute_privilege() {
    let data_dir = unique_temp_dir("pg-read-file-acl");
    fs::create_dir_all(&data_dir).expect("create data dir");

    let mut runtime = RuntimeConfig::default();
    runtime.storage.backend = StorageBackend::InMemory;
    runtime.storage.data_dir = data_dir.clone();
    let engine = EngineBuilder::for_testing()
        .with_runtime_config(runtime)
        .build()
        .unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");
    let admin_session = bootstrap_admin_session(&engine, &session);

    engine
        .execute_sql(&admin_session, "CREATE ROLE regress_file_reader LOGIN")
        .expect("create role");

    let mut reader_params = startup_params();
    reader_params.credential = Credential::Anonymous {
        user: "regress_file_reader".to_owned(),
    };
    let (reader_session, _) = engine.startup(reader_params).expect("reader startup");

    let denied = engine
        .execute_sql(&reader_session, "SELECT pg_read_file('postmaster.pid')")
        .expect_err("pg_read_file should require EXECUTE");
    assert_eq!(denied.sqlstate(), SqlState::InsufficientPrivilege);

    engine
        .execute_sql(
            &admin_session,
            "GRANT EXECUTE ON FUNCTION pg_read_file(text) TO regress_file_reader",
        )
        .expect("grant execute");

    let rows = query_rows(
        &engine,
        &reader_session,
        "SELECT length(pg_read_file('postmaster.pid')) > 20",
    );
    assert_eq!(rows.len(), 1);
    assert!(bool_col(&rows[0], 0));

    let _ = fs::remove_dir_all(&data_dir);
}

#[test]
fn pg_tablespace_databases_matches_synthetic_catalog() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let rows = query_rows(
        &engine,
        &session,
        "SELECT pg_tablespace_databases(1663), pg_tablespace_databases(1664)",
    );
    assert_eq!(rows.len(), 1);
    assert_eq!(int_col(&rows[0], 0), compat_database_oid("default"));
    assert!(matches!(rows[0].values[1], Value::Null));
}

#[test]
fn pg_database_owner_joins_to_visible_role() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let databases = query_rows(&engine, &session, "SELECT datname, datdba FROM pg_database");
    let roles = query_rows(&engine, &session, "SELECT oid, rolname FROM pg_roles");

    let database = databases
        .iter()
        .find(|row| text_col(row, 0) == "default")
        .expect("default database should be visible");
    let owner_oid = int_col(database, 1);
    let owner_name = roles
        .iter()
        .find(|row| int_col(row, 0) == owner_oid)
        .map(|row| text_col(row, 1).to_owned())
        .expect("database owner should be exposed in pg_roles");
    assert_eq!(owner_name, "alice");
}
