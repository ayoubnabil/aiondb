use super::*;
use aiondb_core::SqlState;

// ===================================================================
// CREATE ROLE
// ===================================================================

#[test]
fn create_role_basic() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let results = engine
        .execute_sql(&session, "CREATE ROLE testrole")
        .expect("create role");
    assert_eq!(
        results,
        vec![StatementResult::Command {
            tag: "CREATE ROLE".to_owned(),
            rows_affected: 0,
        }]
    );
}

#[test]
fn create_role_with_login() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let results = engine
        .execute_sql(&session, "CREATE ROLE testrole WITH LOGIN")
        .expect("create role with login");
    assert_eq!(
        results,
        vec![StatementResult::Command {
            tag: "CREATE ROLE".to_owned(),
            rows_affected: 0,
        }]
    );
}

#[test]
fn create_role_with_password() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let results = engine
        .execute_sql(&session, "CREATE ROLE testrole LOGIN PASSWORD 'secret'")
        .expect("create role with password");
    assert_eq!(
        results,
        vec![StatementResult::Command {
            tag: "CREATE ROLE".to_owned(),
            rows_affected: 0,
        }]
    );
}

#[test]
fn create_role_with_superuser() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let results = engine
        .execute_sql(&session, "CREATE ROLE admin SUPERUSER LOGIN")
        .expect("create role with superuser");
    assert_eq!(
        results,
        vec![StatementResult::Command {
            tag: "CREATE ROLE".to_owned(),
            rows_affected: 0,
        }]
    );
}

#[test]
fn create_role_case_insensitive() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");
    bootstrap_admin(&engine, &session);

    engine
        .execute_sql(&session, "CREATE ROLE TestRole")
        .expect("create role");

    // Creating a role with the same name in different case should fail.
    let err = engine
        .execute_sql(&session, "CREATE ROLE testrole")
        .expect_err("duplicate should fail");
    let msg = format!("{err}");
    assert!(
        msg.contains("already exists"),
        "unexpected error message: {msg}"
    );
}

// ===================================================================
// DROP ROLE
// ===================================================================

#[test]
fn drop_role_basic() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");
    bootstrap_admin(&engine, &session);

    engine
        .execute_sql(&session, "CREATE ROLE testrole")
        .expect("create role");

    let results = engine
        .execute_sql(&session, "DROP ROLE testrole")
        .expect("drop role");
    assert_eq!(
        results,
        vec![StatementResult::Command {
            tag: "DROP ROLE".to_owned(),
            rows_affected: 0,
        }]
    );
}

#[test]
fn drop_role_removes_privileges() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");
    bootstrap_admin(&engine, &session);

    engine
        .execute_sql(
            &session,
            "CREATE TABLE items (id INT NOT NULL); \
             CREATE ROLE testrole; \
             GRANT SELECT ON items TO testrole",
        )
        .expect("setup");

    engine
        .execute_sql(&session, "DROP ROLE testrole")
        .expect("drop role");

    // Granting to the dropped role should now fail.
    let err = engine
        .execute_sql(&session, "GRANT INSERT ON items TO testrole")
        .expect_err("grant to dropped role should fail");
    let msg = format!("{err}");
    assert!(
        msg.contains("does not exist"),
        "unexpected error message: {msg}"
    );
}

#[test]
fn drop_owned_revokes_grants_emitted_by_owner_role() {
    let catalog = Arc::new(aiondb_catalog_store::CatalogStore::default());
    let storage = Arc::new(aiondb_storage_engine::InMemoryStorage::new_without_wal());
    let engine = build_engine_with_store(catalog.clone(), storage);
    let (session, _) = engine.startup(startup_params()).expect("startup");
    bootstrap_admin(&engine, &session);

    engine
        .execute_sql(
            &session,
            "CREATE ROLE grantor LOGIN; CREATE ROLE grantee LOGIN;",
        )
        .expect("create roles");
    engine
        .execute_sql(
            &session,
            "SET SESSION AUTHORIZATION grantor; \
             CREATE TABLE msource (a int); \
             GRANT SELECT ON msource TO grantee; \
             SET SESSION AUTHORIZATION alice;",
        )
        .expect("grant select as grantor");

    engine
        .execute_sql(&session, "DROP OWNED BY grantor")
        .expect("drop owned");

    let grantee_privileges = catalog
        .get_privileges(TxnId::default(), "grantee")
        .expect("get privileges for grantee");
    assert!(
        !grantee_privileges.iter().any(|privilege| {
            matches!(
                &privilege.target,
                aiondb_catalog::PrivilegeTarget::Table(name)
                    if name.name.eq_ignore_ascii_case("msource")
            )
        }),
        "DROP OWNED BY grantor should revoke grants on msource, remaining privileges: {grantee_privileges:?}"
    );

    engine
        .execute_sql(&session, "DROP USER grantee")
        .expect("drop user grantee after drop owned");
}

#[test]
fn drop_owned_missing_role_fails_explicitly() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");
    bootstrap_admin(&engine, &session);

    let err = engine
        .execute_sql(&session, "DROP OWNED BY missing_drop_owned_role")
        .expect_err("DROP OWNED should fail explicitly for missing role");
    assert_eq!(err.sqlstate(), SqlState::UndefinedObject);
    assert!(
        err.report()
            .message
            .contains("role \"missing_drop_owned_role\" does not exist"),
        "unexpected message: {}",
        err.report().message
    );
}

#[test]
fn reassign_owned_missing_role_fails_explicitly() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");
    bootstrap_admin(&engine, &session);

    engine
        .execute_sql(&session, "CREATE ROLE existing_target LOGIN")
        .expect("create target role");

    let missing_source_err = engine
        .execute_sql(
            &session,
            "REASSIGN OWNED BY missing_source_role TO existing_target",
        )
        .expect_err("REASSIGN OWNED should fail for missing source role");
    assert_eq!(missing_source_err.sqlstate(), SqlState::UndefinedObject);
    assert!(
        missing_source_err
            .report()
            .message
            .contains("role \"missing_source_role\" does not exist"),
        "unexpected message: {}",
        missing_source_err.report().message
    );

    let missing_target_err = engine
        .execute_sql(
            &session,
            "REASSIGN OWNED BY existing_target TO missing_target_role",
        )
        .expect_err("REASSIGN OWNED should fail for missing target role");
    assert_eq!(missing_target_err.sqlstate(), SqlState::UndefinedObject);
    assert!(
        missing_target_err
            .report()
            .message
            .contains("role \"missing_target_role\" does not exist"),
        "unexpected message: {}",
        missing_target_err.report().message
    );
}

// ===================================================================
// GRANT
// ===================================================================

#[test]
fn grant_select_on_table() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");
    bootstrap_admin(&engine, &session);

    engine
        .execute_sql(
            &session,
            "CREATE TABLE items (id INT NOT NULL); \
             CREATE ROLE testrole",
        )
        .expect("setup");

    let results = engine
        .execute_sql(&session, "GRANT SELECT ON items TO testrole")
        .expect("grant select");
    assert_eq!(
        results,
        vec![StatementResult::Command {
            tag: "GRANT".to_owned(),
            rows_affected: 0,
        }]
    );
}

#[test]
fn grant_multiple_privileges() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");
    bootstrap_admin(&engine, &session);

    engine
        .execute_sql(
            &session,
            "CREATE TABLE items (id INT NOT NULL); \
             CREATE ROLE testrole",
        )
        .expect("setup");

    let results = engine
        .execute_sql(
            &session,
            "GRANT SELECT, INSERT, UPDATE, DELETE ON items TO testrole",
        )
        .expect("grant multiple");
    assert_eq!(
        results,
        vec![StatementResult::Command {
            tag: "GRANT".to_owned(),
            rows_affected: 0,
        }]
    );
}

#[test]
fn grant_all_privileges() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");
    bootstrap_admin(&engine, &session);

    engine
        .execute_sql(
            &session,
            "CREATE TABLE items (id INT NOT NULL); \
             CREATE ROLE testrole",
        )
        .expect("setup");

    let results = engine
        .execute_sql(&session, "GRANT ALL PRIVILEGES ON items TO testrole")
        .expect("grant all");
    assert_eq!(
        results,
        vec![StatementResult::Command {
            tag: "GRANT".to_owned(),
            rows_affected: 0,
        }]
    );
}

#[test]
fn grant_on_table_keyword() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");
    bootstrap_admin(&engine, &session);

    engine
        .execute_sql(
            &session,
            "CREATE TABLE items (id INT NOT NULL); \
             CREATE ROLE testrole",
        )
        .expect("setup");

    let results = engine
        .execute_sql(&session, "GRANT SELECT ON TABLE items TO testrole")
        .expect("grant with TABLE keyword");
    assert_eq!(
        results,
        vec![StatementResult::Command {
            tag: "GRANT".to_owned(),
            rows_affected: 0,
        }]
    );
}

#[test]
fn grant_on_schema() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");
    bootstrap_admin(&engine, &session);

    engine
        .execute_sql(&session, "CREATE ROLE testrole")
        .expect("create role");

    let results = engine
        .execute_sql(&session, "GRANT CREATE ON SCHEMA public TO testrole")
        .expect("grant on schema");
    assert_eq!(
        results,
        vec![StatementResult::Command {
            tag: "GRANT".to_owned(),
            rows_affected: 0,
        }]
    );
}

// ===================================================================
// REVOKE
// ===================================================================

#[test]
fn revoke_select_on_table() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");
    bootstrap_admin(&engine, &session);

    engine
        .execute_sql(
            &session,
            "CREATE TABLE items (id INT NOT NULL); \
             CREATE ROLE testrole; \
             GRANT SELECT ON items TO testrole",
        )
        .expect("setup");

    let results = engine
        .execute_sql(&session, "REVOKE SELECT ON items FROM testrole")
        .expect("revoke select");
    assert_eq!(
        results,
        vec![StatementResult::Command {
            tag: "REVOKE".to_owned(),
            rows_affected: 0,
        }]
    );
}

#[test]
fn revoke_all_privileges() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");
    bootstrap_admin(&engine, &session);

    engine
        .execute_sql(
            &session,
            "CREATE TABLE items (id INT NOT NULL); \
             CREATE ROLE testrole; \
             GRANT ALL ON items TO testrole",
        )
        .expect("setup");

    let results = engine
        .execute_sql(&session, "REVOKE ALL PRIVILEGES ON items FROM testrole")
        .expect("revoke all");
    assert_eq!(
        results,
        vec![StatementResult::Command {
            tag: "REVOKE".to_owned(),
            rows_affected: 0,
        }]
    );
}

#[test]
fn revoke_grant_option_for_object_privilege_rejected_explicitly() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");
    bootstrap_admin(&engine, &session);

    engine
        .execute_sql(
            &session,
            "CREATE TABLE items (id INT NOT NULL); \
             CREATE ROLE testrole; \
             GRANT SELECT ON items TO testrole",
        )
        .expect("setup");

    let err = engine
        .execute_sql(
            &session,
            "REVOKE GRANT OPTION FOR SELECT ON items FROM testrole",
        )
        .expect_err("unsupported object-privilege grant-option revoke must fail");
    assert_eq!(err.sqlstate(), aiondb_core::SqlState::FeatureNotSupported);
    let msg = format!("{err}");
    assert!(
        msg.contains("REVOKE GRANT OPTION FOR") || msg.contains("not supported"),
        "unexpected error message: {msg}"
    );
}

#[test]
fn revoke_admin_option_granted_by_mismatch_fails_explicitly() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");
    bootstrap_admin(&engine, &session);

    engine
        .execute_sql(
            &session,
            "CREATE ROLE role_target; \
             CREATE ROLE role_grantee; \
             CREATE ROLE grantor_a; \
             CREATE ROLE grantor_b; \
             GRANT role_target TO role_grantee WITH ADMIN OPTION GRANTED BY grantor_a",
        )
        .expect("setup");

    let err = engine
        .execute_sql(
            &session,
            "REVOKE ADMIN OPTION FOR role_target FROM role_grantee GRANTED BY grantor_b",
        )
        .expect_err("mismatched GRANTED BY must fail explicitly");
    assert_eq!(err.sqlstate(), aiondb_core::SqlState::InsufficientPrivilege);
    let msg = format!("{err}");
    assert!(
        msg.contains("has not been granted membership") || msg.contains("privilege"),
        "unexpected error message: {msg}"
    );
}

// ===================================================================
// Error cases
// ===================================================================

#[test]
fn error_create_role_duplicate() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");
    bootstrap_admin(&engine, &session);

    engine
        .execute_sql(&session, "CREATE ROLE testrole")
        .expect("create role");

    let err = engine
        .execute_sql(&session, "CREATE ROLE testrole")
        .expect_err("duplicate should fail");
    let msg = format!("{err}");
    assert!(
        msg.contains("already exists"),
        "unexpected error message: {msg}"
    );
}

#[test]
fn error_drop_role_not_found() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let err = engine
        .execute_sql(&session, "DROP ROLE nonexistent")
        .expect_err("should fail");
    let msg = format!("{err}");
    assert!(
        msg.contains("does not exist"),
        "unexpected error message: {msg}"
    );
}

#[test]
fn drop_role_if_exists_missing_succeeds_quietly() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let results = engine
        .execute_sql(&session, "DROP ROLE IF EXISTS nonexistent")
        .expect("drop role if exists");

    assert_eq!(results.len(), 1);
    match &results[0] {
        StatementResult::Command { tag, rows_affected } => {
            assert_eq!(tag, "DROP ROLE");
            assert_eq!(*rows_affected, 0);
        }
        other => panic!("expected command result, got {other:?}"),
    }
}

#[test]
fn error_grant_to_nonexistent_role() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(&session, "CREATE TABLE items (id INT NOT NULL)")
        .expect("create table");

    let err = engine
        .execute_sql(&session, "GRANT SELECT ON items TO nonexistent")
        .expect_err("should fail");
    let msg = format!("{err}");
    assert!(
        msg.contains("does not exist"),
        "unexpected error message: {msg}"
    );
}

#[test]
fn grant_and_revoke_accept_current_user_pseudo_role() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");
    bootstrap_admin(&engine, &session);

    engine
        .execute_sql(&session, "CREATE TABLE items (id INT NOT NULL)")
        .expect("create table");

    engine
        .execute_sql(&session, "GRANT SELECT ON items TO CURRENT_USER")
        .expect("grant to current_user");
    engine
        .execute_sql(&session, "REVOKE SELECT ON items FROM CURRENT_USER")
        .expect("revoke from current_user");
}

#[test]
fn error_revoke_from_nonexistent_role() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(&session, "CREATE TABLE items (id INT NOT NULL)")
        .expect("create table");

    let err = engine
        .execute_sql(&session, "REVOKE SELECT ON items FROM nonexistent")
        .expect_err("should fail");
    let msg = format!("{err}");
    assert!(
        msg.contains("does not exist"),
        "unexpected error message: {msg}"
    );
}

// ===================================================================
// Full lifecycle
// ===================================================================

#[test]
fn full_role_lifecycle() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");
    bootstrap_admin(&engine, &session);

    // Create a table and a role
    engine
        .execute_sql(
            &session,
            "CREATE TABLE documents (id INT NOT NULL, title TEXT); \
             CREATE ROLE editor LOGIN PASSWORD 'pass123'",
        )
        .expect("setup");

    // Grant privileges
    engine
        .execute_sql(
            &session,
            "GRANT SELECT, INSERT, UPDATE ON documents TO editor",
        )
        .expect("grant privileges");

    // Revoke one privilege
    engine
        .execute_sql(&session, "REVOKE UPDATE ON documents FROM editor")
        .expect("revoke update");

    // Grant on schema
    engine
        .execute_sql(&session, "GRANT CREATE ON SCHEMA public TO editor")
        .expect("grant on schema");

    // Drop the role (should clean up all privileges)
    engine
        .execute_sql(&session, "DROP ROLE editor")
        .expect("drop role");

    // Verify the role is gone
    let err = engine
        .execute_sql(&session, "DROP ROLE editor")
        .expect_err("double drop should fail");
    let msg = format!("{err}");
    assert!(
        msg.contains("does not exist"),
        "unexpected error message: {msg}"
    );
}

#[test]
fn grant_idempotent() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");
    bootstrap_admin(&engine, &session);

    engine
        .execute_sql(
            &session,
            "CREATE TABLE items (id INT NOT NULL); \
             CREATE ROLE testrole",
        )
        .expect("setup");

    // Granting the same privilege twice should succeed (idempotent).
    engine
        .execute_sql(&session, "GRANT SELECT ON items TO testrole")
        .expect("first grant");
    engine
        .execute_sql(&session, "GRANT SELECT ON items TO testrole")
        .expect("second grant should be idempotent");
}
