use super::*;
use crate::SqlState;

fn text_col(row: &Row, idx: usize) -> &str {
    match &row.values[idx] {
        Value::Text(value) => value,
        other => panic!("expected text at column {idx}, got {other:?}"),
    }
}

fn bool_col(row: &Row, idx: usize) -> bool {
    match row.values[idx] {
        Value::Boolean(value) => value,
        ref other => panic!("expected boolean at column {idx}, got {other:?}"),
    }
}

#[test]
fn show_and_pg_settings_share_runtime_compat_defaults() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    for (show_sql, setting_name) in [
        ("SHOW lc_collate", "lc_collate"),
        ("SHOW lc_ctype", "lc_ctype"),
        ("SHOW TimeZone", "TimeZone"),
        ("SHOW server_version", "server_version"),
        ("SHOW hnsw.ef_search", "hnsw.ef_search"),
        ("SHOW hnsw.iterative_scan", "hnsw.iterative_scan"),
        ("SHOW hnsw.max_scan_tuples", "hnsw.max_scan_tuples"),
        ("SHOW hnsw.scan_mem_multiplier", "hnsw.scan_mem_multiplier"),
        ("SHOW ivfflat.probes", "ivfflat.probes"),
        ("SHOW ivfflat.iterative_scan", "ivfflat.iterative_scan"),
        ("SHOW ivfflat.max_probes", "ivfflat.max_probes"),
    ] {
        let show_rows = query_rows(&engine, &session, show_sql);
        let setting_rows = query_rows(
            &engine,
            &session,
            &format!("SELECT setting FROM pg_settings WHERE name = '{setting_name}'"),
        );
        assert_eq!(
            show_rows.len(),
            1,
            "SHOW should return one row for {show_sql}"
        );
        assert_eq!(
            setting_rows.len(),
            1,
            "pg_settings should expose one row for {setting_name}"
        );
        assert_eq!(text_col(&show_rows[0], 0), text_col(&setting_rows[0], 0));
    }
}

#[test]
fn pgvector_runtime_settings_can_be_overridden() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "SET hnsw.ef_search = 128; \
             SET hnsw.iterative_scan = relaxed_order; \
             SET hnsw.max_scan_tuples = 50000; \
             SET hnsw.scan_mem_multiplier = 2.5; \
             SET ivfflat.probes = 4; \
             SET ivfflat.iterative_scan = strict_order; \
             SET ivfflat.max_probes = 64",
        )
        .expect("set pgvector runtime options");

    let rows = query_rows(
        &engine,
        &session,
        "SELECT current_setting('hnsw.ef_search'), \
                current_setting('hnsw.iterative_scan'), \
                current_setting('hnsw.max_scan_tuples'), \
                current_setting('hnsw.scan_mem_multiplier'), \
                current_setting('ivfflat.probes'), \
                current_setting('ivfflat.iterative_scan'), \
                current_setting('ivfflat.max_probes')",
    );
    assert_eq!(rows.len(), 1);
    assert_eq!(text_col(&rows[0], 0), "128");
    assert_eq!(text_col(&rows[0], 1), "relaxed_order");
    assert_eq!(text_col(&rows[0], 2), "50000");
    assert_eq!(text_col(&rows[0], 3), "2.5");
    assert_eq!(text_col(&rows[0], 4), "4");
    assert_eq!(text_col(&rows[0], 5), "strict_order");
    assert_eq!(text_col(&rows[0], 6), "64");
}

#[test]
fn pgvector_runtime_settings_reject_non_positive_values() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    for sql in [
        "SET hnsw.ef_search = 0",
        "SET hnsw.max_scan_tuples = 0",
        "SET hnsw.scan_mem_multiplier = 0",
        "SET hnsw.iterative_scan = precise_order",
        "SET ivfflat.probes = 0",
        "SET ivfflat.max_probes = 0",
        "SET ivfflat.iterative_scan = precise_order",
    ] {
        let error = engine
            .execute_sql(&session, sql)
            .expect_err("non-positive pgvector setting should fail");
        assert_eq!(error.sqlstate(), SqlState::InvalidParameterValue);
    }

    let error = engine
        .execute_sql(&session, "SELECT set_config('hnsw.ef_search', '0', false)")
        .expect_err("set_config should validate pgvector settings");
    assert_eq!(error.sqlstate(), SqlState::InvalidParameterValue);
}

#[test]
fn show_unknown_configuration_parameter_reports_undefined_object() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let error = engine
        .execute_sql(&session, "SHOW no_such_setting")
        .expect_err("unknown SHOW variable should fail");
    assert_eq!(error.report().sqlstate, SqlState::UndefinedObject);
    assert!(error
        .report()
        .message
        .contains("unrecognized configuration parameter"));
}

#[test]
fn describe_show_unknown_configuration_parameter_reports_undefined_object() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .prepare(
            &session,
            "show_missing_setting".to_owned(),
            "SHOW no_such_setting".to_owned(),
        )
        .expect("prepare show should succeed before describe");

    let error = engine
        .describe_statement(&session, "show_missing_setting")
        .expect_err("describe unknown SHOW variable should fail");
    assert_eq!(error.report().sqlstate, SqlState::UndefinedObject);
    assert!(error
        .report()
        .message
        .contains("unrecognized configuration parameter"));
}

#[test]
fn set_statement_timeout_invalid_value_reports_invalid_parameter_value() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let error = engine
        .execute_sql(&session, "SET statement_timeout = bogus")
        .expect_err("invalid statement_timeout should fail");
    assert_eq!(error.report().sqlstate, SqlState::InvalidParameterValue);
    assert!(error
        .report()
        .message
        .contains("invalid value for timeout parameter"));
}

#[test]
fn describe_set_statement_timeout_invalid_value_reports_invalid_parameter_value() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .prepare(
            &session,
            "set_statement_timeout_bogus".to_owned(),
            "SET statement_timeout = bogus".to_owned(),
        )
        .expect("prepare set statement_timeout should succeed before describe");

    let error = engine
        .describe_statement(&session, "set_statement_timeout_bogus")
        .expect_err("describe invalid statement_timeout should fail");
    assert_eq!(error.report().sqlstate, SqlState::InvalidParameterValue);
    assert!(error
        .report()
        .message
        .contains("invalid value for timeout parameter"));
}

#[test]
fn set_statement_timeout_overflow_reports_invalid_parameter_value() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let error = engine
        .execute_sql(&session, "SET statement_timeout = '18446744073709551615h'")
        .expect_err("overflowing statement_timeout should fail");
    assert_eq!(error.report().sqlstate, SqlState::InvalidParameterValue);
    assert!(error
        .report()
        .message
        .contains("invalid value for timeout parameter"));
}

#[test]
fn set_custom_session_variables_hits_program_limit() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    for i in 0..400 {
        let sql = format!("SET custom_session_var_{i} = '{i}'");
        match engine.execute_sql(&session, &sql) {
            Ok(_) => {}
            Err(error) => {
                assert_eq!(error.sqlstate(), SqlState::ProgramLimitExceeded);
                assert!(error.to_string().contains("too many session variables"));
                return;
            }
        }
    }

    panic!("expected session variable cardinality limit to trigger");
}

#[test]
fn set_local_custom_session_variables_hits_program_limit() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");
    engine.execute_sql(&session, "BEGIN").expect("begin");

    for i in 0..400 {
        let sql = format!("SET LOCAL custom_local_var_{i} = '{i}'");
        match engine.execute_sql(&session, &sql) {
            Ok(_) => {}
            Err(error) => {
                assert_eq!(error.sqlstate(), SqlState::ProgramLimitExceeded);
                assert!(error
                    .to_string()
                    .contains("too many transaction-local session variables"));
                engine.execute_sql(&session, "ROLLBACK").expect("rollback");
                return;
            }
        }
    }

    engine.execute_sql(&session, "ROLLBACK").expect("rollback");
    panic!("expected transaction-local session variable cardinality limit to trigger");
}

#[test]
fn set_local_overrides_current_setting_inside_transaction_and_reverts_after_commit() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(&session, "SET application_name = outer_app")
        .expect("set outer application_name");
    engine.execute_sql(&session, "BEGIN").expect("begin");
    engine
        .execute_sql(&session, "SET LOCAL application_name = inner_app")
        .expect("set local application_name");

    let rows = query_rows(
        &engine,
        &session,
        "SELECT current_setting('application_name')",
    );
    assert_eq!(text_col(&rows[0], 0), "inner_app");

    engine.execute_sql(&session, "COMMIT").expect("commit");

    let rows = query_rows(
        &engine,
        &session,
        "SELECT current_setting('application_name')",
    );
    assert_eq!(text_col(&rows[0], 0), "outer_app");
}

#[test]
fn set_config_updates_session_setting_globally() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let rows = query_rows(
        &engine,
        &session,
        "SELECT set_config('application_name', 'func_app', false)",
    );
    assert_eq!(text_col(&rows[0], 0), "func_app");

    let rows = query_rows(&engine, &session, "SHOW application_name");
    assert_eq!(text_col(&rows[0], 0), "func_app");
}

#[test]
fn set_config_with_local_true_is_transaction_local() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(&session, "SET application_name = outer_app")
        .expect("set outer application_name");
    engine.execute_sql(&session, "BEGIN").expect("begin");

    let rows = query_rows(
        &engine,
        &session,
        "SELECT set_config('application_name', 'local_func_app', true)",
    );
    assert_eq!(text_col(&rows[0], 0), "local_func_app");

    let rows = query_rows(&engine, &session, "SHOW application_name");
    assert_eq!(text_col(&rows[0], 0), "local_func_app");

    engine.execute_sql(&session, "COMMIT").expect("commit");

    let rows = query_rows(&engine, &session, "SHOW application_name");
    assert_eq!(text_col(&rows[0], 0), "outer_app");
}

#[test]
fn set_config_statement_timeout_updates_effective_limits() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine.execute_sql(&session, "BEGIN").expect("begin");
    let rows = query_rows(
        &engine,
        &session,
        "SELECT set_config('statement_timeout', '5000', true)",
    );
    assert_eq!(text_col(&rows[0], 0), "5000");

    let info = engine.session_info(&session).expect("session info");
    assert_eq!(
        info.limits.statement_timeout,
        std::time::Duration::from_secs(5)
    );

    engine.execute_sql(&session, "COMMIT").expect("commit");

    let info = engine.session_info(&session).expect("session info");
    assert_eq!(
        info.limits.statement_timeout,
        std::time::Duration::from_secs(30)
    );
}

#[test]
fn set_config_max_parallel_workers_updates_effective_limits() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine.execute_sql(&session, "BEGIN").expect("begin");
    let rows = query_rows(
        &engine,
        &session,
        "SELECT set_config('max_parallel_workers_per_query', '3', true)",
    );
    assert_eq!(text_col(&rows[0], 0), "3");

    let info = engine.session_info(&session).expect("session info");
    assert_eq!(info.limits.max_parallel_workers_per_query, 3);

    engine.execute_sql(&session, "COMMIT").expect("commit");

    let info = engine.session_info(&session).expect("session info");
    assert_eq!(info.limits.max_parallel_workers_per_query, 1);
}

#[test]
fn set_config_max_parallel_workers_rejects_zero() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let error = engine
        .execute_sql(
            &session,
            "SELECT set_config('max_parallel_workers_per_query', '0', false)",
        )
        .expect_err("zero max_parallel_workers_per_query must fail");
    assert_eq!(error.sqlstate(), SqlState::InvalidParameterValue);
}

#[test]
fn show_and_reset_max_parallel_workers_round_trip_to_default() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let rows = query_rows(&engine, &session, "SHOW max_parallel_workers_per_query");
    assert_eq!(text_col(&rows[0], 0), "1");

    engine
        .execute_sql(&session, "SET max_parallel_workers_per_query = 6")
        .expect("set max_parallel_workers_per_query");
    let rows = query_rows(&engine, &session, "SHOW max_parallel_workers_per_query");
    assert_eq!(text_col(&rows[0], 0), "6");

    engine
        .execute_sql(&session, "RESET max_parallel_workers_per_query")
        .expect("reset max_parallel_workers_per_query");
    let rows = query_rows(&engine, &session, "SHOW max_parallel_workers_per_query");
    assert_eq!(text_col(&rows[0], 0), "1");
}

#[test]
fn set_config_distributed_loopback_nodes_rejects_empty_entries() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let error = engine
        .execute_sql(
            &session,
            "SELECT set_config('distributed_loopback_nodes', 'node-a,,node-b', false)",
        )
        .expect_err("invalid distributed_loopback_nodes must fail");
    assert_eq!(error.sqlstate(), SqlState::InvalidParameterValue);
}

#[test]
fn show_set_reset_distributed_loopback_nodes_round_trip() {
    let mut runtime = aiondb_config::RuntimeConfig::default();
    runtime.distributed.loopback_remote_nodes = vec!["node-a".to_owned()];
    let engine = EngineBuilder::for_testing()
        .with_runtime_config(runtime)
        .build()
        .unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let rows = query_rows(&engine, &session, "SHOW distributed_loopback_nodes");
    assert_eq!(text_col(&rows[0], 0), "node-a");

    engine
        .execute_sql(&session, "SET distributed_loopback_nodes = 'node-x,node-y'")
        .expect("set distributed_loopback_nodes");
    let rows = query_rows(&engine, &session, "SHOW distributed_loopback_nodes");
    assert_eq!(text_col(&rows[0], 0), "node-x,node-y");

    engine
        .execute_sql(&session, "RESET distributed_loopback_nodes")
        .expect("reset distributed_loopback_nodes");
    let rows = query_rows(&engine, &session, "SHOW distributed_loopback_nodes");
    assert_eq!(text_col(&rows[0], 0), "node-a");
}

#[test]
fn describe_set_client_encoding_invalid_value_reports_feature_not_supported() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .prepare(
            &session,
            "set_client_encoding_invalid".to_owned(),
            "SET client_encoding = latin1".to_owned(),
        )
        .expect("prepare set client_encoding should succeed before describe");

    let error = engine
        .describe_statement(&session, "set_client_encoding_invalid")
        .expect_err("describe invalid client_encoding should fail");
    assert_eq!(error.report().sqlstate, SqlState::FeatureNotSupported);
    assert!(error
        .report()
        .message
        .contains("only UTF8 encoding is supported"));
}

#[test]
fn pg_log_backend_memory_contexts_is_available_directly_and_via_pg_stat_activity() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let direct = query_rows(
        &engine,
        &session,
        "SELECT pg_log_backend_memory_contexts(pg_backend_pid())",
    );
    assert_eq!(direct.len(), 1);
    assert!(bool_col(&direct[0], 0));

    let via_pg_stat_activity = query_rows(
        &engine,
        &session,
        "SELECT pg_log_backend_memory_contexts(pid) \
         FROM pg_stat_activity \
         WHERE backend_type = 'checkpointer'",
    );
    assert_eq!(via_pg_stat_activity.len(), 1);
    assert!(bool_col(&via_pg_stat_activity[0], 0));
}
