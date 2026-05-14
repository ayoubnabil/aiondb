//! Authorization denial tests for fine-grained statement access control.
//!
//! Verifies that the engine correctly rejects statements when the
//! authorizer denies the required action.

use std::sync::{
    atomic::{AtomicBool, AtomicUsize, Ordering},
    Arc,
};

use aiondb_core::{DbError, DbResult};

use super::*;

/// Authorizer that denies a specific set of actions and allows everything else.
struct DenyActionsAuthorizer {
    denied_actions: Vec<Action>,
}

impl DenyActionsAuthorizer {
    fn denying(actions: &[Action]) -> Self {
        Self {
            denied_actions: actions.to_vec(),
        }
    }
}

impl Authorizer for DenyActionsAuthorizer {
    fn authorize(
        &self,
        _identity: &AuthenticatedIdentity,
        request: &AccessRequest,
    ) -> DbResult<()> {
        if self.denied_actions.contains(&request.action) {
            Err(DbError::insufficient_privilege(format!(
                "action {:?} is denied by policy",
                request.action
            )))
        } else {
            Ok(())
        }
    }
}

struct CountingAuthorizer {
    calls: Arc<AtomicUsize>,
    noop: bool,
}

impl CountingAuthorizer {
    fn new(calls: Arc<AtomicUsize>, noop: bool) -> Self {
        Self { calls, noop }
    }
}

impl Authorizer for CountingAuthorizer {
    fn authorize(
        &self,
        _identity: &AuthenticatedIdentity,
        _request: &AccessRequest,
    ) -> DbResult<()> {
        self.calls.fetch_add(1, Ordering::Relaxed);
        Ok(())
    }

    fn is_noop(&self) -> bool {
        self.noop
    }
}

struct SwitchableDenyActionAuthorizer {
    deny: Arc<AtomicBool>,
    action: Action,
}

impl SwitchableDenyActionAuthorizer {
    fn new(deny: Arc<AtomicBool>, action: Action) -> Self {
        Self { deny, action }
    }
}

impl Authorizer for SwitchableDenyActionAuthorizer {
    fn authorize(
        &self,
        _identity: &AuthenticatedIdentity,
        request: &AccessRequest,
    ) -> DbResult<()> {
        if self.deny.load(Ordering::Relaxed) && request.action == self.action {
            Err(DbError::insufficient_privilege(format!(
                "action {:?} is denied by policy",
                request.action
            )))
        } else {
            Ok(())
        }
    }
}

fn build_engine_with_denied_actions(denied: &[Action]) -> Engine {
    let authorizer = Arc::new(DenyActionsAuthorizer::denying(denied));
    EngineBuilder::for_testing()
        .with_authorizer(authorizer)
        .build()
        .expect("engine should build")
}

fn assert_denied_result<T>(result: &DbResult<T>) {
    match result {
        Err(e) => {
            let msg = e.to_string();
            assert!(
                msg.contains("denied")
                    || msg.contains("privilege")
                    || msg.contains("authorization"),
                "expected authorization error, got: {msg}"
            );
        }
        Ok(_) => panic!("expected authorization error but query succeeded"),
    }
}

fn assert_denied(result: &DbResult<Vec<StatementResult>>) {
    assert_denied_result(result);
}

#[test]
fn noop_authorizer_fast_path_skips_connect_and_statement_authorization() {
    let calls = Arc::new(AtomicUsize::new(0));
    let engine = EngineBuilder::for_testing()
        .with_authorizer(Arc::new(CountingAuthorizer::new(calls.clone(), true)))
        .build()
        .expect("engine should build");
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(&session, "SELECT 1")
        .expect("select should succeed");

    assert_eq!(
        calls.load(Ordering::Relaxed),
        0,
        "noop authorizer should not be invoked on fast path"
    );
}

#[test]
fn non_noop_authorizer_still_runs_for_connect_and_statement_execution() {
    let calls = Arc::new(AtomicUsize::new(0));
    let engine = EngineBuilder::for_testing()
        .with_authorizer(Arc::new(CountingAuthorizer::new(calls.clone(), false)))
        .build()
        .expect("engine should build");
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(&session, "SELECT 1")
        .expect("select should succeed");

    assert!(
        calls.load(Ordering::Relaxed) >= 2,
        "non-noop authorizer should run for startup connect and statement execution"
    );
}

// ===================================================================
// Individual action denial tests
// ===================================================================

#[test]
fn select_denied_rejects_query() {
    let engine = build_engine_with_denied_actions(&[Action::Select]);
    let (session, _) = engine.startup(startup_params()).expect("startup");
    engine
        .execute_sql(&session, "CREATE TABLE t1 (id INT)")
        .expect("create should succeed");
    let result = engine.execute_sql(&session, "SELECT * FROM t1");
    assert_denied(&result);
}

#[test]
fn insert_denied_rejects_insert() {
    let engine = build_engine_with_denied_actions(&[Action::Insert]);
    let (session, _) = engine.startup(startup_params()).expect("startup");
    engine
        .execute_sql(&session, "CREATE TABLE t1 (id INT)")
        .expect("create should succeed");
    let result = engine.execute_sql(&session, "INSERT INTO t1 VALUES (1)");
    assert_denied(&result);
}

#[test]
fn update_denied_rejects_update() {
    let engine = build_engine_with_denied_actions(&[Action::Update]);
    let (session, _) = engine.startup(startup_params()).expect("startup");
    engine
        .execute_sql(&session, "CREATE TABLE t1 (id INT)")
        .expect("create should succeed");
    engine
        .execute_sql(&session, "INSERT INTO t1 VALUES (1)")
        .expect("insert should succeed");
    let result = engine.execute_sql(&session, "UPDATE t1 SET id = 2");
    assert_denied(&result);
}

#[test]
fn delete_denied_rejects_delete() {
    let engine = build_engine_with_denied_actions(&[Action::Delete]);
    let (session, _) = engine.startup(startup_params()).expect("startup");
    engine
        .execute_sql(&session, "CREATE TABLE t1 (id INT)")
        .expect("create should succeed");
    engine
        .execute_sql(&session, "INSERT INTO t1 VALUES (1)")
        .expect("insert should succeed");
    let result = engine.execute_sql(&session, "DELETE FROM t1");
    assert_denied(&result);
}

#[test]
fn create_denied_rejects_ddl() {
    let engine = build_engine_with_denied_actions(&[Action::Create]);
    let (session, _) = engine.startup(startup_params()).expect("startup");
    let result = engine.execute_sql(&session, "CREATE TABLE t1 (id INT)");
    assert_denied(&result);
}

#[test]
fn drop_denied_rejects_drop() {
    let engine = build_engine_with_denied_actions(&[Action::Drop]);
    let (session, _) = engine.startup(startup_params()).expect("startup");
    engine
        .execute_sql(&session, "CREATE TABLE t1 (id INT)")
        .expect("create should succeed");
    let result = engine.execute_sql(&session, "DROP TABLE t1");
    assert_denied(&result);
}

#[test]
fn create_denied_rejects_typed_compat_create() {
    let engine = build_engine_with_denied_actions(&[Action::Create]);
    let (session, _) = engine.startup(startup_params()).expect("startup");
    let result = engine.execute_sql(&session, "CREATE TYPE mood AS ENUM ('sad', 'ok')");
    assert_denied(&result);
}

#[test]
fn drop_denied_rejects_typed_compat_drop() {
    let engine = build_engine_with_denied_actions(&[Action::Drop]);
    let (session, _) = engine.startup(startup_params()).expect("startup");
    let result = engine.execute_sql(&session, "DROP PROCEDURE p()");
    assert_denied(&result);
}

#[test]
fn alter_denied_rejects_typed_compat_alter() {
    let engine = build_engine_with_denied_actions(&[Action::Alter]);
    let (session, _) = engine.startup(startup_params()).expect("startup");
    let result = engine.execute_sql(&session, "ALTER TYPE mood OWNER TO alice");
    assert_denied(&result);
}

#[test]
fn alter_denied_rejects_session_mutating_commands() {
    let engine = build_engine_with_denied_actions(&[Action::Alter]);
    let (session, _) = engine.startup(startup_params()).expect("startup");

    assert_denied(&engine.execute_sql(&session, "ALTER SYSTEM SET work_mem = '64MB'"));
    assert_denied(&engine.execute_sql(&session, "COMMENT ON ROLE aiondb IS 'owner'"));
    assert_denied(&engine.execute_sql(&session, "SECURITY LABEL ON ROLE aiondb IS 'owner'"));
    assert_denied(&engine.execute_sql(&session, "REASSIGN OWNED BY aiondb TO postgres"));
    assert_denied(&engine.execute_sql(&session, "SET work_mem = '64MB'"));
    assert_denied(&engine.execute_sql(&session, "RESET work_mem"));
    assert_denied(&engine.execute_sql(&session, "SET TRANSACTION READ ONLY"));
    assert_denied(&engine.execute_sql(
        &session,
        "SET SESSION CHARACTERISTICS AS TRANSACTION READ ONLY",
    ));
    assert_denied(&engine.execute_sql(&session, "DISCARD PLANS"));
}

#[test]
fn alter_denied_rejects_pseudo_role_grant_revoke() {
    let engine = build_engine_with_denied_actions(&[Action::Alter]);
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(&session, "CREATE TABLE pseudo_role_authz (id INT)")
        .expect("create should succeed");
    assert_denied(&engine.execute_sql(
        &session,
        "GRANT SELECT ON pseudo_role_authz TO CURRENT_USER",
    ));
    assert_denied(&engine.execute_sql(
        &session,
        "REVOKE SELECT ON pseudo_role_authz FROM CURRENT_USER",
    ));
}

#[test]
fn drop_denied_rejects_drop_owned() {
    let engine = build_engine_with_denied_actions(&[Action::Drop]);
    let (session, _) = engine.startup(startup_params()).expect("startup");
    let result = engine.execute_sql(&session, "DROP OWNED BY aiondb");
    assert_denied(&result);
}

#[test]
fn execute_denied_rejects_compat_execution_commands() {
    let engine = build_engine_with_denied_actions(&[Action::Execute]);
    let (session, _) = engine.startup(startup_params()).expect("startup");

    assert_denied(&engine.execute_sql(&session, "DO $$ BEGIN NULL; END $$"));
    assert_denied(&engine.execute_sql(&session, "LOAD 'plpgsql'"));
    assert_denied(&engine.execute_sql(&session, "PREPARE TRANSACTION 'gid-1'"));
    assert_denied(&engine.execute_sql(&session, "COMMIT PREPARED 'gid-1'"));
    assert_denied(&engine.execute_sql(&session, "ROLLBACK PREPARED 'gid-1'"));
}

#[test]
fn copy_from_requires_insert_and_copy_to_requires_select() {
    let insert_denied = build_engine_with_denied_actions(&[Action::Insert]);
    let (insert_session, _) = insert_denied.startup(startup_params()).expect("startup");
    insert_denied
        .execute_sql(&insert_session, "CREATE TABLE copy_authz_insert (id INT)")
        .expect("create should succeed");
    assert_denied(&insert_denied.execute_sql(&insert_session, "COPY copy_authz_insert FROM STDIN"));

    let select_denied = build_engine_with_denied_actions(&[Action::Select]);
    let (select_session, _) = select_denied.startup(startup_params()).expect("startup");
    select_denied
        .execute_sql(&select_session, "CREATE TABLE copy_authz_select (id INT)")
        .expect("create should succeed");
    assert_denied(&select_denied.execute_sql(&select_session, "COPY copy_authz_select TO STDOUT"));
}

#[test]
fn copy_query_uses_inner_statement_action() {
    let insert_denied = build_engine_with_denied_actions(&[Action::Insert]);
    let (insert_session, _) = insert_denied.startup(startup_params()).expect("startup");
    insert_denied
        .execute_sql(
            &insert_session,
            "CREATE TABLE copy_query_insert_authz (id INT)",
        )
        .expect("create should succeed");
    assert_denied(&insert_denied.execute_sql(
        &insert_session,
        "COPY (INSERT INTO copy_query_insert_authz VALUES (1) RETURNING id) TO STDOUT",
    ));

    let update_denied = build_engine_with_denied_actions(&[Action::Update]);
    let (update_session, _) = update_denied.startup(startup_params()).expect("startup");
    update_denied
        .execute_sql(
            &update_session,
            "CREATE TABLE copy_query_update_authz (id INT)",
        )
        .expect("create should succeed");
    update_denied
        .execute_sql(
            &update_session,
            "INSERT INTO copy_query_update_authz VALUES (1)",
        )
        .expect("insert should succeed");
    assert_denied(&update_denied.execute_sql(
        &update_session,
        "COPY (UPDATE copy_query_update_authz SET id = 2 RETURNING id) TO STDOUT",
    ));
}

#[test]
fn explain_analyze_requires_inner_statement_action() {
    let select_denied = build_engine_with_denied_actions(&[Action::Select]);
    let (select_session, _) = select_denied.startup(startup_params()).expect("startup");
    assert_denied(&select_denied.execute_sql(&select_session, "EXPLAIN SELECT 1"));
    assert_denied(&select_denied.execute_sql(&select_session, "EXPLAIN ANALYZE SELECT 1"));

    let insert_denied = build_engine_with_denied_actions(&[Action::Insert]);
    let (insert_session, _) = insert_denied.startup(startup_params()).expect("startup");
    insert_denied
        .execute_sql(&insert_session, "CREATE TABLE explain_authz (id INT)")
        .expect("create should succeed");
    assert_denied(&insert_denied.execute_sql(
        &insert_session,
        "EXPLAIN ANALYZE INSERT INTO explain_authz VALUES (1)",
    ));
}

#[test]
fn execute_denied_rejects_prepared_do_execute_shortcut() {
    let engine = build_engine_with_denied_actions(&[Action::Execute]);
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(&session, "PREPARE stmt AS DO $$ BEGIN NULL; END $$")
        .expect("PREPARE should only require session usage");
    assert_denied(&engine.execute_sql(&session, "EXECUTE stmt"));
}

#[test]
fn select_denied_rejects_prepared_portal_execute() {
    let engine = build_engine_with_denied_actions(&[Action::Select]);
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .prepare(&session, "stmt".to_owned(), "SELECT 1".to_owned())
        .expect("prepare should not execute statement");
    assert_denied_result(&engine.execute_prepared_statement_with_notices(
        &session,
        "stmt".to_owned(),
        Vec::new(),
        0,
    ));
}

#[test]
fn insert_denied_rejects_prepared_portal_execute() {
    let engine = build_engine_with_denied_actions(&[Action::Insert]);
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(&session, "CREATE TABLE prepared_authz (id INT)")
        .expect("create should succeed");
    engine
        .prepare(
            &session,
            "stmt".to_owned(),
            "INSERT INTO prepared_authz VALUES (1)".to_owned(),
        )
        .expect("prepare should not execute statement");
    assert_denied_result(&engine.execute_prepared_statement_with_notices(
        &session,
        "stmt".to_owned(),
        Vec::new(),
        0,
    ));
}

#[test]
fn execute_denied_rejects_prepared_portal_execute_statement_shortcut() {
    let engine = build_engine_with_denied_actions(&[Action::Execute]);
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(&session, "PREPARE inner_stmt AS DO $$ BEGIN NULL; END $$")
        .expect("SQL PREPARE should only register the inner statement");
    engine
        .prepare(
            &session,
            "outer_stmt".to_owned(),
            "EXECUTE inner_stmt".to_owned(),
        )
        .expect("wire PREPARE should only register EXECUTE");
    assert_denied_result(&engine.execute_prepared_statement_with_notices(
        &session,
        "outer_stmt".to_owned(),
        Vec::new(),
        0,
    ));
}

#[test]
fn select_denied_rejects_prepared_portal_explain_execute_shortcut() {
    let engine = build_engine_with_denied_actions(&[Action::Select]);
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(&session, "PREPARE inner_stmt AS SELECT 1")
        .expect("SQL PREPARE should only register the inner statement");
    engine
        .prepare(
            &session,
            "outer_stmt".to_owned(),
            "EXPLAIN EXECUTE inner_stmt".to_owned(),
        )
        .expect("wire PREPARE should only register EXPLAIN EXECUTE");
    assert_denied_result(&engine.execute_prepared_statement_with_notices(
        &session,
        "outer_stmt".to_owned(),
        Vec::new(),
        0,
    ));
}

#[test]
fn cached_portal_batch_rechecks_authorization() {
    let deny = Arc::new(AtomicBool::new(false));
    let engine = EngineBuilder::for_testing()
        .with_authorizer(Arc::new(SwitchableDenyActionAuthorizer::new(
            deny.clone(),
            Action::Insert,
        )))
        .build()
        .expect("engine should build");
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(&session, "CREATE TABLE cached_authz (id INT)")
        .expect("create should succeed before deny switch");
    engine
        .prepare(
            &session,
            "stmt".to_owned(),
            "INSERT INTO cached_authz VALUES (1), (2) RETURNING id".to_owned(),
        )
        .expect("prepare should not execute statement");
    engine
        .bind(&session, "portal".to_owned(), "stmt".to_owned(), Vec::new())
        .expect("bind should succeed");

    engine
        .execute_portal(&session, "portal", 1)
        .expect("first batch should execute before deny switch");
    deny.store(true, Ordering::Relaxed);
    assert_denied_result(&engine.execute_portal(&session, "portal", 1));
}

#[test]
fn alter_denied_rejects_set_tenant_after_tenant_exists() {
    let deny = Arc::new(AtomicBool::new(false));
    let engine = EngineBuilder::for_testing()
        .with_authorizer(Arc::new(SwitchableDenyActionAuthorizer::new(
            deny.clone(),
            Action::Alter,
        )))
        .build()
        .expect("engine should build");
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(&session, "CREATE TENANT authz_tenant")
        .expect("create tenant should succeed before deny switch");
    deny.store(true, Ordering::Relaxed);
    assert_denied(&engine.execute_sql(&session, "SET TENANT authz_tenant"));
}

#[test]
fn copy_from_data_rechecks_insert_authorization() {
    let deny = Arc::new(AtomicBool::new(false));
    let engine = EngineBuilder::for_testing()
        .with_authorizer(Arc::new(SwitchableDenyActionAuthorizer::new(
            deny.clone(),
            Action::Insert,
        )))
        .build()
        .expect("engine should build");
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(&session, "CREATE TABLE copy_authz_recheck (id INT)")
        .expect("create should succeed before deny switch");
    let result = engine
        .execute_sql(&session, "COPY copy_authz_recheck FROM STDIN")
        .expect("copy setup should succeed before deny switch");
    let table_id = match result.as_slice() {
        [StatementResult::CopyIn { table_id, .. }] => *table_id,
        other => panic!("expected CopyIn result, got {other:?}"),
    };

    deny.store(true, Ordering::Relaxed);
    assert_denied_result(&engine.execute_copy_from(&session, table_id, "1\n"));
}

// ===================================================================
// Positive test: allowed actions succeed
// ===================================================================

#[test]
fn allowed_actions_succeed_when_only_delete_denied() {
    let engine = build_engine_with_denied_actions(&[Action::Delete]);
    let (session, _) = engine.startup(startup_params()).expect("startup");
    engine
        .execute_sql(&session, "CREATE TABLE t1 (id INT)")
        .expect("create should succeed");
    engine
        .execute_sql(&session, "INSERT INTO t1 VALUES (1)")
        .expect("insert should succeed");
    engine
        .execute_sql(&session, "SELECT * FROM t1")
        .expect("select should succeed");
    engine
        .execute_sql(&session, "UPDATE t1 SET id = 2")
        .expect("update should succeed");
}
