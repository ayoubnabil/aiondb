use std::fs;
use std::sync::Arc;

use aiondb_config::{RuntimeConfig, StorageBackend};
use aiondb_core::{
    compat_database_oid, compat_function_oid, compat_role_oid, DataType, Row, SqlState, Value,
};

use super::*;

#[path = "pg_catalog_relations.rs"]
mod relations;
#[path = "pg_catalog_roles_and_privileges.rs"]
mod roles_and_privileges;
#[path = "pg_catalog_search_path_and_settings.rs"]
mod search_path_and_settings;

// ---------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------

fn text_col(row: &Row, idx: usize) -> &str {
    match &row.values[idx] {
        Value::Text(s) => s.as_str(),
        other => panic!("expected Text, got {other:?}"),
    }
}

fn int_col(row: &Row, idx: usize) -> i32 {
    match &row.values[idx] {
        Value::Int(n) => *n,
        other => panic!("expected Int, got {other:?}"),
    }
}

fn bigint_col(row: &Row, idx: usize) -> i64 {
    match &row.values[idx] {
        Value::BigInt(n) => *n,
        other => panic!("expected BigInt, got {other:?}"),
    }
}

fn bool_col(row: &Row, idx: usize) -> bool {
    match &row.values[idx] {
        Value::Boolean(b) => *b,
        other => panic!("expected Boolean, got {other:?}"),
    }
}

fn text_array_col(row: &Row, idx: usize) -> Vec<&str> {
    match &row.values[idx] {
        Value::Array(values) => values
            .iter()
            .map(|value| match value {
                Value::Text(text) => text.as_str(),
                other => panic!("expected Array<Text>, got element {other:?}"),
            })
            .collect(),
        other => panic!("expected Array, got {other:?}"),
    }
}

fn ensure_alice_role(engine: &Engine, session: &SessionHandle) {
    let _ = engine.execute_sql(session, "CREATE ROLE alice LOGIN");
}

fn bootstrap_admin_session(engine: &Engine, session: &SessionHandle) -> SessionHandle {
    engine
        .execute_sql(session, "CREATE ROLE bootstrap_admin SUPERUSER LOGIN")
        .expect("create bootstrap admin role");

    let mut admin_params = startup_params();
    admin_params.credential = Credential::Anonymous {
        user: "bootstrap_admin".to_owned(),
    };
    let (admin_session, _) = engine
        .startup(admin_params)
        .expect("bootstrap admin startup");
    admin_session
}

#[test]
fn pg_catalog_helper_functions_return_compat_values() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let results = engine
        .execute_sql(
            &session,
            "SELECT \
                current_setting('search_path'), \
                pg_backend_pid(), \
                pg_get_userbyid(10), \
                pg_get_userbyid(1), \
                set_config('search_path', 'public', false), \
                pg_relation_size('pg_class'), \
                pg_column_size('hello')",
        )
        .expect("session helpers should succeed");
    let StatementResult::Query { rows, .. } = &results[0] else {
        panic!("expected Query result");
    };
    assert_eq!(rows.len(), 1);
    assert_eq!(text_col(&rows[0], 0), "\"$user\", public");
    assert!(int_col(&rows[0], 1) > 0);
    assert_eq!(text_col(&rows[0], 2), "aiondb");
    assert_eq!(text_col(&rows[0], 3), "unknown (OID=1)");
    assert_eq!(text_col(&rows[0], 4), "public");
    assert_eq!(bigint_col(&rows[0], 5), 0);
    assert_eq!(int_col(&rows[0], 6), 5);
}

#[test]
fn pg_database_size_returns_compat_zero() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let results = engine
        .execute_sql(&session, "SELECT pg_database_size('default')")
        .expect("pg_database_size should return a compatibility value");
    let StatementResult::Query { rows, .. } = &results[0] else {
        panic!("expected Query result");
    };
    assert_eq!(rows.len(), 1);
    assert_eq!(bigint_col(&rows[0], 0), 0);
}

#[test]
fn pg_get_userbyid_uses_catalog_role_oids() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(&session, "CREATE ROLE alice SUPERUSER LOGIN")
        .expect("create alice role");
    engine
        .execute_sql(&session, "CREATE ROLE report_reader LOGIN")
        .expect("create role");

    let rows = query_rows(
        &engine,
        &session,
        &format!(
            "SELECT pg_get_userbyid({})",
            compat_role_oid("report_reader")
        ),
    );
    assert_eq!(rows.len(), 1);
    assert_eq!(text_col(&rows[0], 0), "report_reader");
}

#[test]
fn regrole_cast_resolves_role_name_and_oid() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");
    let admin_session = bootstrap_admin_session(&engine, &session);

    engine
        .execute_sql(&admin_session, "CREATE ROLE regress_group LOGIN")
        .expect("create regress_group role");

    let rows = query_rows(
        &engine,
        &admin_session,
        "SELECT 'regress_group'::regrole, 'regress_group'::regrole::text",
    );
    assert_eq!(rows.len(), 1);
    assert_eq!(int_col(&rows[0], 0), compat_role_oid("regress_group"));
    assert_eq!(text_col(&rows[0], 1), "regress_group");

    let roundtrip = query_rows(
        &engine,
        &admin_session,
        &format!("SELECT {}::regrole::text", compat_role_oid("regress_group")),
    );
    assert_eq!(roundtrip.len(), 1);
    assert_eq!(text_col(&roundtrip[0], 0), "regress_group");
}

#[test]
fn regprocedure_cast_resolves_user_function_oid_and_roundtrips_text() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE FUNCTION stats_test_func1() RETURNS VOID LANGUAGE plpgsql AS $$BEGIN END;$$",
        )
        .expect("create test function");

    let rows = query_rows(
        &engine,
        &session,
        "SELECT 'stats_test_func1()'::regprocedure::oid, 'stats_test_func1()'::regprocedure::text",
    );
    assert_eq!(rows.len(), 1);
    assert_eq!(
        int_col(&rows[0], 0),
        compat_function_oid("stats_test_func1()")
    );
    assert_eq!(text_col(&rows[0], 1), "stats_test_func1()");

    let roundtrip = query_rows(
        &engine,
        &session,
        &format!(
            "SELECT {}::regprocedure::text",
            compat_function_oid("stats_test_func1()")
        ),
    );
    assert_eq!(roundtrip.len(), 1);
    assert_eq!(text_col(&roundtrip[0], 0), "stats_test_func1()");
}

#[test]
fn builtin_visibility_helpers_roundtrip_via_regproc_and_regprocedure() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let rows = query_rows(
        &engine,
        &session,
        "SELECT \
            'pg_catalog.pg_type_is_visible'::regproc::oid, \
            'pg_catalog.pg_type_is_visible'::regproc::text, \
            'pg_catalog.pg_proc_is_visible(oid)'::regprocedure::oid, \
            'pg_catalog.pg_proc_is_visible(oid)'::regprocedure::text",
    );
    assert_eq!(rows.len(), 1);
    assert_eq!(int_col(&rows[0], 0), 2078);
    assert_eq!(text_col(&rows[0], 1), "pg_type_is_visible");
    assert_eq!(int_col(&rows[0], 2), 2092);
    assert_eq!(text_col(&rows[0], 3), "pg_proc_is_visible(oid)");
}

#[test]
fn text_search_dictionary_create_populates_pg_ts_catalogs() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE TEXT SEARCH DICTIONARY ts_dict_probe (TEMPLATE = simple, STOPWORDS = english)",
        )
        .expect("create text search dictionary");

    let dict_rows = query_rows(
        &engine,
        &session,
        "SELECT dictname, dictinitoption IS NOT NULL FROM pg_catalog.pg_ts_dict WHERE dictname = 'ts_dict_probe'",
    );
    assert_eq!(dict_rows.len(), 1);
    assert_eq!(text_col(&dict_rows[0], 0), "ts_dict_probe");
    assert!(bool_col(&dict_rows[0], 1));

    let template_rows = query_rows(
        &engine,
        &session,
        "SELECT tmplname FROM pg_catalog.pg_ts_template WHERE tmplname = 'simple'",
    );
    assert_eq!(template_rows.len(), 1);
    assert_eq!(text_col(&template_rows[0], 0), "simple");
}

#[test]
fn text_search_dictionary_is_visible_across_sessions_in_same_engine() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE TEXT SEARCH DICTIONARY ts_dict_shared (TEMPLATE = simple)",
        )
        .expect("create text search dictionary");

    let (second_session, _) = engine.startup(startup_params()).expect("second startup");
    let rows = query_rows(
        &engine,
        &second_session,
        "SELECT dictname FROM pg_catalog.pg_ts_dict WHERE dictname = 'ts_dict_shared'",
    );
    assert_eq!(rows.len(), 1);
    assert_eq!(text_col(&rows[0], 0), "ts_dict_shared");
}

#[test]
fn materialized_view_populates_pg_class_and_pg_matviews() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE MATERIALIZED VIEW mv_probe AS SELECT 1 AS id, 'alice' AS name",
        )
        .expect("create materialized view");

    let class_rows = query_rows(
        &engine,
        &session,
        "SELECT relkind::text, relispopulated FROM pg_catalog.pg_class WHERE relname = 'mv_probe'",
    );
    assert_eq!(class_rows.len(), 1);
    assert_eq!(text_col(&class_rows[0], 0), "m");
    assert!(bool_col(&class_rows[0], 1));

    let matview_rows = query_rows(
        &engine,
        &session,
        "SELECT schemaname, matviewname, ispopulated FROM pg_catalog.pg_matviews WHERE matviewname = 'mv_probe'",
    );
    assert_eq!(matview_rows.len(), 1);
    assert_eq!(text_col(&matview_rows[0], 0), "public");
    assert_eq!(text_col(&matview_rows[0], 1), "mv_probe");
    assert!(bool_col(&matview_rows[0], 2));
}

#[test]
fn dropping_materialized_view_table_cleans_up_sidecar_catalog_rows() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE MATERIALIZED VIEW mv_drop_probe AS SELECT 1 AS id",
        )
        .expect("create materialized view");
    engine
        .execute_sql(&session, "DROP TABLE mv_drop_probe")
        .expect("drop materialized view backing table");

    let class_rows = query_rows(
        &engine,
        &session,
        "SELECT count(*) FROM pg_catalog.pg_class WHERE relname = 'mv_drop_probe'",
    );
    assert_eq!(bigint_col(&class_rows[0], 0), 0);

    let matview_rows = query_rows(
        &engine,
        &session,
        "SELECT count(*) FROM pg_catalog.pg_matviews WHERE matviewname = 'mv_drop_probe'",
    );
    assert_eq!(bigint_col(&matview_rows[0], 0), 0);
}

#[test]
fn materialized_view_create_and_drop_use_materialized_command_tags() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let results = engine
        .execute_sql(
            &session,
            "CREATE MATERIALIZED VIEW mv_tag_probe AS SELECT 1 AS id;
             DROP MATERIALIZED VIEW mv_tag_probe;",
        )
        .expect("materialized view lifecycle should succeed");

    assert!(matches!(
        &results[0],
        StatementResult::Command { tag, .. } if tag == "CREATE MATERIALIZED VIEW"
    ));
    assert!(matches!(
        &results[1],
        StatementResult::Command { tag, .. } if tag == "DROP MATERIALIZED VIEW"
    ));
}

#[test]
fn builtin_visibility_helpers_resolve_via_regproc_and_regprocedure() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    for function_name in [
        "pg_catalog.pg_type_is_visible",
        "pg_catalog.pg_operator_is_visible",
        "pg_catalog.pg_ts_parser_is_visible",
        "pg_catalog.pg_collation_is_visible",
        "pg_catalog.pg_statistics_obj_is_visible",
    ] {
        let rows = query_rows(
            &engine,
            &session,
            &format!("SELECT '{function_name}'::regproc::text"),
        );
        assert_eq!(rows.len(), 1);
        assert_eq!(
            text_col(&rows[0], 0),
            function_name.trim_start_matches("pg_catalog.")
        );
    }

    for function_signature in [
        "pg_catalog.pg_type_is_visible(oid)",
        "pg_catalog.pg_operator_is_visible(oid)",
        "pg_catalog.pg_ts_parser_is_visible(oid)",
        "pg_catalog.pg_collation_is_visible(oid)",
        "pg_catalog.pg_statistics_obj_is_visible(oid)",
    ] {
        let rows = query_rows(
            &engine,
            &session,
            &format!("SELECT '{function_signature}'::regprocedure::text"),
        );
        assert_eq!(rows.len(), 1);
        assert_eq!(
            text_col(&rows[0], 0),
            function_signature.trim_start_matches("pg_catalog.")
        );
    }
}

#[test]
fn pg_proc_contains_visibility_helpers_needed_by_psql() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let rows = query_rows(
        &engine,
        &session,
        "SELECT proname, proargtypes::text
         FROM pg_catalog.pg_proc
         WHERE proname IN (
           'pg_type_is_visible',
           'pg_operator_is_visible',
           'pg_ts_parser_is_visible',
           'pg_collation_is_visible',
           'pg_statistics_obj_is_visible'
         )
         ORDER BY proname",
    );
    assert_eq!(rows.len(), 5);
    for row in &rows {
        assert_eq!(text_col(row, 1), "26");
    }
}

#[test]
fn pg_proc_contains_statistics_definition_helpers_needed_by_psql() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let rows = query_rows(
        &engine,
        &session,
        "SELECT proname, proargtypes::text
         FROM pg_catalog.pg_proc
         WHERE proname IN (
           'pg_get_statisticsobjdef',
           'pg_get_statisticsobjdef_columns'
         )
         ORDER BY proname",
    );
    assert_eq!(rows.len(), 2);
    for row in &rows {
        assert_eq!(text_col(row, 1), "26");
    }
}

#[test]
fn pg_proc_contains_function_definition_helpers_needed_by_psql() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let rows = query_rows(
        &engine,
        &session,
        "SELECT proname, proargtypes::text
         FROM pg_catalog.pg_proc
         WHERE proname IN (
           'pg_get_functiondef',
           'pg_get_function_arguments',
           'pg_get_function_result',
           'pg_get_function_identity_arguments'
         )
         ORDER BY proname",
    );
    assert_eq!(rows.len(), 4);
    for row in &rows {
        assert_eq!(text_col(row, 1), "26");
    }
}

#[test]
fn pg_catalog_definition_helpers_return_compat_values() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(&session, "CREATE TABLE items (id INT, name TEXT)")
        .expect("create table");
    engine
        .execute_sql(&session, "CREATE INDEX idx_items_name ON items (name)")
        .expect("create index");

    let index_rows = query_rows(
        &engine,
        &session,
        "SELECT pg_get_indexdef('idx_items_name')",
    );
    assert_eq!(index_rows.len(), 1);
    assert!(
        text_col(&index_rows[0], 0)
            .contains("CREATE INDEX idx_items_name ON public.items USING btree"),
        "expected CREATE INDEX definition, got {}",
        text_col(&index_rows[0], 0)
    );

    let input_rows = query_rows(&engine, &session, "SELECT pg_input_error_info('x', 'int4')");
    assert_eq!(input_rows.len(), 1);
    assert_eq!(
        text_col(&input_rows[0], 0),
        "invalid input syntax for type integer: \"x\""
    );
}

#[test]
fn pg_input_error_info_interval_reports_invalid_datetime_sqlstate() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let rows = query_rows(
        &engine,
        &session,
        "SELECT * FROM pg_input_error_info('garbage', 'interval')",
    );
    assert_eq!(rows.len(), 1);
    assert_eq!(
        text_col(&rows[0], 0),
        "invalid input syntax for type interval: \"garbage\""
    );
    assert!(matches!(rows[0].values[1], Value::Null));
    assert!(matches!(rows[0].values[2], Value::Null));
    assert_eq!(text_col(&rows[0], 3), "22007");
}

#[test]
fn interval_hash_matches_for_comparison_equal_intervals() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let rows = query_rows(
        &engine,
        &session,
        "SELECT interval_hash('30 days'::interval) = interval_hash('1 month'::interval)",
    );
    assert_eq!(rows.len(), 1);
    assert!(bool_col(&rows[0], 0));
}

#[test]
fn superuser_role_session_reports_is_superuser() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(&session, "CREATE ROLE app_admin SUPERUSER LOGIN")
        .expect("create superuser role");

    let mut admin_params = startup_params();
    admin_params.credential = Credential::Anonymous {
        user: "app_admin".to_owned(),
    };
    let (admin_session, _) = engine.startup(admin_params).expect("superuser startup");

    let current_setting_rows = query_rows(
        &engine,
        &admin_session,
        "SELECT current_setting('is_superuser')",
    );
    assert_eq!(text_col(&current_setting_rows[0], 0), "on");

    let show_rows = query_rows(&engine, &admin_session, "SHOW is_superuser");
    assert_eq!(text_col(&show_rows[0], 0), "on");

    let settings_rows = query_rows(
        &engine,
        &admin_session,
        "SELECT setting FROM pg_settings WHERE name = 'is_superuser'",
    );
    assert_eq!(text_col(&settings_rows[0], 0), "on");
}

#[test]
fn pg_get_indexdef_respects_search_path_and_explicit_schema() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE SCHEMA analytics;
             CREATE TABLE public.items_public (id INT, name TEXT);
             CREATE TABLE analytics.items_analytics (id INT, name TEXT);
             CREATE INDEX shared_idx ON public.items_public (name);
             CREATE INDEX shared_idx ON analytics.items_analytics (name)",
        )
        .expect("create indexed tables");

    let default_rows = query_rows(&engine, &session, "SELECT pg_get_indexdef('shared_idx')");
    assert!(
        text_col(&default_rows[0], 0).contains("ON public.items_public USING btree"),
        "expected public index definition, got {}",
        text_col(&default_rows[0], 0)
    );

    engine
        .execute_sql(&session, "SET search_path TO analytics, public")
        .expect("set search_path");

    let search_path_rows = query_rows(&engine, &session, "SELECT pg_get_indexdef('shared_idx')");
    assert!(
        text_col(&search_path_rows[0], 0).contains("ON analytics.items_analytics USING btree"),
        "expected analytics index definition, got {}",
        text_col(&search_path_rows[0], 0)
    );

    let qualified_rows = query_rows(
        &engine,
        &session,
        "SELECT pg_get_indexdef('analytics.shared_idx')",
    );
    assert!(
        text_col(&qualified_rows[0], 0).contains("ON analytics.items_analytics USING btree"),
        "expected explicitly qualified analytics index definition, got {}",
        text_col(&qualified_rows[0], 0)
    );
}

#[test]
fn current_setting_and_pg_settings_follow_session_state() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let rows = query_rows(&engine, &session, "SELECT current_setting('is_superuser')");
    assert_eq!(rows.len(), 1);
    assert_eq!(text_col(&rows[0], 0), "off");
    let settings_rows = query_rows(
        &engine,
        &session,
        "SELECT setting FROM pg_settings WHERE name = 'is_superuser'",
    );
    assert_eq!(settings_rows.len(), 1);
    assert_eq!(text_col(&settings_rows[0], 0), "off");

    engine
        .execute_sql(&session, "SET search_path TO customschema")
        .expect("set search_path");
    let rows = query_rows(&engine, &session, "SELECT current_setting('search_path')");
    assert_eq!(rows.len(), 1);
    assert_eq!(text_col(&rows[0], 0), "customschema");
    let settings_rows = query_rows(
        &engine,
        &session,
        "SELECT setting FROM pg_settings WHERE name = 'search_path'",
    );
    assert_eq!(settings_rows.len(), 1);
    assert_eq!(text_col(&settings_rows[0], 0), "customschema");
}

#[test]
fn startup_params_seed_session_variables() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let mut params = startup_params();
    params.application_name = Some("pgwire-client".to_owned());
    params.options.insert(
        "options".to_owned(),
        "-c search_path=customschema".to_owned(),
    );

    let (session, _) = engine.startup(params).expect("startup");

    let rows = query_rows(
        &engine,
        &session,
        "SELECT current_setting('application_name'), current_setting('search_path')",
    );
    assert_eq!(rows.len(), 1);
    assert_eq!(text_col(&rows[0], 0), "pgwire-client");
    assert_eq!(text_col(&rows[0], 1), "customschema");
    let settings_rows = query_rows(
        &engine,
        &session,
        "SELECT setting FROM pg_settings WHERE name = 'search_path'",
    );
    assert_eq!(settings_rows.len(), 1);
    assert_eq!(text_col(&settings_rows[0], 0), "customschema");

    let show_application_name = query_rows(&engine, &session, "SHOW application_name");
    assert_eq!(text_col(&show_application_name[0], 0), "pgwire-client");
    let show_search_path = query_rows(&engine, &session, "SHOW search_path");
    assert_eq!(text_col(&show_search_path[0], 0), "customschema");
}

#[test]
fn startup_params_cannot_override_effective_role() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let mut params = startup_params();
    params
        .options
        .insert("role".to_owned(), "postgres".to_owned());
    params.options.insert(
        "options".to_owned(),
        "-c role=postgres -c search_path=public".to_owned(),
    );

    let (session, info) = engine.startup(params).expect("startup");
    assert_eq!(info.identity.user, "alice");

    let rows = query_rows(&engine, &session, "SELECT current_user, session_user");
    assert_eq!(rows.len(), 1);
    assert_eq!(text_col(&rows[0], 0), "alice");
    assert_eq!(text_col(&rows[0], 1), "alice");
}

// ---------------------------------------------------------------
// pg_catalog.pg_namespace
// ---------------------------------------------------------------

#[test]
fn pg_namespace_returns_builtin_schemas() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let results = engine
        .execute_sql(&session, "SELECT * FROM pg_catalog.pg_namespace")
        .expect("query");

    let (columns, rows) = match &results[0] {
        StatementResult::Query { columns, rows } => (columns, rows),
        other => panic!("expected Query, got {other:?}"),
    };

    assert!(columns.len() >= 3);
    assert_eq!(columns[0].name, "oid");
    assert_eq!(columns[0].data_type, DataType::Int);
    assert_eq!(columns[1].name, "nspname");
    assert_eq!(columns[2].name, "nspowner");

    let names: Vec<&str> = rows.iter().map(|r| text_col(r, 1)).collect();
    assert!(names.contains(&"public"), "expected 'public' in {names:?}");
    assert!(
        names.contains(&"pg_catalog"),
        "expected 'pg_catalog' in {names:?}"
    );
    assert!(
        names.contains(&"information_schema"),
        "expected 'information_schema' in {names:?}"
    );
}

fn unique_temp_dir(name: &str) -> std::path::PathBuf {
    crate::test_support::unique_temp_path("engine-tests-pg-catalog", name)
}

#[test]
fn pg_namespace_unqualified_access() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    // PostgreSQL search path: pg_catalog tables can be accessed without schema.
    let rows = query_rows(&engine, &session, "SELECT * FROM pg_namespace");
    assert!(rows.len() >= 3);
}

// ---------------------------------------------------------------
// pg_catalog.pg_class
// ---------------------------------------------------------------

#[test]
fn pg_class_returns_created_tables() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(&session, "CREATE TABLE users (id INT NOT NULL, name TEXT)")
        .expect("create");

    let rows = query_rows(&engine, &session, "SELECT * FROM pg_catalog.pg_class");

    // At least the users table should appear.
    let table_names: Vec<&str> = rows
        .iter()
        .filter(|r| text_col(r, 3) == "r") // relkind = 'r' for table
        .map(|r| text_col(r, 1))
        .collect();
    assert!(
        table_names.contains(&"users"),
        "expected 'users' in {table_names:?}"
    );
}

#[test]
fn pg_class_empty_database() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    // Filter out system catalog entries; only look at user tables in 'public'.
    let rows = query_rows(
        &engine,
        &session,
        "SELECT * FROM pg_catalog.pg_class \
         WHERE relnamespace = (SELECT oid FROM pg_namespace WHERE nspname = 'public')",
    );
    assert!(rows.is_empty());
}

#[test]
fn pg_class_relkind_is_r_for_tables() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(&session, "CREATE TABLE t1 (id INT NOT NULL)")
        .expect("create");

    let rows = query_rows(&engine, &session, "SELECT * FROM pg_catalog.pg_class");
    let table_row = rows
        .iter()
        .find(|r| text_col(r, 1) == "t1")
        .expect("t1 should exist");
    assert_eq!(text_col(table_row, 3), "r");
}

// ---------------------------------------------------------------
// pg_catalog.pg_attribute
// ---------------------------------------------------------------

#[test]
fn pg_attribute_returns_table_columns() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE TABLE products (id INT NOT NULL, name TEXT, price REAL NOT NULL)",
        )
        .expect("create");

    let results = engine
        .execute_sql(&session, "SELECT * FROM pg_catalog.pg_attribute")
        .expect("query");

    let (columns, rows) = match &results[0] {
        StatementResult::Query { columns, rows } => (columns, rows),
        other => panic!("expected Query, got {other:?}"),
    };

    assert_eq!(columns.len(), 19);
    assert_eq!(columns[0].name, "attrelid");
    assert_eq!(columns[1].name, "attname");
    assert_eq!(columns[2].name, "atttypid");
    assert_eq!(columns[3].name, "attnum");
    assert_eq!(columns[4].name, "attnotnull");
    assert_eq!(columns[5].name, "attisdropped");
    assert_eq!(columns[6].name, "atttypmod");
    assert_eq!(columns[7].name, "atthasdef");
    assert_eq!(columns[8].name, "attgenerated");
    assert_eq!(columns[9].name, "attidentity");
    assert_eq!(columns[10].name, "attcollation");
    assert_eq!(columns[11].name, "attlen");
    assert_eq!(columns[12].name, "attalign");
    assert_eq!(columns[13].name, "attstorage");
    assert_eq!(columns[14].name, "attcompression");
    assert_eq!(columns[15].name, "attinhcount");
    assert_eq!(columns[16].name, "attstattarget");
    assert_eq!(columns[17].name, "attislocal");

    // At least 3 user-defined columns (system catalog columns may also appear).
    let user_rows: Vec<_> = rows
        .iter()
        .filter(|r| {
            let n = text_col(r, 1);
            n == "id" || n == "name" || n == "price"
        })
        .collect();
    assert_eq!(user_rows.len(), 3);

    let id_row = rows
        .iter()
        .find(|r| text_col(r, 1) == "id")
        .expect("id column");
    assert_eq!(int_col(id_row, 2), 23); // int4 OID
    assert_eq!(int_col(id_row, 3), 1); // attnum
    assert!(bool_col(id_row, 4)); // attnotnull = true
    assert!(!bool_col(id_row, 5)); // attisdropped = false

    let name_row = rows
        .iter()
        .find(|r| text_col(r, 1) == "name")
        .expect("name column");
    assert_eq!(int_col(name_row, 2), 25); // text OID
    assert_eq!(int_col(name_row, 3), 2); // attnum
    assert!(!bool_col(name_row, 4)); // attnotnull = false (nullable)

    let price_row = rows
        .iter()
        .find(|r| text_col(r, 1) == "price")
        .expect("price column");
    assert_eq!(int_col(price_row, 2), 700); // float4 OID
    assert!(bool_col(price_row, 4)); // attnotnull = true
}

// ---------------------------------------------------------------
// pg_catalog.pg_type
// ---------------------------------------------------------------

#[test]
fn pg_type_returns_known_types() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let results = engine
        .execute_sql(&session, "SELECT * FROM pg_catalog.pg_type")
        .expect("query");

    let (columns, rows) = match &results[0] {
        StatementResult::Query { columns, rows } => (columns, rows),
        other => panic!("expected Query, got {other:?}"),
    };

    assert!(columns.len() >= 23);
    assert_eq!(columns[0].name, "oid");
    assert_eq!(columns[1].name, "typname");
    assert_eq!(columns[8].name, "typcollation");

    let type_names: Vec<&str> = rows.iter().map(|r| text_col(r, 1)).collect();
    assert!(type_names.contains(&"int4"));
    assert!(type_names.contains(&"int8"));
    assert!(type_names.contains(&"text"));
    assert!(type_names.contains(&"bool"));
    assert!(type_names.contains(&"float4"));
    assert!(type_names.contains(&"float8"));
    assert!(type_names.contains(&"numeric"));
    assert!(type_names.contains(&"timestamp"));
    assert!(type_names.contains(&"date"));
    assert!(type_names.contains(&"uuid"));
    assert!(type_names.contains(&"bytea"));

    // Verify int4 OID = 23
    let int4 = rows
        .iter()
        .find(|r| text_col(r, 1) == "int4")
        .expect("int4");
    assert_eq!(int_col(int4, 0), 23);
    assert_eq!(int_col(int4, 4), 4); // typlen = 4
}

// ---------------------------------------------------------------
// pg_catalog.pg_index
// ---------------------------------------------------------------

#[test]
fn pg_index_empty_without_indexes() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(&session, "CREATE TABLE t (id INT NOT NULL)")
        .expect("create");

    let rows = query_rows(&engine, &session, "SELECT * FROM pg_catalog.pg_index");
    // No explicit indexes created (PK doesn't generate a catalog index in AionDB).
    assert!(rows.is_empty());
}

// ---------------------------------------------------------------
// pg_catalog.pg_constraint
// ---------------------------------------------------------------

#[test]
fn pg_constraint_lists_primary_key() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE TABLE orders (id INT NOT NULL, PRIMARY KEY (id))",
        )
        .expect("create");

    let results = engine
        .execute_sql(&session, "SELECT * FROM pg_catalog.pg_constraint")
        .expect("query");

    let (columns, rows) = match &results[0] {
        StatementResult::Query { columns, rows } => (columns, rows),
        other => panic!("expected Query, got {other:?}"),
    };

    assert!(columns.len() >= 17);
    assert_eq!(columns[0].name, "oid");
    assert_eq!(columns[1].name, "conname");
    assert_eq!(columns[3].name, "contype");
    assert_eq!(columns[4].name, "conrelid");
    assert_eq!(columns[5].name, "contypid");
    assert_eq!(columns[15].name, "conislocal");
    assert_eq!(columns[16].name, "coninhcount");

    // At least one PK constraint.
    let pk_rows: Vec<_> = rows.iter().filter(|r| text_col(r, 3) == "p").collect();
    assert!(
        !pk_rows.is_empty(),
        "expected at least one primary key constraint"
    );

    let conname = text_col(pk_rows[0], 1);
    assert!(
        conname.contains("pkey"),
        "expected pkey in constraint name, got {conname}"
    );
}

// ---------------------------------------------------------------
// DDL changes reflected in pg_class and pg_attribute
// ---------------------------------------------------------------

#[test]
fn new_table_appears_in_pg_class_and_pg_attribute() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let public_ns_filter =
        "WHERE relnamespace = (SELECT oid FROM pg_namespace WHERE nspname = 'public')";

    // Initially empty (user tables only).
    let rows_before = query_rows(
        &engine,
        &session,
        &format!("SELECT * FROM pg_catalog.pg_class {public_ns_filter}"),
    );
    assert!(rows_before.is_empty());

    let attr_before = query_rows(&engine, &session, "SELECT * FROM pg_catalog.pg_attribute");
    // Engine now exposes system catalog columns in pg_attribute too,
    // so we remember the baseline row count.
    let baseline_attr_rows = attr_before.len();

    // Create a table.
    engine
        .execute_sql(
            &session,
            "CREATE TABLE widgets (id INT NOT NULL, label TEXT)",
        )
        .expect("create");

    // Table should now appear.
    let rows_after = query_rows(
        &engine,
        &session,
        &format!("SELECT * FROM pg_catalog.pg_class {public_ns_filter}"),
    );
    let table_names: Vec<&str> = rows_after
        .iter()
        .filter(|r| text_col(r, 3) == "r")
        .map(|r| text_col(r, 1))
        .collect();
    assert!(
        table_names.contains(&"widgets"),
        "expected 'widgets' in {table_names:?}"
    );

    // Columns should now appear.
    let attr_after = query_rows(&engine, &session, "SELECT * FROM pg_catalog.pg_attribute");
    assert!(attr_after.len() > baseline_attr_rows);
    let col_names: Vec<&str> = attr_after.iter().map(|r| text_col(r, 1)).collect();
    assert!(col_names.contains(&"id"));
    assert!(col_names.contains(&"label"));
}

#[test]
fn dropped_table_disappears_from_pg_class_and_pg_attribute() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let public_ns_filter =
        "WHERE relnamespace = (SELECT oid FROM pg_namespace WHERE nspname = 'public')";

    engine
        .execute_sql(&session, "CREATE TABLE temp (x INT NOT NULL)")
        .expect("create");

    // Verify it exists (user tables only).
    let rows = query_rows(
        &engine,
        &session,
        &format!("SELECT * FROM pg_catalog.pg_class {public_ns_filter}"),
    );
    assert!(!rows.is_empty());
    let attrs = query_rows(&engine, &session, "SELECT * FROM pg_catalog.pg_attribute");
    assert!(!attrs.is_empty());

    // Drop it.
    engine
        .execute_sql(&session, "DROP TABLE temp")
        .expect("drop");

    // Verify it's gone (user tables only; system catalog entries remain).
    let rows_after = query_rows(
        &engine,
        &session,
        &format!("SELECT * FROM pg_catalog.pg_class {public_ns_filter}"),
    );
    assert!(
        rows_after.is_empty(),
        "pg_class should have no user tables after DROP TABLE"
    );
    let attrs_after = query_rows(&engine, &session, "SELECT * FROM pg_catalog.pg_attribute");
    let user_cols_after: Vec<&str> = attrs_after.iter().map(|r| text_col(r, 1)).collect();
    assert!(
        !user_cols_after.contains(&"x"),
        "user column should be gone after DROP TABLE: {user_cols_after:?}"
    );
}

// ---------------------------------------------------------------
// Case-insensitive schema name
// ---------------------------------------------------------------

#[test]
fn case_insensitive_pg_catalog_schema() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let results = engine
        .execute_sql(&session, "SELECT * FROM PG_CATALOG.pg_namespace")
        .expect("query");
    match &results[0] {
        StatementResult::Query { rows, .. } => {
            assert!(!rows.is_empty());
        }
        other => panic!("expected Query, got {other:?}"),
    }
}

// ---------------------------------------------------------------
// Unsupported pg_catalog table
// ---------------------------------------------------------------

#[test]
fn pg_catalog_settings_is_queryable() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(&session, "SELECT * FROM pg_catalog.pg_settings")
        .expect("pg_settings should be queryable");
}

#[test]
fn pg_catalog_sysviews_easy_win_views_are_queryable() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let pg_config = query_rows(&engine, &session, "SELECT count(*) > 20 FROM pg_config");
    assert_eq!(pg_config.len(), 1);
    assert!(bool_col(&pg_config[0], 0));

    let memory_contexts = query_rows(
        &engine,
        &session,
        "SELECT name, ident, parent, level, total_bytes >= free_bytes \
         FROM pg_backend_memory_contexts WHERE level = 0",
    );
    assert_eq!(memory_contexts.len(), 1);
    assert_eq!(text_col(&memory_contexts[0], 0), "TopMemoryContext");
    assert!(matches!(memory_contexts[0].values[1], Value::Null));
    assert!(matches!(memory_contexts[0].values[2], Value::Null));
    assert_eq!(int_col(&memory_contexts[0], 3), 0);
    assert!(bool_col(&memory_contexts[0], 4));

    let pg_locks = query_rows(&engine, &session, "SELECT count(*) > 0 FROM pg_locks");
    assert_eq!(pg_locks.len(), 1);
    assert!(bool_col(&pg_locks[0], 0));

    let pg_hba = query_rows(
        &engine,
        &session,
        "SELECT count(*) > 0, count(*) FILTER (WHERE error IS NOT NULL) = 0 \
         FROM pg_hba_file_rules",
    );
    assert_eq!(pg_hba.len(), 1);
    assert!(bool_col(&pg_hba[0], 0));
    assert!(bool_col(&pg_hba[0], 1));

    let timezone_names = query_rows(
        &engine,
        &session,
        "SELECT count(distinct utc_offset) >= 24 FROM pg_timezone_names",
    );
    assert_eq!(timezone_names.len(), 1);
    assert!(bool_col(&timezone_names[0], 0));

    let planner_flags = query_rows(
        &engine,
        &session,
        "SELECT name, setting FROM pg_settings WHERE name LIKE 'enable%' ORDER BY name",
    );
    assert!(planner_flags.len() >= 21);
    assert_eq!(text_col(&planner_flags[0], 0), "enable_async_append");
}

#[test]
fn pg_catalog_auth_tables_are_queryable() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    for sql in [
        "SELECT * FROM pg_catalog.pg_authid",
        "SELECT * FROM pg_catalog.pg_roles",
        "SELECT * FROM pg_catalog.pg_database",
        "SELECT count(*) > 0 FROM pg_catalog.pg_init_privs",
    ] {
        engine.execute_sql(&session, sql).expect("should succeed");
    }
}
