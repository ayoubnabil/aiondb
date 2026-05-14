use super::*;
use crate::Row;
use aiondb_core::Value;

#[path = "limits_and_validation_portals_and_negatives.rs"]
mod portals_and_negatives;

#[test]
fn statement_timeout_zero_disables_execution_timeout() {
    let mut runtime = aiondb_config::RuntimeConfig::default();
    runtime.limits.statement_timeout = std::time::Duration::ZERO;

    let engine = EngineBuilder::for_testing()
        .with_runtime_config(runtime)
        .build()
        .unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let results = engine
        .execute_sql(&session, "SELECT 1")
        .expect("zero statement_timeout should disable timeout");
    assert!(matches!(
        results.last(),
        Some(StatementResult::Query { .. })
    ));
}

#[test]
fn transaction_idle_timeout_rolls_back_active_transaction() {
    let mut runtime = aiondb_config::RuntimeConfig::default();
    runtime.security.max_transaction_idle_timeout = Some(std::time::Duration::from_millis(25));

    let engine = EngineBuilder::for_testing()
        .with_runtime_config(runtime)
        .build()
        .unwrap();
    let (session_a, _) = engine.startup(startup_params()).expect("startup A");
    let (session_b, _) = engine.startup(startup_params()).expect("startup B");

    engine
        .execute_sql(&session_a, "CREATE TABLE txn_idle_timeout_t (id INT)")
        .expect("create table");
    engine
        .execute_sql(
            &session_a,
            "BEGIN; INSERT INTO txn_idle_timeout_t VALUES (1)",
        )
        .expect("begin+insert");

    std::thread::sleep(std::time::Duration::from_millis(70));

    let timeout_error = engine
        .execute_sql(&session_a, "SELECT 1")
        .expect_err("transaction idle timeout should trigger");
    assert_eq!(
        timeout_error.sqlstate(),
        aiondb_core::SqlState::IdleInTransactionSessionTimeout
    );
    assert!(
        timeout_error
            .to_string()
            .contains("transaction idle timeout exceeded"),
        "unexpected error: {timeout_error}"
    );

    engine
        .execute_sql(&session_a, "COMMIT")
        .expect("commit after timeout should be a no-op");

    let rows = query_rows(
        &engine,
        &session_b,
        "SELECT EXISTS(SELECT 1 FROM txn_idle_timeout_t WHERE id = 1)",
    );
    assert_eq!(rows.len(), 1);
    match &rows[0].values[0] {
        Value::Boolean(exists) => assert!(!exists, "timed-out transaction should be rolled back"),
        other => panic!("expected BOOLEAN, got {other:?}"),
    }
}

#[test]
fn session_idle_timeout_reports_idle_session_timeout_sqlstate() {
    let mut runtime = aiondb_config::RuntimeConfig::default();
    runtime.security.max_session_idle_timeout = Some(std::time::Duration::from_millis(25));

    let engine = EngineBuilder::for_testing()
        .with_runtime_config(runtime)
        .build()
        .unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    std::thread::sleep(std::time::Duration::from_millis(70));

    let error = engine
        .execute_sql(&session, "SELECT 1")
        .expect_err("session idle timeout should trigger");
    assert_eq!(error.sqlstate(), aiondb_core::SqlState::IdleSessionTimeout);
    assert!(error.to_string().contains("session idle timeout exceeded"));
}

#[test]
fn session_lifetime_reports_admin_shutdown_sqlstate() {
    let mut runtime = aiondb_config::RuntimeConfig::default();
    runtime.security.max_session_lifetime = Some(std::time::Duration::from_millis(25));

    let engine = EngineBuilder::for_testing()
        .with_runtime_config(runtime)
        .build()
        .unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    std::thread::sleep(std::time::Duration::from_millis(70));

    let error = engine
        .execute_sql(&session, "SELECT 1")
        .expect_err("session lifetime should trigger");
    assert_eq!(error.sqlstate(), aiondb_core::SqlState::AdminShutdown);
    assert!(error.to_string().contains("session lifetime exceeded"));
}

#[test]
fn active_transaction_is_not_purged_when_recently_active() {
    let mut runtime = aiondb_config::RuntimeConfig::default();
    runtime.security.max_transaction_idle_timeout = Some(std::time::Duration::from_millis(250));

    let engine = EngineBuilder::for_testing()
        .with_runtime_config(runtime)
        .build()
        .unwrap();
    let (session_a, _) = engine.startup(startup_params()).expect("startup A");

    engine
        .execute_sql(&session_a, "CREATE TABLE txn_keepalive_t (id INT)")
        .expect("create table");
    engine
        .execute_sql(&session_a, "BEGIN; INSERT INTO txn_keepalive_t VALUES (7)")
        .expect("begin+insert");

    // Keep the transaction active across a wall-clock interval longer than
    // max_transaction_idle_timeout.
    for _ in 0..3 {
        std::thread::sleep(std::time::Duration::from_millis(90));
        engine
            .execute_sql(&session_a, "SELECT 1")
            .expect("keepalive statement inside transaction");
    }

    // Startup triggers purge_expired_sessions(); a transaction should not be
    // purged just because of age when it is still active.
    let (session_b, _) = engine.startup(startup_params()).expect("startup B");

    engine
        .execute_sql(&session_a, "COMMIT")
        .expect("transaction should still be committable");

    let rows = query_rows(
        &engine,
        &session_b,
        "SELECT EXISTS(SELECT 1 FROM txn_keepalive_t WHERE id = 7)",
    );
    assert_eq!(rows.len(), 1);
    match &rows[0].values[0] {
        Value::Boolean(exists) => assert!(*exists, "active transaction was incorrectly purged"),
        other => panic!("expected BOOLEAN, got {other:?}"),
    }
}

#[test]
fn result_byte_limit_is_enforced() {
    let mut runtime = aiondb_config::RuntimeConfig::default();
    runtime.limits.max_result_bytes = 0;

    let engine = EngineBuilder::for_testing()
        .with_runtime_config(runtime)
        .build()
        .unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let error = engine
        .execute_sql(&session, "SELECT 1")
        .expect_err("result byte limit");
    assert_eq!(
        error.sqlstate(),
        aiondb_core::SqlState::ProgramLimitExceeded
    );
}

#[test]
fn memory_limit_is_enforced() {
    let mut runtime = aiondb_config::RuntimeConfig::default();
    // Set a very tight memory budget: 1 byte.
    runtime.limits.max_memory_bytes = 1;

    let engine = EngineBuilder::for_testing()
        .with_runtime_config(runtime)
        .build()
        .unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    // Create a table and insert enough rows that the scan exceeds the budget.
    engine
        .execute_sql(&session, "CREATE TABLE mem_test (id INT, name TEXT)")
        .expect("create");
    engine
        .execute_sql(
            &session,
            "INSERT INTO mem_test VALUES (1, 'hello'), (2, 'world')",
        )
        .expect("insert");

    let error = engine
        .execute_sql(&session, "SELECT * FROM mem_test")
        .expect_err("memory limit");
    assert_eq!(
        error.sqlstate(),
        aiondb_core::SqlState::ProgramLimitExceeded
    );
}

#[test]
fn memory_limit_is_enforced_for_select_literal() {
    let mut runtime = aiondb_config::RuntimeConfig::default();
    runtime.limits.max_memory_bytes = 1;

    let engine = EngineBuilder::for_testing()
        .with_runtime_config(runtime)
        .build()
        .unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let error = engine
        .execute_sql(&session, "SELECT 1")
        .expect_err("memory limit on literal select");
    assert_eq!(
        error.sqlstate(),
        aiondb_core::SqlState::ProgramLimitExceeded
    );
}

#[test]
fn alter_table_add_column_rewrite_respects_memory_budget() {
    let mut runtime = aiondb_config::RuntimeConfig::default();
    runtime.limits.max_memory_bytes = 64;

    let engine = EngineBuilder::for_testing()
        .with_runtime_config(runtime)
        .build()
        .unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let oversized_payload = "x".repeat(256);
    engine
        .execute_sql(
            &session,
            &format!(
                "CREATE TABLE alter_mem_test (id INT, payload TEXT); \
                 INSERT INTO alter_mem_test VALUES (1, '{oversized_payload}')"
            ),
        )
        .expect("setup");

    let error = engine
        .execute_sql(
            &session,
            "ALTER TABLE alter_mem_test ADD COLUMN extra INT DEFAULT 7",
        )
        .expect_err("ALTER TABLE rewrite should fail under tight memory budget");
    assert_eq!(
        error.sqlstate(),
        aiondb_core::SqlState::ProgramLimitExceeded
    );
}

#[test]
fn alter_table_add_column_rewrite_failure_does_not_publish_partial_state_in_explicit_txn() {
    let mut runtime = aiondb_config::RuntimeConfig::default();
    runtime.limits.max_memory_bytes = 64;

    let engine = EngineBuilder::for_testing()
        .with_runtime_config(runtime)
        .build()
        .unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let oversized_payload = "x".repeat(256);
    engine
        .execute_sql(
            &session,
            &format!(
                "CREATE TABLE alter_mem_test (id INT, payload TEXT); \
                 INSERT INTO alter_mem_test VALUES (1, '{oversized_payload}')"
            ),
        )
        .expect("setup");

    engine.execute_sql(&session, "BEGIN").expect("begin");
    let error = engine
        .execute_sql(
            &session,
            "ALTER TABLE alter_mem_test ADD COLUMN extra INT DEFAULT 7",
        )
        .expect_err("ALTER TABLE rewrite should fail under tight memory budget");
    assert_eq!(
        error.sqlstate(),
        aiondb_core::SqlState::ProgramLimitExceeded
    );

    // The statement-local rewrite must be rolled back so the transaction
    // stays usable and committable.
    engine
        .execute_sql(
            &session,
            "INSERT INTO alter_mem_test VALUES (2, 'committed_after_failure')",
        )
        .expect("transaction should stay usable after failed rewrite");
    engine
        .execute_sql(&session, "COMMIT")
        .expect("commit after failed rewrite");

    let rows = engine
        .execute_sql(
            &session,
            "SELECT EXISTS (SELECT 1 FROM alter_mem_test WHERE id = 2)",
        )
        .expect("insert after failed rewrite should commit");
    let StatementResult::Query { rows, .. } = &rows[0] else {
        panic!("expected query result");
    };
    assert_eq!(rows.as_slice(), &[Row::new(vec![Value::Boolean(true)])]);

    let error = engine
        .execute_sql(&session, "SELECT extra FROM alter_mem_test")
        .expect_err("failed rewrite must not publish the added column");
    assert_eq!(error.sqlstate(), aiondb_core::SqlState::UndefinedColumn);
}

#[test]
fn alter_table_add_column_rewrite_failure_can_be_rolled_back_to_user_savepoint() {
    let mut runtime = aiondb_config::RuntimeConfig::default();
    runtime.limits.max_memory_bytes = 64;

    let engine = EngineBuilder::for_testing()
        .with_runtime_config(runtime)
        .build()
        .unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let oversized_payload = "x".repeat(256);
    engine
        .execute_sql(
            &session,
            &format!(
                "CREATE TABLE alter_mem_test (id INT, payload TEXT); \
                 INSERT INTO alter_mem_test VALUES (1, '{oversized_payload}')"
            ),
        )
        .expect("setup");

    engine.execute_sql(&session, "BEGIN").expect("begin");
    engine
        .execute_sql(&session, "SAVEPOINT sp1")
        .expect("create user savepoint");

    let error = engine
        .execute_sql(
            &session,
            "ALTER TABLE alter_mem_test ADD COLUMN extra INT DEFAULT 7",
        )
        .expect_err("ALTER TABLE rewrite should fail under tight memory budget");
    assert_eq!(
        error.sqlstate(),
        aiondb_core::SqlState::ProgramLimitExceeded
    );

    engine
        .execute_sql(&session, "ROLLBACK TO SAVEPOINT sp1")
        .expect("rollback to user savepoint after failed rewrite");
    engine
        .execute_sql(
            &session,
            "INSERT INTO alter_mem_test VALUES (2, 'committed_after_savepoint')",
        )
        .expect("transaction should remain usable after rollback to savepoint");
    engine
        .execute_sql(&session, "COMMIT")
        .expect("commit after rollback to savepoint");

    let rows = engine
        .execute_sql(
            &session,
            "SELECT EXISTS (SELECT 1 FROM alter_mem_test WHERE id = 2)",
        )
        .expect("insert after rollback to savepoint should commit");
    let StatementResult::Query { rows, .. } = &rows[0] else {
        panic!("expected query result");
    };
    assert_eq!(rows.as_slice(), &[Row::new(vec![Value::Boolean(true)])]);

    let error = engine
        .execute_sql(&session, "SELECT extra FROM alter_mem_test")
        .expect_err("failed rewrite must not publish the added column");
    assert_eq!(error.sqlstate(), aiondb_core::SqlState::UndefinedColumn);
}

#[test]
fn temp_limit_is_enforced_for_select_literal() {
    let mut runtime = aiondb_config::RuntimeConfig::default();
    runtime.limits.max_temp_bytes = 1;

    let engine = EngineBuilder::for_testing()
        .with_runtime_config(runtime)
        .build()
        .unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let error = engine
        .execute_sql(&session, "SELECT 1")
        .expect_err("temporary workspace limit on literal select");
    assert_eq!(
        error.sqlstate(),
        aiondb_core::SqlState::ProgramLimitExceeded
    );
}

#[test]
fn memory_limit_is_enforced_for_join_result_collection() {
    let mut limits = aiondb_config::LimitsConfig::default();
    limits.max_memory_bytes = 24;
    let engine = build_engine_with_limits(limits);
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE TABLE mj_left (lx INT); \
             CREATE TABLE mj_right (ry INT); \
             INSERT INTO mj_left VALUES (1), (2); \
             INSERT INTO mj_right VALUES (10), (20)",
        )
        .expect("setup");

    let error = engine
        .execute_sql(
            &session,
            "SELECT lx, ry FROM mj_left, mj_right ORDER BY lx, ry",
        )
        .expect_err("join result should exceed memory budget");
    assert_eq!(
        error.sqlstate(),
        aiondb_core::SqlState::ProgramLimitExceeded
    );
}

#[test]
fn memory_limit_is_enforced_for_aggregate_output_on_empty_input() {
    let mut limits = aiondb_config::LimitsConfig::default();
    limits.max_memory_bytes = 1;
    let engine = build_engine_with_limits(limits);
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(&session, "CREATE TABLE empty_agg (id INT)")
        .expect("create");

    let error = engine
        .execute_sql(&session, "SELECT COUNT(*) AS cnt FROM empty_agg")
        .expect_err("aggregate output should exceed memory budget");
    assert_eq!(
        error.sqlstate(),
        aiondb_core::SqlState::ProgramLimitExceeded
    );
}

#[test]
fn expired_sessions_do_not_block_per_role_session_limits() {
    let mut runtime = aiondb_config::RuntimeConfig::default();
    runtime.security.max_concurrent_sessions_per_role = Some(1);
    runtime.security.max_session_idle_timeout = Some(std::time::Duration::ZERO);

    let engine = EngineBuilder::for_testing()
        .with_runtime_config(runtime)
        .build()
        .unwrap();
    let (_first, _) = engine.startup(startup_params()).expect("first startup");

    std::thread::sleep(std::time::Duration::from_millis(1));

    let (_second, _) = engine
        .startup(startup_params())
        .expect("expired session should be purged before admission");
}

#[test]
fn per_role_session_limits_report_too_many_connections() {
    let mut runtime = aiondb_config::RuntimeConfig::default();
    runtime.security.max_concurrent_sessions_per_role = Some(1);

    let engine = EngineBuilder::for_testing()
        .with_runtime_config(runtime)
        .build()
        .unwrap();
    let (_first, _) = engine.startup(startup_params()).expect("first startup");

    let error = engine
        .startup(startup_params())
        .expect_err("second session for same role should be rejected");
    assert_eq!(error.sqlstate(), aiondb_core::SqlState::TooManyConnections);
    assert!(error
        .report()
        .message
        .contains("too many sessions for role"));
}

// Limits and cancellation tests
// =========================================================================

fn build_engine_with_limits(limits: aiondb_config::LimitsConfig) -> Engine {
    let mut runtime = aiondb_config::RuntimeConfig::default();
    runtime.limits = limits;
    EngineBuilder::for_testing()
        .with_runtime_config(runtime)
        .build()
        .unwrap()
}

// ---- 1. SQL input limits ------------------------------------------------

#[test]
fn sql_exceeding_max_length_returns_program_limit() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let long_sql = "SELECT ".to_owned() + &"1,".repeat(crate::config::MAX_SQL_LENGTH) + "1";
    assert!(long_sql.len() > crate::config::MAX_SQL_LENGTH);

    let error = engine
        .execute_sql(&session, &long_sql)
        .expect_err("SQL too long");
    assert_eq!(
        error.sqlstate(),
        aiondb_core::SqlState::ProgramLimitExceeded
    );
}

#[test]
fn prepare_rejects_sql_exceeding_max_length() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let long_sql = "SELECT ".to_owned() + &"1,".repeat(crate::config::MAX_SQL_LENGTH) + "1";

    let error = engine
        .prepare(&session, "s1".to_owned(), long_sql)
        .expect_err("SQL too long in prepare");
    assert_eq!(
        error.sqlstate(),
        aiondb_core::SqlState::ProgramLimitExceeded
    );
}

#[test]
fn prepare_rejects_statement_name_exceeding_max_identifier_length() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let long_name = "s".repeat(crate::config::MAX_IDENTIFIER_LENGTH + 1);

    let error = engine
        .prepare(&session, long_name, "SELECT 1".to_owned())
        .expect_err("statement name too long");
    assert_eq!(
        error.sqlstate(),
        aiondb_core::SqlState::ProgramLimitExceeded
    );
}

#[test]
fn bind_rejects_portal_name_exceeding_max_identifier_length() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .prepare(&session, "s1".to_owned(), "SELECT 1".to_owned())
        .expect("prepare");

    let long_portal = "p".repeat(crate::config::MAX_IDENTIFIER_LENGTH + 1);

    let error = engine
        .bind(&session, long_portal, "s1".to_owned(), vec![])
        .expect_err("portal name too long");
    assert_eq!(
        error.sqlstate(),
        aiondb_core::SqlState::ProgramLimitExceeded
    );
}

#[test]
fn prepare_accepts_short_sql_and_empty_statement_name() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    // Empty statement name (unnamed prepared statement) is valid in the PostgreSQL protocol.
    let desc = engine
        .prepare(&session, String::new(), "SELECT 1".to_owned())
        .expect("prepare with empty name");
    assert_eq!(desc.result_columns.len(), 1);

    let replacement = engine
        .prepare(&session, String::new(), "SELECT 2".to_owned())
        .expect("reprepare unnamed statement");
    assert_eq!(replacement.result_columns.len(), 1);
}

#[test]
fn prepare_duplicate_named_statement_fails() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .prepare(&session, "s_dup".to_owned(), "SELECT 1".to_owned())
        .expect("first prepare");
    let error = engine
        .prepare(&session, "s_dup".to_owned(), "SELECT 2".to_owned())
        .expect_err("duplicate named prepare should fail");
    assert_eq!(error.sqlstate(), aiondb_core::SqlState::DuplicateObject);
    assert!(
        error.report().message.contains("already exists"),
        "unexpected error: {}",
        error.report().message
    );
}

#[test]
fn prepare_accepts_very_short_sql() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let desc = engine
        .prepare(&session, "s1".to_owned(), "SELECT 1".to_owned())
        .expect("prepare short SQL");
    assert_eq!(desc.result_columns.len(), 1);
}

// ---- 2. Result row and byte limits --------------------------------------

#[test]
fn max_result_rows_rejects_query_exceeding_limit() {
    let mut limits = aiondb_config::LimitsConfig::default();
    limits.max_result_rows = 2;
    let engine = build_engine_with_limits(limits);
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE TABLE t_rows (id INT); \
             INSERT INTO t_rows VALUES (1), (2), (3), (4), (5)",
        )
        .expect("setup");

    let error = engine
        .execute_sql(&session, "SELECT id FROM t_rows")
        .expect_err("max_result_rows exceeded");
    assert_eq!(
        error.sqlstate(),
        aiondb_core::SqlState::ProgramLimitExceeded
    );
}

#[test]
fn max_result_rows_allows_query_within_limit() {
    let mut limits = aiondb_config::LimitsConfig::default();
    limits.max_result_rows = 5;
    let engine = build_engine_with_limits(limits);
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE TABLE t_within (id INT); \
             INSERT INTO t_within VALUES (1), (2), (3)",
        )
        .expect("setup");

    let results = engine
        .execute_sql(&session, "SELECT id FROM t_within")
        .expect("within limit");
    match &results[0] {
        StatementResult::Query { rows, .. } => assert_eq!(rows.len(), 3),
        other => panic!("expected Query, got {other:?}"),
    }
}

#[test]
fn max_result_rows_rejects_multi_statement_cumulative_overflow() {
    let mut limits = aiondb_config::LimitsConfig::default();
    limits.max_result_rows = 3;
    let engine = build_engine_with_limits(limits);
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let error = engine
        .execute_sql(&session, "SELECT 1; SELECT 2; SELECT 3; SELECT 4")
        .expect_err("cumulative row limit exceeded");
    assert_eq!(
        error.sqlstate(),
        aiondb_core::SqlState::ProgramLimitExceeded
    );
}

#[test]
fn max_result_bytes_rejects_large_result() {
    let mut limits = aiondb_config::LimitsConfig::default();
    // Set a very small byte limit so even a single row overflows.
    limits.max_result_bytes = 1;
    let engine = build_engine_with_limits(limits);
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let error = engine
        .execute_sql(&session, "SELECT 'hello world'")
        .expect_err("max_result_bytes exceeded");
    assert_eq!(
        error.sqlstate(),
        aiondb_core::SqlState::ProgramLimitExceeded
    );
}

#[test]
fn max_result_bytes_allows_small_result() {
    let mut limits = aiondb_config::LimitsConfig::default();
    limits.max_result_bytes = 1024;
    let engine = build_engine_with_limits(limits);
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let results = engine
        .execute_sql(&session, "SELECT 1")
        .expect("small result within byte limit");
    assert_eq!(results.len(), 1);
}

#[test]
fn max_result_bytes_rejects_multi_statement_cumulative_overflow() {
    let mut limits = aiondb_config::LimitsConfig::default();
    limits.max_result_bytes = 10;
    let engine = build_engine_with_limits(limits);
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let error = engine
        .execute_sql(&session, "SELECT 'abcd'; SELECT 'abcd'; SELECT 'abcd'")
        .expect_err("cumulative byte limit exceeded");
    assert_eq!(
        error.sqlstate(),
        aiondb_core::SqlState::ProgramLimitExceeded
    );
}

#[test]
fn max_result_rows_rejects_cumulative_multi_statement_result() {
    let mut limits = aiondb_config::LimitsConfig::default();
    limits.max_result_rows = 3;
    let engine = build_engine_with_limits(limits);
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE TABLE t_multi_rows (id INT); INSERT INTO t_multi_rows VALUES (1), (2)",
        )
        .expect("setup");

    let err = engine
        .execute_sql(
            &session,
            "SELECT id FROM t_multi_rows ORDER BY id; SELECT id FROM t_multi_rows ORDER BY id",
        )
        .expect_err("cumulative row budget should be enforced");
    assert_eq!(err.sqlstate(), aiondb_core::SqlState::ProgramLimitExceeded);
    assert!(
        err.to_string()
            .contains("maximum number of result rows reached"),
        "unexpected error: {err}"
    );
}

#[test]
fn max_result_bytes_rejects_cumulative_multi_statement_result() {
    let mut limits = aiondb_config::LimitsConfig::default();
    limits.max_result_bytes = 10;
    let engine = build_engine_with_limits(limits);
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let err = engine
        .execute_sql(&session, "SELECT 'abcdef'; SELECT 'abcdef'")
        .expect_err("cumulative byte budget should be enforced");
    assert_eq!(err.sqlstate(), aiondb_core::SqlState::ProgramLimitExceeded);
    assert!(
        err.to_string()
            .contains("maximum number of result bytes reached"),
        "unexpected error: {err}"
    );
}

#[test]
fn portal_max_rows_paginates_result() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE TABLE t_page (id INT); \
             INSERT INTO t_page VALUES (1), (2), (3), (4), (5)",
        )
        .expect("setup");

    engine
        .prepare(
            &session,
            "q_page".to_owned(),
            "SELECT id FROM t_page".to_owned(),
        )
        .expect("prepare");
    engine
        .bind(&session, "p_page".to_owned(), "q_page".to_owned(), vec![])
        .expect("bind");

    // First fetch: max_rows = 2
    let batch1 = engine
        .execute_portal(&session, "p_page", 2)
        .expect("batch 1");
    assert_eq!(batch1.rows.len(), 2);
    assert!(!batch1.exhausted);

    // Second fetch: max_rows = 2
    let batch2 = engine
        .execute_portal(&session, "p_page", 2)
        .expect("batch 2");
    assert_eq!(batch2.rows.len(), 2);
    assert!(!batch2.exhausted);

    // Third fetch: only 1 remaining
    let batch3 = engine
        .execute_portal(&session, "p_page", 2)
        .expect("batch 3");
    assert_eq!(batch3.rows.len(), 1);
    assert!(batch3.exhausted);

    // Fourth fetch: portal is exhausted
    let batch4 = engine
        .execute_portal(&session, "p_page", 2)
        .expect("batch 4");
    assert_eq!(batch4.rows.len(), 0);
    assert!(batch4.exhausted);
}

#[test]
fn portal_pagination_does_not_recollect_consumed_prefix_under_tight_memory_budget() {
    let mut limits = aiondb_config::LimitsConfig::default();
    limits.max_memory_bytes = 12;
    let engine = build_engine_with_limits(limits);
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE TABLE t_page_tight (id INT); \
             INSERT INTO t_page_tight VALUES (1), (2), (3), (4), (5)",
        )
        .expect("setup");

    engine
        .prepare(
            &session,
            "q_page_tight".to_owned(),
            "SELECT id FROM t_page_tight".to_owned(),
        )
        .expect("prepare");
    engine
        .bind(
            &session,
            "p_page_tight".to_owned(),
            "q_page_tight".to_owned(),
            vec![],
        )
        .expect("bind");

    let batch1 = engine
        .execute_portal(&session, "p_page_tight", 1)
        .expect("batch 1");
    assert_eq!(batch1.rows, vec![Row::new(vec![Value::Int(1)])]);
    assert!(!batch1.exhausted);

    let batch2 = engine
        .execute_portal(&session, "p_page_tight", 1)
        .expect("batch 2");
    assert_eq!(batch2.rows, vec![Row::new(vec![Value::Int(2)])]);
    assert!(!batch2.exhausted);

    let batch3 = engine
        .execute_portal(&session, "p_page_tight", 1)
        .expect("batch 3");
    assert_eq!(batch3.rows, vec![Row::new(vec![Value::Int(3)])]);
    assert!(!batch3.exhausted);
}

#[test]
fn sql_limit_restricts_rows_even_below_max_result_rows() {
    let mut limits = aiondb_config::LimitsConfig::default();
    limits.max_result_rows = 100;
    let engine = build_engine_with_limits(limits);
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE TABLE t_sqlimit (id INT); \
             INSERT INTO t_sqlimit VALUES (1), (2), (3), (4), (5)",
        )
        .expect("setup");

    let results = engine
        .execute_sql(&session, "SELECT id FROM t_sqlimit LIMIT 2")
        .expect("SQL LIMIT");
    match &results[0] {
        StatementResult::Query { rows, .. } => assert_eq!(rows.len(), 2),
        other => panic!("expected Query, got {other:?}"),
    }
}

#[test]
fn sql_limit_null_means_no_limit() {
    let mut limits = aiondb_config::LimitsConfig::default();
    limits.max_result_rows = 100;
    let engine = build_engine_with_limits(limits);
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE TABLE t_limit_null (id INT); \
             INSERT INTO t_limit_null VALUES (1), (2), (3), (4), (5)",
        )
        .expect("setup");

    let results = engine
        .execute_sql(
            &session,
            "SELECT id FROM t_limit_null ORDER BY id LIMIT NULL",
        )
        .expect("SQL LIMIT NULL");
    match &results[0] {
        StatementResult::Query { rows, .. } => {
            assert_eq!(
                rows,
                &vec![
                    Row::new(vec![Value::Int(1)]),
                    Row::new(vec![Value::Int(2)]),
                    Row::new(vec![Value::Int(3)]),
                    Row::new(vec![Value::Int(4)]),
                    Row::new(vec![Value::Int(5)]),
                ]
            );
        }
        other => panic!("expected Query, got {other:?}"),
    }
}

#[test]
fn sql_offset_null_behaves_like_zero() {
    let mut limits = aiondb_config::LimitsConfig::default();
    limits.max_result_rows = 100;
    let engine = build_engine_with_limits(limits);
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE TABLE t_offset_null (id INT); \
             INSERT INTO t_offset_null VALUES (1), (2), (3), (4), (5)",
        )
        .expect("setup");

    let results = engine
        .execute_sql(
            &session,
            "SELECT id FROM t_offset_null ORDER BY id LIMIT 2 OFFSET NULL",
        )
        .expect("SQL OFFSET NULL");
    match &results[0] {
        StatementResult::Query { rows, .. } => {
            assert_eq!(
                rows,
                &vec![Row::new(vec![Value::Int(1)]), Row::new(vec![Value::Int(2)])]
            );
        }
        other => panic!("expected Query, got {other:?}"),
    }
}

// Regression: the scalar-int top-K fast path in projection_plans
// previously hardcoded DESC ordering, so on an `INT NOT NULL` column
// with `ORDER BY ... ASC LIMIT k` it returned the K *largest* rows
// (and degraded to O(N·K) Vec::insert shifts on monotone-ASC inputs).
// The non-int Value path always used `compare_sort_values(.., descending,
// ..)` so this only fired when the gate `!nullable && Int|BigInt` was
// satisfied. Cover both ASC and DESC + a small LIMIT against rows
// inserted in the *opposite* order so the bug-vs-fix divergence is
// observable.
#[test]
fn scalar_int_top_k_respects_asc_descending_on_not_null_column() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE TABLE t_topk_int (id INT NOT NULL); \
             INSERT INTO t_topk_int VALUES (5),(1),(4),(2),(7),(3),(6)",
        )
        .expect("setup");

    let asc_rows = query_rows(
        &engine,
        &session,
        "SELECT id FROM t_topk_int ORDER BY id ASC LIMIT 3",
    );
    assert_eq!(
        asc_rows
            .iter()
            .map(|row| row.values[0].clone())
            .collect::<Vec<_>>(),
        vec![Value::Int(1), Value::Int(2), Value::Int(3)],
        "ASC LIMIT 3 must return the three smallest ids",
    );

    let desc_rows = query_rows(
        &engine,
        &session,
        "SELECT id FROM t_topk_int ORDER BY id DESC LIMIT 3",
    );
    assert_eq!(
        desc_rows
            .iter()
            .map(|row| row.values[0].clone())
            .collect::<Vec<_>>(),
        vec![Value::Int(7), Value::Int(6), Value::Int(5)],
        "DESC LIMIT 3 must return the three largest ids",
    );
}

#[test]
fn eq_filter_ordered_limit_returns_descending_rows_for_dense_and_sparse_matches() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE TABLE limit_user_probe (
                 id INT PRIMARY KEY,
                 user_id INT NOT NULL,
                 title TEXT NOT NULL,
                 likes INT NOT NULL
             );
             CREATE INDEX limit_user_probe_user_idx ON limit_user_probe(user_id);",
        )
        .expect("create table and index");

    for start in (1..=2000).step_by(200) {
        let end = (start + 199).min(2000);
        let mut sql = String::from("INSERT INTO limit_user_probe VALUES ");
        for id in start..=end {
            if id > start {
                sql.push(',');
            }
            let user_id = ((id - 1) % 200) + 1;
            let likes = (id * 17) % 10_000;
            sql.push_str(&format!("({id}, {user_id}, 'title-{id}', {likes})"));
        }
        engine.execute_sql(&session, &sql).expect("seed rows");
    }

    let dense_rows = query_rows(
        &engine,
        &session,
        "SELECT id, title, likes
         FROM limit_user_probe
         WHERE user_id = 7
         ORDER BY id DESC
         LIMIT 5",
    );
    assert_eq!(
        dense_rows
            .iter()
            .map(|row| row.values[0].clone())
            .collect::<Vec<_>>(),
        vec![
            Value::Int(1807),
            Value::Int(1607),
            Value::Int(1407),
            Value::Int(1207),
            Value::Int(1007),
        ],
    );

    let sparse_rows = query_rows(
        &engine,
        &session,
        "SELECT id
         FROM limit_user_probe
         WHERE user_id = 199
         ORDER BY id DESC
         LIMIT 20",
    );
    assert_eq!(
        sparse_rows
            .iter()
            .map(|row| row.values[0].clone())
            .collect::<Vec<_>>(),
        vec![
            Value::Int(1999),
            Value::Int(1799),
            Value::Int(1599),
            Value::Int(1399),
            Value::Int(1199),
            Value::Int(999),
            Value::Int(799),
            Value::Int(599),
            Value::Int(399),
            Value::Int(199),
        ],
    );
}

#[test]
fn eq_filter_ordered_limit_uses_index_equality_access_path() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE TABLE limit_user_plan_probe (
                 id INT PRIMARY KEY,
                 user_id INT NOT NULL,
                 title TEXT NOT NULL,
                 likes INT NOT NULL
             );
             CREATE INDEX limit_user_plan_probe_user_idx ON limit_user_plan_probe(user_id);",
        )
        .expect("create table and index");

    let access_path = access_path_for_query(
        &engine,
        &session,
        "SELECT id, title, likes
         FROM limit_user_plan_probe
         WHERE user_id = 7
         ORDER BY id DESC
         LIMIT 20",
    );
    assert!(
        matches!(
            access_path,
            aiondb_plan::ScanAccessPath::IndexEq { .. }
                | aiondb_plan::ScanAccessPath::IndexEqComposite { .. }
        ),
        "unexpected access path for limit_user_order: {access_path:?}"
    );
}
