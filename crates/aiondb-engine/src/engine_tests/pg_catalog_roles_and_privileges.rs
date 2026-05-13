use super::*;

#[test]
fn pg_auth_members_reflects_role_membership_and_admin_option() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");
    let admin_session = bootstrap_admin_session(&engine, &session);

    engine
        .execute_sql(&admin_session, "CREATE ROLE pam_role1")
        .expect("create role1");
    engine
        .execute_sql(&admin_session, "CREATE ROLE pam_role2")
        .expect("create role2");
    engine
        .execute_sql(&admin_session, "CREATE ROLE pam_role3")
        .expect("create role3");

    engine
        .execute_sql(
            &admin_session,
            "GRANT pam_role1 TO pam_role2 WITH ADMIN OPTION",
        )
        .expect("grant role1->role2 with admin");
    engine
        .execute_sql(&admin_session, "GRANT pam_role1 TO pam_role3")
        .expect("grant role1->role3");

    let rows = query_rows(
        &engine,
        &admin_session,
        "SELECT roleid::regrole::text, member::regrole::text, admin_option \
         FROM pg_auth_members \
         WHERE roleid = 'pam_role1'::regrole \
         ORDER BY member::regrole::text",
    );
    assert_eq!(rows.len(), 2);

    assert_eq!(text_col(&rows[0], 0), "pam_role1");
    assert_eq!(text_col(&rows[0], 1), "pam_role2");
    assert!(bool_col(&rows[0], 2));

    assert_eq!(text_col(&rows[1], 0), "pam_role1");
    assert_eq!(text_col(&rows[1], 1), "pam_role3");
    assert!(!bool_col(&rows[1], 2));
}

#[test]
fn pg_auth_members_uses_recorded_grantor_for_granted_by_membership() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");
    let admin_session = bootstrap_admin_session(&engine, &session);

    engine
        .execute_sql(&admin_session, "CREATE ROLE pam_grantor_role")
        .expect("create role");
    engine
        .execute_sql(&admin_session, "CREATE ROLE pam_grantor_manager")
        .expect("create manager");
    engine
        .execute_sql(&admin_session, "CREATE ROLE pam_grantor_member")
        .expect("create member");

    engine
        .execute_sql(
            &admin_session,
            "GRANT pam_grantor_role TO pam_grantor_manager WITH ADMIN OPTION",
        )
        .expect("grant manager with admin");
    engine
        .execute_sql(
            &admin_session,
            "GRANT pam_grantor_role TO pam_grantor_member GRANTED BY pam_grantor_manager",
        )
        .expect("grant member with explicit grantor");

    let rows = query_rows(
        &engine,
        &admin_session,
        "SELECT member::regrole::text, grantor::regrole::text \
         FROM pg_auth_members \
         WHERE roleid = 'pam_grantor_role'::regrole \
         ORDER BY member::regrole::text",
    );

    assert_eq!(rows.len(), 2);
    assert_eq!(text_col(&rows[1], 0), "pam_grantor_member");
    assert_eq!(text_col(&rows[1], 1), "pam_grantor_manager");
}

#[test]
fn has_function_privilege_reflects_execute_grants_and_pg_monitor_membership() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(&session, "CREATE ROLE alice SUPERUSER LOGIN")
        .expect("create alice role");
    engine
        .execute_sql(&session, "CREATE ROLE regress_log_memory")
        .expect("create role");
    engine
        .execute_sql(&session, "CREATE ROLE regress_slot_dir_funcs")
        .expect("create role");

    let before_direct_grant = query_rows(
        &engine,
        &session,
        "SELECT has_function_privilege('regress_log_memory', \
         'pg_log_backend_memory_contexts(integer)', 'EXECUTE')",
    );
    assert_eq!(before_direct_grant.len(), 1);
    assert!(!bool_col(&before_direct_grant[0], 0));

    engine
        .execute_sql(
            &session,
            "GRANT EXECUTE ON FUNCTION pg_log_backend_memory_contexts(integer) \
             TO regress_log_memory",
        )
        .expect("grant execute");

    let after_direct_grant = query_rows(
        &engine,
        &session,
        "SELECT has_function_privilege('regress_log_memory', \
         'pg_log_backend_memory_contexts(integer)', 'EXECUTE')",
    );
    assert_eq!(after_direct_grant.len(), 1);
    assert!(bool_col(&after_direct_grant[0], 0));

    let before_pg_monitor = query_rows(
        &engine,
        &session,
        "SELECT \
            has_function_privilege('regress_slot_dir_funcs', 'pg_ls_logicalsnapdir()', 'EXECUTE'), \
            has_function_privilege('regress_slot_dir_funcs', 'pg_ls_logicalmapdir()', 'EXECUTE'), \
            has_function_privilege('regress_slot_dir_funcs', 'pg_ls_replslotdir(text)', 'EXECUTE')",
    );
    assert_eq!(before_pg_monitor.len(), 1);
    assert!(!bool_col(&before_pg_monitor[0], 0));
    assert!(!bool_col(&before_pg_monitor[0], 1));
    assert!(!bool_col(&before_pg_monitor[0], 2));

    engine
        .execute_sql(&session, "GRANT pg_monitor TO regress_slot_dir_funcs")
        .expect("grant role membership");

    let after_pg_monitor = query_rows(
        &engine,
        &session,
        "SELECT \
            has_function_privilege('regress_slot_dir_funcs', 'pg_ls_logicalsnapdir()', 'EXECUTE'), \
            has_function_privilege('regress_slot_dir_funcs', 'pg_ls_logicalmapdir()', 'EXECUTE'), \
            has_function_privilege('regress_slot_dir_funcs', 'pg_ls_replslotdir(text)', 'EXECUTE')",
    );
    assert_eq!(after_pg_monitor.len(), 1);
    assert!(bool_col(&after_pg_monitor[0], 0));
    assert!(bool_col(&after_pg_monitor[0], 1));
    assert!(bool_col(&after_pg_monitor[0], 2));
}

#[test]
fn has_function_privilege_rejects_usage_grant_on_function() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(&session, "CREATE ROLE alice SUPERUSER LOGIN")
        .expect("create alice role");
    engine
        .execute_sql(&session, "CREATE ROLE regress_usage_only")
        .expect("create role");
    let err = engine
        .execute_sql(
            &session,
            "GRANT USAGE ON FUNCTION vector_top_k_ids(text,text,text,integer) TO regress_usage_only",
        )
        .expect_err("USAGE should be rejected for functions");
    assert_eq!(err.report().sqlstate, SqlState::InvalidParameterValue);
    assert!(
        err.report()
            .message
            .contains("invalid privilege type USAGE for function"),
        "unexpected error: {err}"
    );
}

#[test]
fn has_function_privilege_reflects_public_execute_grants() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(&session, "CREATE ROLE alice SUPERUSER LOGIN")
        .expect("create alice role");
    engine
        .execute_sql(&session, "CREATE ROLE regress_public_exec")
        .expect("create role");
    engine
        .execute_sql(
            &session,
            "CREATE FUNCTION acl_public_exec(x INT) RETURNS INT AS 'x' LANGUAGE sql; \
             GRANT EXECUTE ON FUNCTION acl_public_exec(integer) TO PUBLIC",
        )
        .expect("setup public function grant");

    let rows = query_rows(
        &engine,
        &session,
        "SELECT has_function_privilege('regress_public_exec', \
         'acl_public_exec(integer)', 'EXECUTE')",
    );
    assert_eq!(rows.len(), 1);
    assert!(bool_col(&rows[0], 0));
}

#[test]
fn reset_role_clears_session_role_override() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(&session, "SET ROLE regress_log_memory")
        .expect("SET ROLE should set the role session variable");
    engine
        .execute_sql(&session, "RESET ROLE")
        .expect("RESET ROLE should clear the role session variable");

    let rows = query_rows(&engine, &session, "SHOW role");
    assert_eq!(text_col(&rows[0], 0), "none");
}

#[test]
fn set_session_authorization_sets_role_context() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");
    let admin_session = bootstrap_admin_session(&engine, &session);

    engine
        .execute_sql(&admin_session, "CREATE ROLE regress_log_memory LOGIN")
        .expect("create role");

    let denied = engine
        .execute_sql(&session, "SET SESSION AUTHORIZATION regress_log_memory")
        .expect_err("unmapped session should not set session authorization when roles exist");
    assert_eq!(denied.report().sqlstate, SqlState::InsufficientPrivilege);

    engine
        .execute_sql(
            &admin_session,
            "SET SESSION AUTHORIZATION regress_log_memory",
        )
        .expect("set session authorization");
    let rows = query_rows(&engine, &admin_session, "SHOW role");
    assert_eq!(text_col(&rows[0], 0), "regress_log_memory");

    engine
        .execute_sql(&admin_session, "RESET SESSION AUTHORIZATION")
        .expect("reset session authorization");
    let rows = query_rows(&engine, &admin_session, "SHOW role");
    assert_eq!(text_col(&rows[0], 0), "none");
}

#[test]
fn set_session_authorization_unknown_role_reports_undefined_object() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");
    let admin_session = bootstrap_admin_session(&engine, &session);

    engine
        .execute_sql(&admin_session, "CREATE ROLE existing_role LOGIN")
        .expect("create role to activate role system");

    let error = engine
        .execute_sql(&admin_session, "SET SESSION AUTHORIZATION no_such_role")
        .expect_err("unknown session authorization role should fail");
    assert_eq!(error.report().sqlstate, SqlState::UndefinedObject);
    assert!(error.report().message.contains("does not exist"));
}

#[test]
fn describe_set_session_authorization_unknown_role_reports_undefined_object() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");
    let admin_session = bootstrap_admin_session(&engine, &session);

    engine
        .execute_sql(&admin_session, "CREATE ROLE existing_role LOGIN")
        .expect("create role to activate role system");

    engine
        .prepare(
            &admin_session,
            "set_session_auth_missing_role".to_owned(),
            "SET SESSION AUTHORIZATION no_such_role".to_owned(),
        )
        .expect("prepare set session authorization should succeed before describe");

    let error = engine
        .describe_statement(&admin_session, "set_session_auth_missing_role")
        .expect_err("describe unknown session authorization role should fail");
    assert_eq!(error.report().sqlstate, SqlState::UndefinedObject);
    assert!(error.report().message.contains("does not exist"));
}

#[test]
fn set_role_requires_membership_when_role_system_is_active() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");
    let admin_session = bootstrap_admin_session(&engine, &session);

    engine
        .execute_sql(&admin_session, "CREATE ROLE app_user NOINHERIT LOGIN")
        .expect("create app_user role");
    engine
        .execute_sql(&admin_session, "CREATE ROLE target_role LOGIN")
        .expect("create target role");

    let denied = engine
        .execute_sql(&session, "SET ROLE target_role")
        .expect_err("unmapped session should not set role when roles exist");
    assert_eq!(denied.report().sqlstate, SqlState::InsufficientPrivilege);

    let mut app_user_params = startup_params();
    app_user_params.credential = Credential::Anonymous {
        user: "app_user".to_owned(),
    };
    let (app_user_session, _) = engine.startup(app_user_params).expect("app_user startup");

    let err = engine
        .execute_sql(&app_user_session, "SET ROLE target_role")
        .expect_err("set role without membership should fail");
    assert_eq!(err.report().sqlstate, SqlState::InsufficientPrivilege);

    engine
        .execute_sql(&admin_session, "GRANT target_role TO app_user")
        .expect("grant role membership");

    engine
        .execute_sql(&app_user_session, "SET ROLE target_role")
        .expect("set role with membership should succeed");
    let rows = query_rows(&engine, &app_user_session, "SHOW role");
    assert_eq!(text_col(&rows[0], 0), "target_role");
}

#[test]
fn set_role_unknown_role_reports_undefined_object() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");
    let admin_session = bootstrap_admin_session(&engine, &session);

    engine
        .execute_sql(&admin_session, "CREATE ROLE app_user LOGIN")
        .expect("create role to activate role system");

    let error = engine
        .execute_sql(&admin_session, "SET ROLE no_such_role")
        .expect_err("unknown role target should fail");
    assert_eq!(error.report().sqlstate, SqlState::UndefinedObject);
    assert!(error.report().message.contains("does not exist"));
}

#[test]
fn describe_set_role_unknown_role_reports_undefined_object() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");
    let admin_session = bootstrap_admin_session(&engine, &session);

    engine
        .execute_sql(&admin_session, "CREATE ROLE app_user LOGIN")
        .expect("create role to activate role system");

    engine
        .prepare(
            &admin_session,
            "set_role_missing_role".to_owned(),
            "SET ROLE no_such_role".to_owned(),
        )
        .expect("prepare set role should succeed before describe");

    let error = engine
        .describe_statement(&admin_session, "set_role_missing_role")
        .expect_err("describe unknown role target should fail");
    assert_eq!(error.report().sqlstate, SqlState::UndefinedObject);
    assert!(error.report().message.contains("does not exist"));
}

#[test]
fn describe_set_role_without_membership_reports_insufficient_privilege() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");
    let admin_session = bootstrap_admin_session(&engine, &session);

    engine
        .execute_sql(&admin_session, "CREATE ROLE app_user LOGIN")
        .expect("create app_user role");
    engine
        .execute_sql(&admin_session, "CREATE ROLE target_role LOGIN")
        .expect("create target role");

    let mut app_user_params = startup_params();
    app_user_params.credential = Credential::Anonymous {
        user: "app_user".to_owned(),
    };
    let (app_user_session, _) = engine.startup(app_user_params).expect("app_user startup");

    engine
        .prepare(
            &app_user_session,
            "set_role_without_membership".to_owned(),
            "SET ROLE target_role".to_owned(),
        )
        .expect("prepare set role should succeed before describe");

    let error = engine
        .describe_statement(&app_user_session, "set_role_without_membership")
        .expect_err("describe set role without membership should fail");
    assert_eq!(error.report().sqlstate, SqlState::InsufficientPrivilege);
}

#[test]
fn set_role_accepts_special_targets_and_inherited_membership() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");
    let admin_session = bootstrap_admin_session(&engine, &session);

    engine
        .execute_sql(&admin_session, "CREATE ROLE app_user LOGIN")
        .expect("create app_user role");
    engine
        .execute_sql(&admin_session, "CREATE ROLE parent_role")
        .expect("create parent_role role");
    engine
        .execute_sql(&admin_session, "CREATE ROLE child_role")
        .expect("create child_role role");

    engine
        .execute_sql(&admin_session, "GRANT parent_role TO app_user")
        .expect("grant parent role to app_user");
    engine
        .execute_sql(&admin_session, "GRANT child_role TO parent_role")
        .expect("grant child role to parent role");

    let mut app_user_params = startup_params();
    app_user_params.credential = Credential::Anonymous {
        user: "app_user".to_owned(),
    };
    let (app_user_session, _) = engine.startup(app_user_params).expect("app_user startup");

    engine
        .execute_sql(&app_user_session, "SET ROLE child_role")
        .expect("set role should follow transitive role membership");
    let rows = query_rows(&engine, &app_user_session, "SHOW role");
    assert_eq!(text_col(&rows[0], 0), "child_role");

    engine
        .execute_sql(&app_user_session, "SET ROLE CURRENT_USER")
        .expect("current_user should resolve to active role");
    let rows = query_rows(&engine, &app_user_session, "SHOW role");
    assert_eq!(text_col(&rows[0], 0), "child_role");

    engine
        .execute_sql(&app_user_session, "SET ROLE SESSION_USER")
        .expect("session_user should resolve to login role");
    let rows = query_rows(&engine, &app_user_session, "SHOW role");
    assert_eq!(text_col(&rows[0], 0), "app_user");

    engine
        .execute_sql(&app_user_session, "SET ROLE NONE")
        .expect("none should clear effective role override");
    let rows = query_rows(&engine, &app_user_session, "SHOW role");
    assert_eq!(text_col(&rows[0], 0), "none");
}

#[test]
fn set_role_updates_current_user_but_preserves_session_user() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");
    let admin_session = bootstrap_admin_session(&engine, &session);

    engine
        .execute_sql(&admin_session, "CREATE ROLE app_user LOGIN")
        .expect("create app_user role");
    engine
        .execute_sql(&admin_session, "CREATE ROLE target_role")
        .expect("create target role");
    engine
        .execute_sql(&admin_session, "GRANT target_role TO app_user")
        .expect("grant target_role");

    let mut app_user_params = startup_params();
    app_user_params.credential = Credential::Anonymous {
        user: "app_user".to_owned(),
    };
    let (app_user_session, _) = engine.startup(app_user_params).expect("app_user startup");

    let rows = query_rows(
        &engine,
        &app_user_session,
        "SELECT current_user, session_user",
    );
    assert_eq!(text_col(&rows[0], 0), "app_user");
    assert_eq!(text_col(&rows[0], 1), "app_user");

    engine
        .execute_sql(&app_user_session, "SET ROLE target_role")
        .expect("set role");
    let rows = query_rows(
        &engine,
        &app_user_session,
        "SELECT current_user, session_user",
    );
    assert_eq!(text_col(&rows[0], 0), "target_role");
    assert_eq!(text_col(&rows[0], 1), "app_user");
}

#[test]
fn set_local_role_updates_current_user_temporarily() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");
    let admin_session = bootstrap_admin_session(&engine, &session);

    engine
        .execute_sql(&admin_session, "CREATE ROLE app_user LOGIN")
        .expect("create app_user role");
    engine
        .execute_sql(&admin_session, "CREATE ROLE target_role")
        .expect("create target role");
    engine
        .execute_sql(&admin_session, "GRANT target_role TO app_user")
        .expect("grant target_role");

    let mut app_user_params = startup_params();
    app_user_params.credential = Credential::Anonymous {
        user: "app_user".to_owned(),
    };
    let (app_user_session, _) = engine.startup(app_user_params).expect("app_user startup");

    engine
        .execute_sql(&app_user_session, "BEGIN")
        .expect("begin");
    engine
        .execute_sql(&app_user_session, "SET LOCAL ROLE target_role")
        .expect("set local role");

    let rows = query_rows(
        &engine,
        &app_user_session,
        "SELECT current_user, session_user",
    );
    assert_eq!(text_col(&rows[0], 0), "target_role");
    assert_eq!(text_col(&rows[0], 1), "app_user");

    engine
        .execute_sql(&app_user_session, "COMMIT")
        .expect("commit");

    let rows = query_rows(
        &engine,
        &app_user_session,
        "SELECT current_user, session_user",
    );
    assert_eq!(text_col(&rows[0], 0), "app_user");
    assert_eq!(text_col(&rows[0], 1), "app_user");
}

#[test]
fn set_session_authorization_updates_current_and_session_user() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");
    let admin_session = bootstrap_admin_session(&engine, &session);

    engine
        .execute_sql(&admin_session, "CREATE ROLE app_user LOGIN")
        .expect("create app_user role");

    let denied = engine
        .execute_sql(&session, "SET SESSION AUTHORIZATION app_user")
        .expect_err("unmapped session should not set session authorization when roles exist");
    assert_eq!(denied.report().sqlstate, SqlState::InsufficientPrivilege);

    engine
        .execute_sql(&admin_session, "SET SESSION AUTHORIZATION app_user")
        .expect("set session authorization");
    let rows = query_rows(&engine, &admin_session, "SELECT current_user, session_user");
    assert_eq!(text_col(&rows[0], 0), "app_user");
    assert_eq!(text_col(&rows[0], 1), "app_user");
}

#[test]
fn set_local_session_authorization_updates_current_and_session_user_temporarily() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");
    let admin_session = bootstrap_admin_session(&engine, &session);

    engine
        .execute_sql(&admin_session, "CREATE ROLE app_user LOGIN")
        .expect("create app_user role");

    engine.execute_sql(&admin_session, "BEGIN").expect("begin");
    engine
        .execute_sql(&admin_session, "SET LOCAL SESSION AUTHORIZATION app_user")
        .expect("set local session authorization");

    let rows = query_rows(&engine, &admin_session, "SELECT current_user, session_user");
    assert_eq!(text_col(&rows[0], 0), "app_user");
    assert_eq!(text_col(&rows[0], 1), "app_user");

    engine
        .execute_sql(&admin_session, "COMMIT")
        .expect("commit");

    let rows = query_rows(&engine, &admin_session, "SELECT current_user, session_user");
    assert_eq!(text_col(&rows[0], 0), "bootstrap_admin");
    assert_eq!(text_col(&rows[0], 1), "bootstrap_admin");
}

#[test]
fn describe_set_session_authorization_without_superuser_reports_insufficient_privilege() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");
    let admin_session = bootstrap_admin_session(&engine, &session);

    engine
        .execute_sql(&admin_session, "CREATE ROLE app_user LOGIN")
        .expect("create app_user role");

    engine
        .prepare(
            &session,
            "set_session_auth_without_superuser".to_owned(),
            "SET SESSION AUTHORIZATION app_user".to_owned(),
        )
        .expect("prepare set session authorization should succeed before describe");

    let error = engine
        .describe_statement(&session, "set_session_auth_without_superuser")
        .expect_err("describe set session authorization without superuser should fail");
    assert_eq!(error.report().sqlstate, SqlState::InsufficientPrivilege);
}

#[test]
fn set_role_applies_acl_as_effective_role() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(&session, "CREATE TABLE secure_items (id INT)")
        .expect("create table");
    engine
        .execute_sql(&session, "INSERT INTO secure_items VALUES (1)")
        .expect("insert row");

    let admin_session = bootstrap_admin_session(&engine, &session);

    engine
        .execute_sql(&admin_session, "CREATE ROLE app_user LOGIN")
        .expect("create app_user role");
    engine
        .execute_sql(&admin_session, "CREATE ROLE reader_role")
        .expect("create reader_role");
    engine
        .execute_sql(
            &admin_session,
            "GRANT SELECT ON TABLE public.secure_items TO reader_role",
        )
        .expect("grant select to reader_role");
    engine
        .execute_sql(&admin_session, "GRANT reader_role TO app_user")
        .expect("grant role membership");

    let mut app_user_params = startup_params();
    app_user_params.credential = Credential::Anonymous {
        user: "app_user".to_owned(),
    };
    let (app_user_session, _) = engine.startup(app_user_params).expect("app_user startup");

    let err = engine
        .execute_sql(&app_user_session, "SELECT * FROM secure_items")
        .expect_err("app_user should not inherit reader_role privileges before SET ROLE");
    assert_eq!(err.report().sqlstate, SqlState::InsufficientPrivilege);

    engine
        .execute_sql(&app_user_session, "SET ROLE reader_role")
        .expect("set role");
    let rows = query_rows(&engine, &app_user_session, "SELECT * FROM secure_items");
    assert_eq!(rows.len(), 1);
    assert_eq!(int_col(&rows[0], 0), 1);
}

#[test]
fn derived_pg_tablespace_databases_join_no_longer_errors_on_internal_alias_names() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let rows = query_rows(
        &engine,
        &session,
        "SELECT count(*) > 0 \
         FROM (SELECT pg_tablespace_databases(oid) AS pts FROM pg_tablespace \
               WHERE spcname = 'pg_default') pts \
         JOIN pg_database db ON pts.pts = db.oid",
    );
    assert_eq!(rows.len(), 1);
    assert!(matches!(rows[0].values[0], Value::Boolean(_)));
}

#[test]
fn pg_function_privilege_helpers_follow_grants_and_pg_monitor_membership() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(&session, "CREATE ROLE alice SUPERUSER LOGIN")
        .expect("create alice role");
    engine
        .execute_sql(&session, "CREATE ROLE regress_log_memory")
        .expect("create role");
    let rows = query_rows(
        &engine,
        &session,
        "SELECT has_function_privilege('regress_log_memory', 'pg_log_backend_memory_contexts(integer)', 'EXECUTE')",
    );
    assert!(!bool_col(&rows[0], 0));

    engine
        .execute_sql(
            &session,
            "GRANT EXECUTE ON FUNCTION pg_log_backend_memory_contexts(integer) TO regress_log_memory",
        )
        .expect("grant execute");
    let rows = query_rows(
        &engine,
        &session,
        "SELECT has_function_privilege('regress_log_memory', 'pg_log_backend_memory_contexts(integer)', 'EXECUTE')",
    );
    assert!(bool_col(&rows[0], 0));

    engine
        .execute_sql(&session, "CREATE ROLE regress_slot_dir_funcs")
        .expect("create monitor target role");
    let rows = query_rows(
        &engine,
        &session,
        "SELECT has_function_privilege('regress_slot_dir_funcs', 'pg_ls_logicalsnapdir()', 'EXECUTE')",
    );
    assert!(!bool_col(&rows[0], 0));

    engine
        .execute_sql(&session, "GRANT pg_monitor TO regress_slot_dir_funcs")
        .expect("grant pg_monitor");
    let rows = query_rows(
        &engine,
        &session,
        "SELECT has_function_privilege('regress_slot_dir_funcs', 'pg_ls_logicalsnapdir()', 'EXECUTE')",
    );
    assert!(bool_col(&rows[0], 0));
}

#[test]
fn pg_log_backend_memory_contexts_helpers_are_usable() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let rows = query_rows(
        &engine,
        &session,
        "SELECT pg_log_backend_memory_contexts(pg_backend_pid())",
    );
    assert!(bool_col(&rows[0], 0));

    let rows = query_rows(
        &engine,
        &session,
        "SELECT pg_log_backend_memory_contexts(pid) FROM pg_stat_activity WHERE backend_type = 'checkpointer'",
    );
    assert_eq!(rows.len(), 1);
    assert!(bool_col(&rows[0], 0));
}

#[test]
fn pg_ls_dir_behaves_like_a_text_srf() {
    let data_dir = unique_temp_dir("pg-ls-dir");
    fs::create_dir_all(&data_dir).expect("create data dir");
    fs::write(data_dir.join("entry.txt"), b"ok").expect("write data dir entry");

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
            "GRANT EXECUTE ON FUNCTION pg_ls_dir(text, bool, bool) TO alice",
        )
        .expect("grant pg_ls_dir execute");

    let rows = query_rows(
        &engine,
        &session,
        "select count(*) >= 0 from pg_ls_dir('.', false, false)",
    );
    assert!(bool_col(&rows[0], 0));

    let rows = query_rows(
        &engine,
        &session,
        "select pg_ls_dir('does not exist', true, false)",
    );
    assert!(rows.is_empty());

    let _ = fs::remove_dir_all(&data_dir);
}

#[test]
fn pg_ls_archive_statusdir_is_usable_in_from() {
    let data_dir = unique_temp_dir("pg-ls-archive-statusdir");
    fs::create_dir_all(data_dir.join("pg_wal").join("archive_status"))
        .expect("create archive status dir");

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
            "GRANT EXECUTE ON FUNCTION pg_ls_archive_statusdir() TO alice",
        )
        .expect("grant pg_ls_archive_statusdir execute");

    let rows = query_rows(
        &engine,
        &session,
        "select count(*) >= 0 from pg_ls_archive_statusdir()",
    );
    assert!(bool_col(&rows[0], 0));

    let _ = fs::remove_dir_all(&data_dir);
}

#[test]
fn pg_ls_internal_helpers_read_from_configured_data_dir() {
    let data_dir = unique_temp_dir("pg-ls-dirs");
    fs::create_dir_all(data_dir.join("log")).expect("create log dir");
    fs::create_dir_all(data_dir.join("pg_wal").join("archive_status"))
        .expect("create archive status dir");
    fs::create_dir_all(data_dir.join("base").join("pgsql_tmp")).expect("create tmp dir");
    fs::write(data_dir.join("log").join("server.log"), b"log").expect("write log file");
    fs::write(
        data_dir
            .join("pg_wal")
            .join("archive_status")
            .join("000000010000000000000001.ready"),
        b"ready",
    )
    .expect("write archive marker");
    fs::write(
        data_dir
            .join("base")
            .join("pgsql_tmp")
            .join("pgsql_tmp123.0"),
        b"tmp",
    )
    .expect("write tmp file");

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
            "GRANT EXECUTE ON FUNCTION pg_ls_logdir() TO alice;
             GRANT EXECUTE ON FUNCTION pg_ls_archive_statusdir() TO alice;
             GRANT EXECUTE ON FUNCTION pg_ls_tmpdir() TO alice;",
        )
        .expect("grant pg_ls_*dir execute");

    let log_rows = query_rows(&engine, &session, "SELECT * FROM pg_ls_logdir() ORDER BY 1");
    assert_eq!(log_rows.len(), 1);
    assert_eq!(text_col(&log_rows[0], 0), "server.log");

    let archive_rows = query_rows(
        &engine,
        &session,
        "SELECT * FROM pg_ls_archive_statusdir() ORDER BY 1",
    );
    assert_eq!(archive_rows.len(), 1);
    assert_eq!(
        text_col(&archive_rows[0], 0),
        "000000010000000000000001.ready"
    );

    let tmp_rows = query_rows(&engine, &session, "SELECT * FROM pg_ls_tmpdir() ORDER BY 1");
    assert_eq!(tmp_rows.len(), 1);
    assert_eq!(text_col(&tmp_rows[0], 0), "pgsql_tmp123.0");

    let _ = fs::remove_dir_all(&data_dir);
}

#[test]
fn set_role_executes_for_pg_compatibility() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(&session, "SET ROLE regress_log_memory")
        .expect("set role should succeed");
    let rows = query_rows(&engine, &session, "SHOW role");
    assert_eq!(text_col(&rows[0], 0), "regress_log_memory");
}
