#![allow(clippy::pedantic)]

use super::*;

mod cypher_and_hybrid;

#[allow(dead_code)]
fn text_col(row: &Row, idx: usize) -> &str {
    match &row.values[idx] {
        aiondb_core::Value::Text(s) => s.as_str(),
        other => panic!("expected text value, got {other:?}"),
    }
}

#[allow(dead_code)]
fn int_col(row: &Row, idx: usize) -> i32 {
    match &row.values[idx] {
        aiondb_core::Value::Int(v) => *v,
        aiondb_core::Value::BigInt(v) => *v as i32,
        other => panic!("expected integer value, got {other:?}"),
    }
}

/// Helper: create startup params for a given user name.
fn startup_as(user: &str) -> StartupParams {
    StartupParams {
        database: "default".to_owned(),
        application_name: Some("test".to_owned()),
        options: BTreeMap::new(),
        credential: Credential::Anonymous {
            user: user.to_owned(),
        },
        transport: TransportInfo::in_process(),
    }
}

// ===================================================================
// SELECT denied without GRANT
// ===================================================================

#[test]
fn select_denied_without_grant() {
    let engine = EngineBuilder::for_testing().build().unwrap();

    // Admin session (alice) -- not a catalog role, so no ACL.
    let (admin, _) = engine.startup(startup_params()).expect("admin startup");
    engine
        .execute_sql(
            &admin,
            "CREATE TABLE items (id INT NOT NULL); \
             CREATE ROLE reader LOGIN",
        )
        .expect("setup");

    // Session as 'reader' -- a catalog role, ACL enforced.
    let (reader_session, _) = engine
        .startup(startup_as("reader"))
        .expect("reader startup");

    let err = engine
        .execute_sql(&reader_session, "SELECT * FROM items")
        .expect_err("SELECT should be denied");
    let msg = format!("{err}");
    assert!(msg.contains("permission denied"), "unexpected error: {msg}");
}

// ===================================================================
// SELECT allowed after GRANT SELECT
// ===================================================================

#[test]
fn select_allowed_after_grant() {
    let engine = EngineBuilder::for_testing().build().unwrap();

    let (admin, _) = engine.startup(startup_params()).expect("admin startup");
    engine
        .execute_sql(
            &admin,
            "CREATE ROLE alice SUPERUSER LOGIN; \
             CREATE TABLE items (id INT NOT NULL); \
             CREATE ROLE reader LOGIN; \
             GRANT SELECT ON items TO reader",
        )
        .expect("setup");

    let (reader_session, _) = engine
        .startup(startup_as("reader"))
        .expect("reader startup");
    let results = engine
        .execute_sql(&reader_session, "SELECT * FROM items")
        .expect("SELECT should succeed");
    assert_eq!(results.len(), 1);
}

// ===================================================================
// INSERT denied without GRANT
// ===================================================================

#[test]
fn insert_denied_without_grant() {
    let engine = EngineBuilder::for_testing().build().unwrap();

    let (admin, _) = engine.startup(startup_params()).expect("admin startup");
    engine
        .execute_sql(
            &admin,
            "CREATE TABLE items (id INT NOT NULL); \
             CREATE ROLE writer LOGIN",
        )
        .expect("setup");

    let (writer_session, _) = engine
        .startup(startup_as("writer"))
        .expect("writer startup");

    let err = engine
        .execute_sql(&writer_session, "INSERT INTO items (id) VALUES (1)")
        .expect_err("INSERT should be denied");
    let msg = format!("{err}");
    assert!(msg.contains("permission denied"), "unexpected error: {msg}");
}

// ===================================================================
// GRANT ALL allows everything
// ===================================================================

#[test]
fn grant_all_allows_everything() {
    let engine = EngineBuilder::for_testing().build().unwrap();

    let (admin, _) = engine.startup(startup_params()).expect("admin startup");
    engine
        .execute_sql(
            &admin,
            "CREATE ROLE alice SUPERUSER LOGIN; \
             CREATE TABLE items (id INT NOT NULL); \
             CREATE ROLE poweruser LOGIN; \
             GRANT ALL PRIVILEGES ON items TO poweruser",
        )
        .expect("setup");

    let (pu_session, _) = engine
        .startup(startup_as("poweruser"))
        .expect("poweruser startup");

    // SELECT
    engine
        .execute_sql(&pu_session, "SELECT * FROM items")
        .expect("SELECT should succeed");

    // INSERT
    engine
        .execute_sql(&pu_session, "INSERT INTO items (id) VALUES (1)")
        .expect("INSERT should succeed");

    // UPDATE
    engine
        .execute_sql(&pu_session, "UPDATE items SET id = 2 WHERE id = 1")
        .expect("UPDATE should succeed");

    // DELETE
    engine
        .execute_sql(&pu_session, "DELETE FROM items WHERE id = 2")
        .expect("DELETE should succeed");
}

// ===================================================================
// Superuser bypasses ACL
// ===================================================================

#[test]
fn superuser_bypasses_acl() {
    let engine = EngineBuilder::for_testing().build().unwrap();

    let (admin, _) = engine.startup(startup_params()).expect("admin startup");
    engine
        .execute_sql(
            &admin,
            "CREATE TABLE secret (id INT NOT NULL); \
             CREATE ROLE superadmin SUPERUSER LOGIN",
        )
        .expect("setup");

    // No GRANT to superadmin, but superuser should bypass ACL.
    let (su_session, _) = engine
        .startup(startup_as("superadmin"))
        .expect("superadmin startup");

    engine
        .execute_sql(&su_session, "SELECT * FROM secret")
        .expect("superuser SELECT should succeed");

    engine
        .execute_sql(&su_session, "INSERT INTO secret (id) VALUES (1)")
        .expect("superuser INSERT should succeed");

    engine
        .execute_sql(&su_session, "UPDATE secret SET id = 2")
        .expect("superuser UPDATE should succeed");

    engine
        .execute_sql(&su_session, "DELETE FROM secret")
        .expect("superuser DELETE should succeed");
}

// ===================================================================
// REVOKE removes access
// ===================================================================

#[test]
fn revoke_removes_access() {
    let engine = EngineBuilder::for_testing().build().unwrap();

    let (admin, _) = engine.startup(startup_params()).expect("admin startup");
    engine
        .execute_sql(
            &admin,
            "CREATE ROLE alice SUPERUSER LOGIN; \
             CREATE TABLE items (id INT NOT NULL); \
             CREATE ROLE reader LOGIN; \
             GRANT SELECT ON items TO reader",
        )
        .expect("setup");

    // Verify access works.
    let (reader_session, _) = engine
        .startup(startup_as("reader"))
        .expect("reader startup");
    engine
        .execute_sql(&reader_session, "SELECT * FROM items")
        .expect("SELECT should succeed before revoke");

    // Revoke the privilege.
    engine
        .execute_sql(&admin, "REVOKE SELECT ON items FROM reader")
        .expect("revoke");

    // Verify access is now denied.
    let err = engine
        .execute_sql(&reader_session, "SELECT * FROM items")
        .expect_err("SELECT should be denied after revoke");
    let msg = format!("{err}");
    assert!(msg.contains("permission denied"), "unexpected error: {msg}");
}

// ===================================================================
// UPDATE denied without specific GRANT
// ===================================================================

#[test]
fn update_denied_without_grant() {
    let engine = EngineBuilder::for_testing().build().unwrap();

    let (admin, _) = engine.startup(startup_params()).expect("admin startup");
    engine
        .execute_sql(
            &admin,
            "CREATE ROLE alice SUPERUSER LOGIN; \
             CREATE TABLE items (id INT NOT NULL); \
             CREATE ROLE reader LOGIN; \
             GRANT SELECT ON items TO reader",
        )
        .expect("setup");

    let (reader_session, _) = engine
        .startup(startup_as("reader"))
        .expect("reader startup");

    // SELECT works.
    engine
        .execute_sql(&reader_session, "SELECT * FROM items")
        .expect("SELECT should succeed");

    // UPDATE should be denied (only SELECT was granted).
    let err = engine
        .execute_sql(&reader_session, "UPDATE items SET id = 1")
        .expect_err("UPDATE should be denied");
    let msg = format!("{err}");
    assert!(msg.contains("permission denied"), "unexpected error: {msg}");
}

// ===================================================================
// DELETE denied without specific GRANT
// ===================================================================

#[test]
fn delete_denied_without_grant() {
    let engine = EngineBuilder::for_testing().build().unwrap();

    let (admin, _) = engine.startup(startup_params()).expect("admin startup");
    engine
        .execute_sql(
            &admin,
            "CREATE ROLE alice SUPERUSER LOGIN; \
             CREATE TABLE items (id INT NOT NULL); \
             CREATE ROLE reader LOGIN; \
             GRANT SELECT ON items TO reader",
        )
        .expect("setup");

    let (reader_session, _) = engine
        .startup(startup_as("reader"))
        .expect("reader startup");

    let err = engine
        .execute_sql(&reader_session, "DELETE FROM items")
        .expect_err("DELETE should be denied");
    let msg = format!("{err}");
    assert!(msg.contains("permission denied"), "unexpected error: {msg}");
}

#[test]
fn vector_top_k_ids_denied_without_select_on_target_table() {
    let engine = EngineBuilder::for_testing().build().unwrap();

    let (admin, _) = engine.startup(startup_params()).expect("admin startup");
    engine
        .execute_sql(
            &admin,
            "CREATE TABLE items (id INT NOT NULL, embedding VECTOR(2)); \
             INSERT INTO items VALUES (1, '[1.0,0.0]'); \
             CREATE ROLE reader LOGIN",
        )
        .expect("setup");

    let (reader_session, _) = engine
        .startup(startup_as("reader"))
        .expect("reader startup");

    let err = engine
        .execute_sql(
            &reader_session,
            "SELECT item_id FROM vector_top_k_ids('items', 'embedding', '[1.0,0.0]', 1) AS v(item_id)",
        )
        .expect_err("vector_top_k_ids should be denied");
    let msg = format!("{err}");
    assert!(
        msg.contains("permission denied") || msg.contains("schema-qualified"),
        "unexpected error: {msg}"
    );
}

#[test]
fn grant_usage_on_function_is_rejected() {
    let engine = EngineBuilder::for_testing().build().unwrap();

    let (admin, _) = engine.startup(startup_params()).expect("admin startup");
    engine
        .execute_sql(
            &admin,
            "CREATE ROLE alice SUPERUSER LOGIN; CREATE ROLE reader LOGIN",
        )
        .expect("setup roles");

    let err = engine
        .execute_sql(
            &admin,
            "GRANT USAGE ON FUNCTION vector_top_k_ids(text,text,text,integer) TO reader",
        )
        .expect_err("USAGE should be rejected for functions");
    let report = err.report();
    assert_eq!(
        report.sqlstate,
        aiondb_core::SqlState::InvalidParameterValue
    );
    assert!(
        report
            .message
            .contains("invalid privilege type USAGE for function"),
        "unexpected error: {err}"
    );
}

#[test]
fn function_creator_can_grant_execute_without_superuser() {
    let engine = EngineBuilder::for_testing().build().unwrap();

    let (admin, _) = engine.startup(startup_params()).expect("admin startup");
    engine
        .execute_sql(
            &admin,
            "CREATE ROLE alice SUPERUSER LOGIN; \
             CREATE ROLE fn_owner LOGIN; \
             CREATE ROLE fn_reader LOGIN; \
             GRANT USAGE ON LANGUAGE sql TO fn_owner",
        )
        .expect("setup roles");

    let (owner_session, _) = engine
        .startup(startup_as("fn_owner"))
        .expect("owner startup");
    engine
        .execute_sql(
            &owner_session,
            "CREATE FUNCTION owner_acl_fn(x INT) RETURNS INT AS 'x + 1' LANGUAGE sql; \
             GRANT EXECUTE ON FUNCTION owner_acl_fn(integer) TO fn_reader",
        )
        .expect("owner should be able to grant execute");

    let (reader_session, _) = engine
        .startup(startup_as("fn_reader"))
        .expect("reader startup");
    let rows = query_rows(&engine, &reader_session, "SELECT owner_acl_fn(41)");
    assert_eq!(rows.len(), 1);
    match rows[0].values[0] {
        Value::Int(value) => assert_eq!(value, 42),
        ref other => panic!("expected int result, got {other:?}"),
    }
}

#[test]
fn non_owner_cannot_grant_execute_on_function_without_superuser() {
    let engine = EngineBuilder::for_testing().build().unwrap();

    let (admin, _) = engine.startup(startup_params()).expect("admin startup");
    engine
        .execute_sql(
            &admin,
            "CREATE ROLE alice SUPERUSER LOGIN; \
             CREATE ROLE fn_owner LOGIN; \
             CREATE ROLE fn_granter LOGIN; \
             CREATE ROLE fn_reader LOGIN; \
             GRANT USAGE ON LANGUAGE sql TO fn_owner",
        )
        .expect("setup roles");

    let (owner_session, _) = engine
        .startup(startup_as("fn_owner"))
        .expect("owner startup");
    engine
        .execute_sql(
            &owner_session,
            "CREATE FUNCTION owner_acl_fn_non_owner(x INT) RETURNS INT AS 'x + 1' LANGUAGE sql",
        )
        .expect("owner create function");

    let (granter_session, _) = engine
        .startup(startup_as("fn_granter"))
        .expect("granter startup");
    let err = engine
        .execute_sql(
            &granter_session,
            "GRANT EXECUTE ON FUNCTION owner_acl_fn_non_owner(integer) TO fn_reader",
        )
        .expect_err("non-owner grant should fail");
    assert_eq!(err.sqlstate(), aiondb_core::SqlState::InsufficientPrivilege);
    let msg = format!("{err}");
    assert!(
        msg.contains("owner of function") || msg.contains("permission denied"),
        "unexpected error: {msg}"
    );
}

#[test]
fn non_owner_cannot_revoke_execute_on_function_without_superuser() {
    let engine = EngineBuilder::for_testing().build().unwrap();

    let (admin, _) = engine.startup(startup_params()).expect("admin startup");
    engine
        .execute_sql(
            &admin,
            "CREATE ROLE alice SUPERUSER LOGIN; \
             CREATE ROLE fn_owner LOGIN; \
             CREATE ROLE fn_revoke LOGIN; \
             CREATE ROLE fn_reader LOGIN; \
             GRANT USAGE ON LANGUAGE sql TO fn_owner",
        )
        .expect("setup roles");

    let (owner_session, _) = engine
        .startup(startup_as("fn_owner"))
        .expect("owner startup");
    engine
        .execute_sql(
            &owner_session,
            "CREATE FUNCTION owner_acl_fn_non_owner_revoke(x INT) RETURNS INT AS 'x + 1' LANGUAGE sql; \
             GRANT EXECUTE ON FUNCTION owner_acl_fn_non_owner_revoke(integer) TO fn_reader",
        )
        .expect("owner setup function grant");

    let (revoker_session, _) = engine
        .startup(startup_as("fn_revoke"))
        .expect("revoker startup");
    let err = engine
        .execute_sql(
            &revoker_session,
            "REVOKE EXECUTE ON FUNCTION owner_acl_fn_non_owner_revoke(integer) FROM fn_reader",
        )
        .expect_err("non-owner revoke should fail");
    assert_eq!(err.sqlstate(), aiondb_core::SqlState::InsufficientPrivilege);
    let msg = format!("{err}");
    assert!(
        msg.contains("owner of function") || msg.contains("permission denied"),
        "unexpected error: {msg}"
    );
}

#[test]
fn overloaded_function_execute_grant_targets_specific_signature() {
    let engine = EngineBuilder::for_testing().build().unwrap();

    let (admin, _) = engine.startup(startup_params()).expect("admin startup");
    engine
        .execute_sql(
            &admin,
            "CREATE ROLE alice SUPERUSER LOGIN; \
             CREATE FUNCTION acl_overload(x INT) RETURNS INT AS 'x + 1' LANGUAGE sql; \
             CREATE FUNCTION acl_overload(x INT, y INT) RETURNS INT AS 'x + y' LANGUAGE sql; \
             CREATE ROLE reader LOGIN; \
             REVOKE EXECUTE ON FUNCTION acl_overload(integer) FROM PUBLIC; \
             REVOKE EXECUTE ON FUNCTION acl_overload(integer, integer) FROM PUBLIC; \
             GRANT EXECUTE ON FUNCTION acl_overload(integer) TO reader",
        )
        .expect("setup");

    let (reader_session, _) = engine
        .startup(startup_as("reader"))
        .expect("reader startup");

    engine
        .execute_sql(&reader_session, "SELECT acl_overload(1) AS v")
        .expect("single-arg overload should be allowed");

    let err = engine
        .execute_sql(&reader_session, "SELECT acl_overload(1, 2) AS v")
        .expect_err("non-granted overload should be denied");
    let msg = format!("{err}");
    assert!(
        msg.contains("EXECUTE on function acl_overload"),
        "unexpected error: {msg}"
    );
}

#[test]
fn overloaded_function_execute_revoke_targets_specific_signature() {
    let engine = EngineBuilder::for_testing().build().unwrap();

    let (admin, _) = engine.startup(startup_params()).expect("admin startup");
    engine
        .execute_sql(
            &admin,
            "CREATE ROLE alice SUPERUSER LOGIN; \
             CREATE FUNCTION acl_overload_revoke(x INT) RETURNS INT AS 'x + 1' LANGUAGE sql; \
             CREATE FUNCTION acl_overload_revoke(x INT, y INT) RETURNS INT AS 'x + y' LANGUAGE sql; \
             CREATE ROLE reader LOGIN; \
             REVOKE EXECUTE ON FUNCTION acl_overload_revoke(integer) FROM PUBLIC; \
             REVOKE EXECUTE ON FUNCTION acl_overload_revoke(integer, integer) FROM PUBLIC; \
             GRANT EXECUTE ON FUNCTION acl_overload_revoke(integer) TO reader; \
             GRANT EXECUTE ON FUNCTION acl_overload_revoke(integer, integer) TO reader; \
             REVOKE EXECUTE ON FUNCTION acl_overload_revoke(integer) FROM reader",
        )
        .expect("setup");

    let (reader_session, _) = engine
        .startup(startup_as("reader"))
        .expect("reader startup");

    let err = engine
        .execute_sql(&reader_session, "SELECT acl_overload_revoke(1) AS v")
        .expect_err("revoked overload should be denied");
    let msg = format!("{err}");
    assert!(
        msg.contains("EXECUTE on function acl_overload_revoke"),
        "unexpected error: {msg}"
    );

    engine
        .execute_sql(&reader_session, "SELECT acl_overload_revoke(1, 2) AS v")
        .expect("second overload grant should remain effective");
}

#[test]
fn grant_all_on_function_includes_execute() {
    let engine = EngineBuilder::for_testing().build().unwrap();

    let (admin, _) = engine.startup(startup_params()).expect("admin startup");
    engine
        .execute_sql(
            &admin,
            "CREATE ROLE alice SUPERUSER LOGIN; \
             CREATE TABLE items (id INT NOT NULL, embedding VECTOR(2)); \
             INSERT INTO items VALUES (1, '[1.0,0.0]'); \
             CREATE ROLE reader LOGIN; \
             GRANT SELECT ON items TO reader; \
             GRANT ALL PRIVILEGES ON FUNCTION vector_top_k_ids(text,text,text,integer) TO reader",
        )
        .expect("setup");

    let (reader_session, _) = engine
        .startup(startup_as("reader"))
        .expect("reader startup");

    engine
        .execute_sql(
            &reader_session,
            "SELECT item_id FROM vector_top_k_ids('public.items', 'embedding', '[1.0,0.0]', 1) AS v(item_id)",
        )
        .expect("ALL ON FUNCTION should include EXECUTE");
}

#[test]
fn graph_neighbors_denied_without_select_on_backing_edge_table() {
    let engine = EngineBuilder::for_testing().build().unwrap();

    let (admin, _) = engine.startup(startup_params()).expect("admin startup");
    engine
        .execute_sql(
            &admin,
            "CREATE TABLE docs (id INT NOT NULL); \
             CREATE TABLE doc_links (source_id INT NOT NULL, target_id INT NOT NULL); \
             CREATE NODE LABEL doc ON docs; \
             CREATE EDGE LABEL related_doc ON doc_links SOURCE doc TARGET doc; \
             INSERT INTO doc_links VALUES (1, 2); \
             CREATE ROLE reader LOGIN",
        )
        .expect("setup");

    let (reader_session, _) = engine
        .startup(startup_as("reader"))
        .expect("reader startup");

    let err = engine
        .execute_sql(
            &reader_session,
            "SELECT doc_id FROM graph_neighbors('related_doc', 1) AS g(doc_id)",
        )
        .expect_err("graph_neighbors should be denied");
    let msg = format!("{err}");
    assert!(msg.contains("permission denied"), "unexpected error: {msg}");
}

#[test]
fn vector_top_k_ids_scalar_expression_denied_without_select_on_target_table() {
    let engine = EngineBuilder::for_testing().build().unwrap();

    let (admin, _) = engine.startup(startup_params()).expect("admin startup");
    engine
        .execute_sql(
            &admin,
            "CREATE TABLE items (id INT NOT NULL, embedding VECTOR(2)); \
             INSERT INTO items VALUES (1, '[1.0,0.0]'); \
             CREATE ROLE reader LOGIN",
        )
        .expect("setup");

    let (reader_session, _) = engine
        .startup(startup_as("reader"))
        .expect("reader startup");

    let err = engine
        .execute_sql(
            &reader_session,
            "SELECT vector_top_k_ids('items', 'embedding', '[1.0,0.0]', 1) IS NOT NULL",
        )
        .expect_err("scalar vector_top_k_ids should be denied");
    let msg = format!("{err}");
    assert!(
        msg.contains("permission denied") || msg.contains("schema-qualified"),
        "unexpected error: {msg}"
    );
}

#[test]
fn graph_neighbors_scalar_expression_denied_without_select_on_backing_edge_table() {
    let engine = EngineBuilder::for_testing().build().unwrap();

    let (admin, _) = engine.startup(startup_params()).expect("admin startup");
    engine
        .execute_sql(
            &admin,
            "CREATE TABLE docs (id INT NOT NULL); \
             CREATE TABLE doc_links (source_id INT NOT NULL, target_id INT NOT NULL); \
             CREATE NODE LABEL doc ON docs; \
             CREATE EDGE LABEL related_doc ON doc_links SOURCE doc TARGET doc; \
             INSERT INTO doc_links VALUES (1, 2); \
             CREATE ROLE reader LOGIN",
        )
        .expect("setup");

    let (reader_session, _) = engine
        .startup(startup_as("reader"))
        .expect("reader startup");

    let err = engine
        .execute_sql(
            &reader_session,
            "SELECT graph_neighbors('related_doc', 1) IS NOT NULL",
        )
        .expect_err("scalar graph_neighbors should be denied");
    let msg = format!("{err}");
    assert!(msg.contains("permission denied"), "unexpected error: {msg}");
}

#[test]
fn vector_top_k_ids_dynamic_target_denied_when_roles_are_active() {
    let engine = EngineBuilder::for_testing().build().unwrap();

    let (admin, _) = engine.startup(startup_params()).expect("admin startup");
    engine
        .execute_sql(
            &admin,
            "CREATE TABLE items (id INT NOT NULL, embedding VECTOR(2)); \
             INSERT INTO items VALUES (1, '[1.0,0.0]'); \
             CREATE ROLE reader LOGIN",
        )
        .expect("setup");

    let (reader_session, _) = engine
        .startup(startup_as("reader"))
        .expect("reader startup");

    let err = engine
        .execute_sql(
            &reader_session,
            "SELECT vector_top_k_ids(CASE WHEN TRUE THEN 'items' ELSE 'items' END, 'embedding', '[1.0,0.0]', 1) IS NOT NULL",
        )
        .expect_err("dynamic vector_top_k_ids target must be denied when roles are active");
    let msg = format!("{err}");
    assert!(
        msg.contains("string literal") || msg.contains("permission denied"),
        "unexpected error: {msg}"
    );
}

// ===================================================================
// Creation DDL requires a mapped role when RBAC is active
// ===================================================================

#[test]
fn creation_ddl_denied_for_unknown_identity_when_roles_exist() {
    let engine = EngineBuilder::for_testing().build().unwrap();

    let (admin, _) = engine.startup(startup_params()).expect("admin startup");
    engine
        .execute_sql(&admin, "CREATE ROLE some_role LOGIN")
        .expect("setup");

    let (outsider, _) = engine
        .startup(startup_as("outsider"))
        .expect("outsider startup");

    let err = engine
        .execute_sql(
            &outsider,
            "CREATE TABLE owned_by_outsider (id INT NOT NULL)",
        )
        .expect_err("unmapped identity must not create objects when RBAC is active");
    assert_eq!(err.sqlstate(), aiondb_core::SqlState::InsufficientPrivilege);
    let msg = format!("{err}");
    assert!(
        msg.contains("existing role") || msg.contains("permission denied"),
        "unexpected error: {msg}"
    );
}

#[test]
fn grant_denied_for_unknown_identity_when_target_is_pseudo_role() {
    let engine = EngineBuilder::for_testing().build().unwrap();

    let (admin, _) = engine.startup(startup_params()).expect("admin startup");
    engine
        .execute_sql(
            &admin,
            "CREATE TABLE items (id INT NOT NULL); \
             CREATE ROLE reader LOGIN",
        )
        .expect("setup");

    let (outsider, _) = engine
        .startup(startup_as("outsider"))
        .expect("outsider startup");

    let err = engine
        .execute_sql(&outsider, "GRANT SELECT ON items TO CURRENT_USER")
        .expect_err("GRANT to pseudo-role must be rejected for unmapped identities");
    assert_eq!(err.sqlstate(), aiondb_core::SqlState::InsufficientPrivilege);
    let msg = format!("{err}");
    assert!(
        msg.contains("pseudo-role target") || msg.contains("privilege"),
        "unexpected error: {msg}"
    );
}

#[test]
fn creation_ddl_allowed_for_catalog_role_without_superuser() {
    let engine = EngineBuilder::for_testing().build().unwrap();

    let (admin, _) = engine.startup(startup_params()).expect("admin startup");
    engine
        .execute_sql(
            &admin,
            "CREATE ROLE alice SUPERUSER LOGIN; \
             CREATE ROLE builder LOGIN",
        )
        .expect("setup");

    let (builder, _) = engine
        .startup(startup_as("builder"))
        .expect("builder startup");

    engine
        .execute_sql(&builder, "CREATE TABLE owned_by_builder (id INT NOT NULL)")
        .expect("mapped non-superuser may create a table");
}

#[test]
fn create_schema_denied_for_unknown_identity_when_roles_exist() {
    let engine = EngineBuilder::for_testing().build().unwrap();

    let (admin, _) = engine.startup(startup_params()).expect("admin startup");
    engine
        .execute_sql(&admin, "CREATE ROLE some_role LOGIN")
        .expect("setup");

    let (outsider, _) = engine
        .startup(startup_as("outsider"))
        .expect("outsider startup");

    let err = engine
        .execute_sql(&outsider, "CREATE SCHEMA outsider_schema")
        .expect_err("unmapped identity must not create schema when RBAC is active");
    assert_eq!(err.sqlstate(), aiondb_core::SqlState::InsufficientPrivilege);
    let msg = format!("{err}");
    assert!(
        msg.contains("existing role") || msg.contains("permission denied"),
        "unexpected error: {msg}"
    );
}

#[test]
fn create_schema_allowed_for_catalog_role_without_superuser() {
    let engine = EngineBuilder::for_testing().build().unwrap();

    let (admin, _) = engine.startup(startup_params()).expect("admin startup");
    engine
        .execute_sql(
            &admin,
            "CREATE ROLE alice SUPERUSER LOGIN; \
             CREATE ROLE builder LOGIN",
        )
        .expect("setup");

    let (builder, _) = engine
        .startup(startup_as("builder"))
        .expect("builder startup");

    engine
        .execute_sql(&builder, "CREATE SCHEMA builder_schema")
        .expect("mapped non-superuser may create schema");
}

#[test]
fn grant_denied_for_catalog_role_without_superuser_on_foreign_table() {
    let engine = EngineBuilder::for_testing().build().unwrap();

    let (admin, _) = engine.startup(startup_params()).expect("admin startup");
    engine
        .execute_sql(
            &admin,
            "CREATE ROLE alice SUPERUSER LOGIN; \
             CREATE TABLE items (id INT NOT NULL); \
             CREATE ROLE grantor LOGIN; \
             CREATE ROLE reader LOGIN",
        )
        .expect("setup");

    let (grantor, _) = engine
        .startup(startup_as("grantor"))
        .expect("grantor startup");

    let err = engine
        .execute_sql(&grantor, "GRANT SELECT ON items TO reader")
        .expect_err("non-superuser must not grant on table they do not own");
    assert_eq!(err.sqlstate(), aiondb_core::SqlState::InsufficientPrivilege);
    let msg = format!("{err}");
    assert!(
        msg.contains("must be owner of table") || msg.contains("superuser"),
        "unexpected error: {msg}"
    );
}

#[test]
fn revoke_denied_for_catalog_role_without_superuser_on_foreign_table() {
    let engine = EngineBuilder::for_testing().build().unwrap();

    let (admin, _) = engine.startup(startup_params()).expect("admin startup");
    engine
        .execute_sql(
            &admin,
            "CREATE ROLE alice SUPERUSER LOGIN; \
             CREATE TABLE items (id INT NOT NULL); \
             CREATE ROLE grantor LOGIN; \
             CREATE ROLE reader LOGIN; \
             GRANT SELECT ON items TO reader",
        )
        .expect("setup");

    let (grantor, _) = engine
        .startup(startup_as("grantor"))
        .expect("grantor startup");

    let err = engine
        .execute_sql(&grantor, "REVOKE SELECT ON items FROM reader")
        .expect_err("non-superuser must not revoke on table they do not own");
    assert_eq!(err.sqlstate(), aiondb_core::SqlState::InsufficientPrivilege);
    let msg = format!("{err}");
    assert!(
        msg.contains("must be owner of table")
            || msg.contains("must be owner of object")
            || msg.contains("superuser"),
        "unexpected error: {msg}"
    );
}

#[test]
fn grant_allowed_for_catalog_role_without_superuser_on_owned_table() {
    let engine = EngineBuilder::for_testing().build().unwrap();

    let (admin, _) = engine.startup(startup_params()).expect("admin startup");
    engine
        .execute_sql(
            &admin,
            "CREATE ROLE alice SUPERUSER LOGIN; \
             CREATE ROLE grantor LOGIN; \
             CREATE ROLE reader LOGIN",
        )
        .expect("setup");

    let (grantor, _) = engine
        .startup(startup_as("grantor"))
        .expect("grantor startup");

    engine
        .execute_sql(
            &grantor,
            "CREATE TABLE grantor_items (id INT NOT NULL); \
             GRANT SELECT ON grantor_items TO reader",
        )
        .expect("owner non-superuser may grant on own table");
}

#[test]
fn grant_on_missing_table_returns_undefined_table() {
    let engine = EngineBuilder::for_testing().build().unwrap();

    let (admin, _) = engine.startup(startup_params()).expect("admin startup");
    engine
        .execute_sql(
            &admin,
            "CREATE ROLE alice SUPERUSER LOGIN; \
             CREATE ROLE reader LOGIN",
        )
        .expect("setup");

    let err = engine
        .execute_sql(&admin, "GRANT SELECT ON missing_acl_tbl TO reader")
        .expect_err("grant on missing table must fail");
    assert_eq!(err.sqlstate(), aiondb_core::SqlState::UndefinedTable);
}

#[test]
fn revoke_on_missing_table_returns_undefined_table() {
    let engine = EngineBuilder::for_testing().build().unwrap();

    let (admin, _) = engine.startup(startup_params()).expect("admin startup");
    engine
        .execute_sql(
            &admin,
            "CREATE ROLE alice SUPERUSER LOGIN; \
             CREATE ROLE reader LOGIN",
        )
        .expect("setup");

    let err = engine
        .execute_sql(&admin, "REVOKE SELECT ON missing_acl_tbl FROM reader")
        .expect_err("revoke on missing table must fail");
    assert_eq!(err.sqlstate(), aiondb_core::SqlState::UndefinedTable);
}

#[test]
fn function_ddl_denied_for_unknown_identity_when_roles_exist() {
    let engine = EngineBuilder::for_testing().build().unwrap();

    let (admin, _) = engine.startup(startup_params()).expect("admin startup");
    engine
        .execute_sql(&admin, "CREATE ROLE some_role LOGIN")
        .expect("setup");

    let (outsider, _) = engine
        .startup(startup_as("outsider"))
        .expect("outsider startup");

    let err = engine
        .execute_sql(
            &outsider,
            "CREATE FUNCTION outsider_fn(x INT) RETURNS INT AS 'x' LANGUAGE sql",
        )
        .expect_err("CREATE FUNCTION should be denied for unmapped identity");
    let msg = format!("{err}");
    assert!(
        msg.contains("superuser") || msg.contains("permission denied"),
        "unexpected error: {msg}"
    );
}

#[test]
fn tenant_management_denied_for_unknown_identity_when_roles_exist() {
    let engine = EngineBuilder::for_testing().build().unwrap();

    let (admin, _) = engine.startup(startup_params()).expect("admin startup");
    engine
        .execute_sql(&admin, "CREATE ROLE some_role LOGIN")
        .expect("setup");

    let (outsider, _) = engine
        .startup(startup_as("outsider"))
        .expect("outsider startup");

    let err = engine
        .execute_sql(&outsider, "CREATE TENANT outsider_tenant")
        .expect_err("CREATE TENANT should be denied for unmapped identity");
    let msg = format!("{err}");
    assert!(
        msg.contains("superuser") || msg.contains("permission denied"),
        "unexpected error: {msg}"
    );

    let err = engine
        .execute_sql(&outsider, "DROP TENANT outsider_tenant")
        .expect_err("DROP TENANT should be denied for unmapped identity");
    let msg = format!("{err}");
    assert!(
        msg.contains("superuser") || msg.contains("permission denied"),
        "unexpected error: {msg}"
    );

    let err = engine
        .execute_sql(&outsider, "SET TENANT outsider_tenant")
        .expect_err("SET TENANT should be denied for unmapped identity");
    let msg = format!("{err}");
    assert!(
        msg.contains("superuser") || msg.contains("permission denied"),
        "unexpected error: {msg}"
    );
}

#[test]
fn extension_management_denied_for_unknown_identity_when_roles_exist() {
    let engine = EngineBuilder::for_testing().build().unwrap();

    let (admin, _) = engine.startup(startup_params()).expect("admin startup");
    engine
        .execute_sql(&admin, "CREATE ROLE some_role LOGIN")
        .expect("setup");

    let (outsider, _) = engine
        .startup(startup_as("outsider"))
        .expect("outsider startup");

    let err = engine
        .execute_sql(&outsider, "CREATE EXTENSION pgcrypto")
        .expect_err("CREATE EXTENSION should be denied for unmapped identity");
    let msg = format!("{err}");
    assert!(
        msg.contains("superuser") || msg.contains("permission denied"),
        "unexpected error: {msg}"
    );

    let err = engine
        .execute_sql(&outsider, "DROP EXTENSION pgcrypto")
        .expect_err("DROP EXTENSION should be denied for unmapped identity");
    let msg = format!("{err}");
    assert!(
        msg.contains("superuser") || msg.contains("permission denied"),
        "unexpected error: {msg}"
    );
}

#[test]
fn backup_restore_denied_for_unknown_identity_when_roles_exist() {
    let engine = EngineBuilder::for_testing().build().unwrap();

    let (admin, _) = engine.startup(startup_params()).expect("admin startup");
    engine
        .execute_sql(&admin, "CREATE ROLE some_role LOGIN")
        .expect("setup");

    let (outsider, _) = engine
        .startup(startup_as("outsider"))
        .expect("outsider startup");

    let err = engine
        .execute_sql(&outsider, "BACKUP DATABASE TO 'rbac_outsider_backup.sql'")
        .expect_err("BACKUP DATABASE should be denied for unmapped identity");
    let msg = format!("{err}");
    assert!(
        msg.contains("superuser") || msg.contains("permission denied"),
        "unexpected error: {msg}"
    );

    let err = engine
        .execute_sql(
            &outsider,
            "RESTORE DATABASE FROM 'rbac_outsider_backup.sql'",
        )
        .expect_err("RESTORE DATABASE should be denied for unmapped identity");
    let msg = format!("{err}");
    assert!(
        msg.contains("superuser") || msg.contains("permission denied"),
        "unexpected error: {msg}"
    );
}

// ===================================================================
// vector_top_k_ids: schema-qualified name allowed with GRANT
// ===================================================================

#[test]
fn vector_top_k_ids_qualified_name_allowed_with_grant() {
    let engine = EngineBuilder::for_testing().build().unwrap();

    let (admin, _) = engine.startup(startup_params()).expect("admin startup");
    engine
        .execute_sql(
            &admin,
            "CREATE ROLE alice SUPERUSER LOGIN; \
             CREATE TABLE items (id INT NOT NULL, embedding VECTOR(2)); \
             INSERT INTO items VALUES (1, '[1.0,0.0]'); \
             CREATE ROLE reader LOGIN; \
             GRANT SELECT ON items TO reader; \
             GRANT EXECUTE ON FUNCTION vector_top_k_ids(text,text,text,integer) TO reader",
        )
        .expect("setup");

    let (reader_session, _) = engine
        .startup(startup_as("reader"))
        .expect("reader startup");

    // Schema-qualified name should work fine.
    engine
        .execute_sql(
            &reader_session,
            "SELECT item_id FROM vector_top_k_ids('public.items', 'embedding', '[1.0,0.0]', 1) AS v(item_id)",
        )
        .expect("qualified vector_top_k_ids should succeed with GRANT");
}

#[test]
fn vector_top_k_ids_optional_arguments_allowed_with_legacy_execute_grant() {
    let engine = EngineBuilder::for_testing().build().unwrap();

    let (admin, _) = engine.startup(startup_params()).expect("admin startup");
    engine
        .execute_sql(
            &admin,
            "CREATE ROLE alice SUPERUSER LOGIN; \
             CREATE TABLE items (id INT NOT NULL, embedding VECTOR(2)); \
             INSERT INTO items VALUES (1, '[1.0,0.0]'); \
             CREATE ROLE reader LOGIN; \
             GRANT SELECT ON items TO reader; \
             GRANT EXECUTE ON FUNCTION vector_top_k_ids(text,text,text,integer) TO reader",
        )
        .expect("setup");

    let (reader_session, _) = engine
        .startup(startup_as("reader"))
        .expect("reader startup");

    engine
        .execute_sql(
            &reader_session,
            "SELECT item_id \
             FROM vector_top_k_ids('public.items', 'embedding', '[1.0,0.0]', 1, 'l2', 64, 10.0, false, -0.5) \
                  AS v(item_id)",
        )
        .expect("optional vector_top_k_ids args should succeed with legacy execute grant");
}

#[test]
fn vector_top_k_ids_json_options_allowed_with_legacy_execute_grant() {
    let engine = EngineBuilder::for_testing().build().unwrap();

    let (admin, _) = engine.startup(startup_params()).expect("admin startup");
    engine
        .execute_sql(
            &admin,
            "CREATE ROLE alice SUPERUSER LOGIN; \
             CREATE TABLE items (id INT NOT NULL, embedding VECTOR(2)); \
             INSERT INTO items VALUES (1, '[1.0,0.0]'); \
             CREATE ROLE reader LOGIN; \
             GRANT SELECT ON items TO reader; \
             GRANT EXECUTE ON FUNCTION vector_top_k_ids(text,text,text,integer) TO reader",
        )
        .expect("setup");

    let (reader_session, _) = engine
        .startup(startup_as("reader"))
        .expect("reader startup");

    engine
        .execute_sql(
            &reader_session,
            "SELECT item_id \
             FROM vector_top_k_ids('public.items', 'embedding', '[1.0,0.0]', 1, 'l2', 64, 10.0, false, -10.0, '{\"exact\":true}'::jsonb) \
                  AS v(item_id)",
        )
        .expect("json options should succeed with legacy execute grant");
}

#[test]
fn vector_top_k_hits_optional_arguments_allowed_with_legacy_execute_grant() {
    let engine = EngineBuilder::for_testing().build().unwrap();

    let (admin, _) = engine.startup(startup_params()).expect("admin startup");
    engine
        .execute_sql(
            &admin,
            "CREATE ROLE alice SUPERUSER LOGIN; \
             CREATE TABLE items (id INT NOT NULL, embedding VECTOR(2)); \
             INSERT INTO items VALUES (1, '[1.0,0.0]'); \
             CREATE ROLE reader LOGIN; \
             GRANT SELECT ON items TO reader; \
             GRANT EXECUTE ON FUNCTION vector_top_k_hits(text,text,text,integer) TO reader",
        )
        .expect("setup");

    let (reader_session, _) = engine
        .startup(startup_as("reader"))
        .expect("reader startup");

    engine
        .execute_sql(
            &reader_session,
            "SELECT hit \
             FROM vector_top_k_hits('public.items', 'embedding', '[1.0,0.0]', 1, 'l2', 64, 10.0, false, -10.0, '{\"exact\":true}'::jsonb) \
                  AS hits(hit)",
        )
        .expect("optional vector_top_k_hits args should succeed with legacy execute grant");
}

#[test]
fn vector_recommend_top_k_hits_optional_arguments_allowed_with_legacy_execute_grant() {
    let engine = EngineBuilder::for_testing().build().unwrap();

    let (admin, _) = engine.startup(startup_params()).expect("admin startup");
    engine
        .execute_sql(
            &admin,
            "CREATE ROLE alice SUPERUSER LOGIN; \
             CREATE TABLE items (id INT NOT NULL, embedding VECTOR(2)); \
             INSERT INTO items VALUES (1, '[1.0,0.0]'); \
             CREATE ROLE reader LOGIN; \
             GRANT SELECT ON items TO reader; \
             GRANT EXECUTE ON FUNCTION vector_recommend_top_k_hits(text,text,text,text,integer) TO reader",
        )
        .expect("setup");

    let (reader_session, _) = engine
        .startup(startup_as("reader"))
        .expect("reader startup");

    engine
        .execute_sql(
            &reader_session,
            "SELECT hit \
             FROM vector_recommend_top_k_hits('public.items', 'embedding', '[1.0,0.0]', NULL, 1, 'l2', 64, 10.0, false, -10.0, '{\"exact\":true}'::jsonb) \
                  AS rec(hit)",
        )
        .expect("optional vector_recommend_top_k_hits args should succeed with legacy execute grant");
}

#[test]
fn vector_recommend_top_k_hits_json_examples_allowed_with_legacy_execute_grant() {
    let engine = EngineBuilder::for_testing().build().unwrap();

    let (admin, _) = engine.startup(startup_params()).expect("admin startup");
    engine
        .execute_sql(
            &admin,
            "CREATE ROLE alice SUPERUSER LOGIN; \
             CREATE TABLE items (id INT NOT NULL, embedding VECTOR(2)); \
             INSERT INTO items VALUES (1, '[1.0,0.0]'); \
             CREATE ROLE reader LOGIN; \
             GRANT SELECT ON items TO reader; \
             GRANT EXECUTE ON FUNCTION vector_recommend_top_k_hits(text,text,text,text,integer) TO reader",
        )
        .expect("setup");

    let (reader_session, _) = engine
        .startup(startup_as("reader"))
        .expect("reader startup");

    engine
        .execute_sql(
            &reader_session,
            "SELECT hit \
             FROM vector_recommend_top_k_hits('public.items', 'embedding', '[{\"id\":1}]'::jsonb, NULL::jsonb, 1, 'l2', 64, 10.0, false, -10.0, '{\"exact\":true}'::jsonb) \
                  AS rec(hit)",
        )
        .expect(
            "json examples for vector_recommend_top_k_hits should succeed with legacy execute grant",
        );
}

#[test]
fn full_text_top_k_hits_optional_arguments_allowed_with_legacy_execute_grant() {
    let engine = EngineBuilder::for_testing().build().unwrap();

    let (admin, _) = engine.startup(startup_params()).expect("admin startup");
    engine
        .execute_sql(
            &admin,
            "CREATE ROLE alice SUPERUSER LOGIN; \
             CREATE TABLE docs (id INT NOT NULL, body TEXT); \
             INSERT INTO docs VALUES (1, 'running fast fox'); \
             CREATE ROLE reader LOGIN; \
             GRANT SELECT ON docs TO reader; \
             GRANT EXECUTE ON FUNCTION full_text_top_k_hits(text,text,text,integer) TO reader",
        )
        .expect("setup");

    let (reader_session, _) = engine
        .startup(startup_as("reader"))
        .expect("reader startup");

    engine
        .execute_sql(
            &reader_session,
            "SELECT hit \
             FROM full_text_top_k_hits('public.docs', 'body', 'run fox', 1, 'plain', 'english', 0.0, '{\"offset\":0}'::jsonb) \
                  AS fts(hit)",
        )
        .expect("optional full_text_top_k_hits args should succeed with legacy execute grant");
}

#[test]
fn hybrid_search_top_k_hits_optional_arguments_allowed_with_legacy_execute_grant() {
    let engine = EngineBuilder::for_testing().build().unwrap();

    let (admin, _) = engine.startup(startup_params()).expect("admin startup");
    engine
        .execute_sql(
            &admin,
            "CREATE ROLE alice SUPERUSER LOGIN; \
             CREATE TABLE docs (id INT NOT NULL, body TEXT, embedding VECTOR(2)); \
             INSERT INTO docs VALUES (1, 'running fast fox', '[1.0,0.0]'); \
             CREATE ROLE reader LOGIN; \
             GRANT SELECT ON docs TO reader; \
             GRANT EXECUTE ON FUNCTION hybrid_search_top_k_hits(text,text,text,text,text,integer) TO reader",
        )
        .expect("setup");

    let (reader_session, _) = engine
        .startup(startup_as("reader"))
        .expect("reader startup");

    engine
        .execute_sql(
            &reader_session,
            "SELECT hit \
             FROM hybrid_search_top_k_hits('public.docs', 'embedding', 'body', '[1.0,0.0]', 'run fox', 1, '{\"source_k\":2,\"fusion\":\"dbsf\"}'::jsonb) \
                  AS hybrid(hit)",
        )
        .expect("optional hybrid_search_top_k_hits args should succeed with legacy execute grant");
}

#[test]
fn vector_prefetch_top_k_hits_optional_arguments_allowed_with_legacy_execute_grant() {
    let engine = EngineBuilder::for_testing().build().unwrap();

    let (admin, _) = engine.startup(startup_params()).expect("admin startup");
    engine
        .execute_sql(
            &admin,
            "CREATE ROLE alice SUPERUSER LOGIN; \
             CREATE TABLE items (id INT NOT NULL, embedding VECTOR(2)); \
             INSERT INTO items VALUES (1, '[1.0,0.0]'); \
             CREATE ROLE reader LOGIN; \
             GRANT SELECT ON items TO reader; \
             GRANT EXECUTE ON FUNCTION vector_prefetch_top_k_hits(text,text,text,text,integer) TO reader",
        )
        .expect("setup");

    let (reader_session, _) = engine
        .startup(startup_as("reader"))
        .expect("reader startup");

    engine
        .execute_sql(
            &reader_session,
            "SELECT hit \
             FROM vector_prefetch_top_k_hits('public.items', 'embedding', '[1.0,0.0]', '[{\"id\":1}]'::jsonb, 1, 'l2', 10.0, -10.0, '{\"offset\":0}'::jsonb) \
                  AS pref(hit)",
        )
        .expect("optional vector_prefetch_top_k_hits args should succeed with legacy execute grant");
}

#[test]
fn graph_neighbors_dynamic_target_denied_when_roles_are_active() {
    let engine = EngineBuilder::for_testing().build().unwrap();

    let (admin, _) = engine.startup(startup_params()).expect("admin startup");
    engine
        .execute_sql(
            &admin,
            "CREATE TABLE docs (id INT NOT NULL); \
             CREATE TABLE doc_links (source_id INT NOT NULL, target_id INT NOT NULL); \
             CREATE NODE LABEL doc ON docs; \
             CREATE EDGE LABEL related_doc ON doc_links SOURCE doc TARGET doc; \
             INSERT INTO doc_links VALUES (1, 2); \
             CREATE ROLE reader LOGIN",
        )
        .expect("setup");

    let (reader_session, _) = engine
        .startup(startup_as("reader"))
        .expect("reader startup");

    let err = engine
        .execute_sql(
            &reader_session,
            "SELECT graph_neighbors(CASE WHEN TRUE THEN 'related_doc' ELSE 'related_doc' END, 1) IS NOT NULL",
        )
        .expect_err("dynamic graph_neighbors target must be denied when roles are active");
    let msg = format!("{err}");
    assert!(
        msg.contains("string literal") || msg.contains("permission denied"),
        "unexpected error: {msg}"
    );
}

#[test]
fn update_from_denied_without_select_on_from_table() {
    let engine = EngineBuilder::for_testing().build().unwrap();

    let (admin, _) = engine.startup(startup_params()).expect("admin startup");
    engine
        .execute_sql(
            &admin,
            "CREATE ROLE alice SUPERUSER LOGIN; \
             CREATE TABLE items (id INT NOT NULL); \
             CREATE TABLE src (id INT NOT NULL); \
             INSERT INTO items VALUES (1); \
             INSERT INTO src VALUES (1); \
             CREATE ROLE updater LOGIN; \
             GRANT UPDATE ON items TO updater",
        )
        .expect("setup");

    let (updater_session, _) = engine
        .startup(startup_as("updater"))
        .expect("updater startup");

    let err = engine
        .execute_sql(
            &updater_session,
            "UPDATE items SET id = src.id FROM src WHERE items.id = src.id",
        )
        .expect_err("UPDATE ... FROM should be denied without SELECT on source table");
    let msg = format!("{err}");
    assert!(msg.contains("permission denied"), "unexpected error: {msg}");
}

#[test]
fn delete_using_denied_without_select_on_using_table() {
    let engine = EngineBuilder::for_testing().build().unwrap();

    let (admin, _) = engine.startup(startup_params()).expect("admin startup");
    engine
        .execute_sql(
            &admin,
            "CREATE ROLE alice SUPERUSER LOGIN; \
             CREATE TABLE items (id INT NOT NULL); \
             CREATE TABLE src (id INT NOT NULL); \
             INSERT INTO items VALUES (1); \
             INSERT INTO src VALUES (1); \
             CREATE ROLE deleter LOGIN; \
             GRANT DELETE ON items TO deleter",
        )
        .expect("setup");

    let (deleter_session, _) = engine
        .startup(startup_as("deleter"))
        .expect("deleter startup");

    let err = engine
        .execute_sql(
            &deleter_session,
            "DELETE FROM items USING src WHERE items.id = src.id",
        )
        .expect_err("DELETE ... USING should be denied without SELECT on USING table");
    let msg = format!("{err}");
    assert!(msg.contains("permission denied"), "unexpected error: {msg}");
}

// ===================================================================
// SECURITY REGRESSION TESTS
// ===================================================================
//
// These tests ensure catalog ACL / role enforcement still applies even when
// the engine authorizer is configured as AllowAllAuthorizer.

/// without GRANTed privileges.
#[test]
fn audit_bypass_select_without_grant_on_allow_all_authorizer() {
    let engine = EngineBuilder::for_testing().build().unwrap();

    let (admin, _) = engine.startup(startup_params()).expect("admin startup");
    engine
        .execute_sql(
            &admin,
            "CREATE TABLE secrets (id INT NOT NULL, payload TEXT); \
             INSERT INTO secrets VALUES (1, 'nuclear-codes'); \
             CREATE ROLE attacker LOGIN",
        )
        .expect("setup");

    // Attacker session: a catalog role with no GRANTs whatsoever.
    let (attacker, _) = engine
        .startup(startup_as("attacker"))
        .expect("attacker startup");

    let err = engine
        .execute_sql(&attacker, "SELECT payload FROM secrets")
        .expect_err("SELECT without GRANT should be denied");
    let msg = format!("{err}");
    assert!(msg.contains("permission denied"), "unexpected error: {msg}");
}

#[test]
fn audit_bypass_write_without_grant_on_allow_all_authorizer() {
    let engine = EngineBuilder::for_testing().build().unwrap();

    let (admin, _) = engine.startup(startup_params()).expect("admin startup");
    engine
        .execute_sql(
            &admin,
            "CREATE TABLE balances (account INT NOT NULL, amount INT NOT NULL); \
             INSERT INTO balances VALUES (1, 100); \
             CREATE ROLE attacker LOGIN",
        )
        .expect("setup");

    let (attacker, _) = engine
        .startup(startup_as("attacker"))
        .expect("attacker startup");

    let insert_err = engine
        .execute_sql(&attacker, "INSERT INTO balances VALUES (2, 999999)")
        .expect_err("INSERT without GRANT should be denied");
    assert!(
        format!("{insert_err}").contains("permission denied"),
        "unexpected error: {insert_err}"
    );

    let update_err = engine
        .execute_sql(
            &attacker,
            "UPDATE balances SET amount = 0 WHERE account = 1",
        )
        .expect_err("UPDATE without GRANT should be denied");
    assert!(
        format!("{update_err}").contains("permission denied"),
        "unexpected error: {update_err}"
    );

    let delete_err = engine
        .execute_sql(&attacker, "DELETE FROM balances WHERE account = 2")
        .expect_err("DELETE without GRANT should be denied");
    assert!(
        format!("{delete_err}").contains("permission denied"),
        "unexpected error: {delete_err}"
    );
}

/// After REVOKE, the role must lose access.
#[test]
fn audit_bypass_revoke_does_not_stop_access_on_allow_all_authorizer() {
    let engine = EngineBuilder::for_testing().build().unwrap();

    let (admin, _) = engine.startup(startup_params()).expect("admin startup");
    engine
        .execute_sql(
            &admin,
            "CREATE ROLE alice SUPERUSER LOGIN; \
             CREATE TABLE items (id INT NOT NULL); \
             INSERT INTO items VALUES (1); \
             CREATE ROLE reader LOGIN; \
             GRANT SELECT ON items TO reader",
        )
        .expect("setup");

    let (reader_session, _) = engine
        .startup(startup_as("reader"))
        .expect("reader startup");

    // First read: works.
    engine
        .execute_sql(&reader_session, "SELECT * FROM items")
        .expect("grant honoured");

    // Revoke the privilege.
    engine
        .execute_sql(&admin, "REVOKE SELECT ON items FROM reader")
        .expect("revoke");

    let err = engine
        .execute_sql(&reader_session, "SELECT * FROM items")
        .expect_err("REVOKE should be enforced under AllowAllAuthorizer");
    assert!(
        format!("{err}").contains("permission denied"),
        "unexpected error: {err}"
    );
}

/// ALTER TABLE must reject structural changes by non-owner /
/// non-superuser roles, mirroring PostgreSQL semantics.
#[test]
fn alter_table_owner_to_rejects_non_owner_transfer() {
    let engine = EngineBuilder::for_testing().build().unwrap();

    let (admin, _) = engine.startup(startup_params()).expect("admin startup");
    engine
        .execute_sql(
            &admin,
            "CREATE ROLE alice SUPERUSER LOGIN; \
             CREATE ROLE victim LOGIN; \
             CREATE ROLE attacker LOGIN; \
             SET SESSION AUTHORIZATION victim; \
             CREATE TABLE victim_data (id INT NOT NULL); \
             INSERT INTO victim_data VALUES (1), (2), (3); \
             RESET SESSION AUTHORIZATION",
        )
        .expect("setup");

    let (attacker, _) = engine
        .startup(startup_as("attacker"))
        .expect("attacker startup");

    let alter_err = engine
        .execute_sql(&attacker, "ALTER TABLE victim_data ADD COLUMN stolen INT")
        .expect_err("attacker must not be able to alter victim-owned table");
    let alter_msg = format!("{alter_err}");
    assert!(
        alter_msg.contains("must be owner of table")
            || alter_msg.contains("permission denied")
            || alter_msg.contains("must be superuser"),
        "unexpected error: {alter_msg}"
    );

    let select_err = engine
        .execute_sql(&attacker, "SELECT COUNT(*) FROM victim_data")
        .expect_err("attacker must not gain table access");
    assert!(
        format!("{select_err}").contains("permission denied"),
        "unexpected error: {select_err}"
    );
}

/// SCENARIO 10: `pg_has_role` / `has_table_privilege` consult the live
/// catalog (roles, ownership, grants, inheritance). Unknown roles or
/// unknown relations raise `undefined_object`/`undefined_table`, matching
/// PostgreSQL behaviour.
#[test]
fn pg_has_role_rejects_unknown_roles() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let err = engine
        .execute_sql(
            &session,
            "SELECT pg_has_role('nobody', 'no_such_role', 'MEMBER') AS r",
        )
        .expect_err("pg_has_role should error on unknown probe role");
    let msg = format!("{err}");
    assert!(
        msg.contains("role \"nobody\" does not exist"),
        "unexpected error: {msg}"
    );
}

#[test]
fn has_table_privilege_rejects_unknown_relation() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(&session, "CREATE ROLE regress_priv_audit")
        .expect("create role");

    let err = engine
        .execute_sql(
            &session,
            "SELECT has_table_privilege('regress_priv_audit', 'no_such_table', 'SELECT') AS r",
        )
        .expect_err("unknown relation should error");
    let msg = format!("{err}");
    assert!(
        msg.contains("relation \"no_such_table\" does not exist"),
        "unexpected error: {msg}"
    );
}

#[test]
fn select_respects_row_security_visibility() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (admin, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &admin,
            "CREATE ROLE alice SUPERUSER LOGIN; \
             CREATE ROLE bob LOGIN; \
             CREATE TABLE rls_sel (id INT, owner TEXT); \
             INSERT INTO rls_sel VALUES (1, 'bob'), (2, 'alice'); \
             GRANT SELECT ON rls_sel TO bob; \
             ALTER TABLE rls_sel ENABLE ROW LEVEL SECURITY; \
             CREATE POLICY rls_sel_vis ON rls_sel FOR SELECT TO bob USING (owner = current_user)",
        )
        .expect("setup");

    let rows = query_rows(
        &engine,
        &admin,
        "SET ROLE bob; SELECT id, owner FROM rls_sel ORDER BY id",
    );
    assert_eq!(rows.len(), 1);
    assert_eq!(int_col(&rows[0], 0), 1);
    assert_eq!(text_col(&rows[0], 1), "bob");
}

#[test]
fn non_owner_cannot_alter_or_drop_policy() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (admin, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &admin,
            "CREATE ROLE alice SUPERUSER LOGIN; \
             CREATE ROLE bob LOGIN; \
             SET SESSION AUTHORIZATION alice; \
             CREATE TABLE rls_owner_guard (id INT); \
             CREATE POLICY p1 ON rls_owner_guard USING (true); \
             RESET SESSION AUTHORIZATION; \
             SET SESSION AUTHORIZATION bob",
        )
        .expect("setup");

    let alter_err = engine
        .execute_sql(&admin, "ALTER POLICY p1 ON rls_owner_guard USING (id > 0)")
        .expect_err("non-owner ALTER POLICY must fail");
    let alter_msg = format!("{alter_err}");
    assert!(
        alter_msg.contains("must be owner of table rls_owner_guard"),
        "unexpected error: {alter_msg}"
    );

    let drop_err = engine
        .execute_sql(&admin, "DROP POLICY p1 ON rls_owner_guard")
        .expect_err("non-owner DROP POLICY must fail");
    let drop_msg = format!("{drop_err}");
    assert!(
        drop_msg.contains("must be owner of relation rls_owner_guard"),
        "unexpected error: {drop_msg}"
    );
}

#[test]
fn drop_table_removes_stale_policy_metadata() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (admin, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &admin,
            "CREATE ROLE alice LOGIN; \
             SET SESSION AUTHORIZATION alice; \
             CREATE TABLE rls_recreate (id INT); \
             CREATE POLICY p1 ON rls_recreate USING (true); \
             DROP TABLE rls_recreate; \
             CREATE TABLE rls_recreate (id INT); \
             CREATE POLICY p1 ON rls_recreate USING (true)",
        )
        .expect("policy name should be reusable after dropping/recreating table");
}

#[test]
fn set_session_authorization_then_insert_persists_for_subsequent_select() {
    // Mirrors the privileges suite: a superuser bootstraps roles + a table,
    // then SET SESSION AUTHORIZATION switches to a non-superuser with full
    // privileges. INSERTs by that role must be visible to the same session
    // and to a later session that can read.
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (admin, _) = engine.startup(startup_params()).expect("admin startup");
    engine
        .execute_sql(
            &admin,
            "CREATE ROLE alice SUPERUSER LOGIN; \
             CREATE ROLE u_owner LOGIN; \
             CREATE ROLE u_writer LOGIN;",
        )
        .expect("setup roles");

    // Create table as alice (no roles in catalog yet at startup, but after
    // `CREATE ROLE` they are; alice has no superuser; mirror the privileges
    // suite which grants schema CREATE via GRANT ON SCHEMA public).
    engine
        .execute_sql(&admin, "GRANT CREATE ON SCHEMA public TO u_owner")
        .expect("grant create on public");
    engine
        .execute_sql(&admin, "SET SESSION AUTHORIZATION u_owner")
        .expect("switch to owner");
    engine
        .execute_sql(&admin, "CREATE TABLE atest1 (a int, b text)")
        .expect("create table as owner");
    engine
        .execute_sql(&admin, "GRANT ALL ON atest1 TO u_writer")
        .expect("grant all to writer");
    engine
        .execute_sql(&admin, "RESET SESSION AUTHORIZATION")
        .expect("reset");

    engine
        .execute_sql(&admin, "SET SESSION AUTHORIZATION u_writer")
        .expect("switch to writer");
    engine
        .execute_sql(&admin, "INSERT INTO atest1 VALUES (2, 'two')")
        .expect("insert as writer");
    engine
        .execute_sql(&admin, "INSERT INTO atest1 SELECT 1, b FROM atest1")
        .expect("insert select as writer");
    engine
        .execute_sql(&admin, "UPDATE atest1 SET a = 1 WHERE a = 2")
        .expect("update as writer");

    // Same session, still as writer, must see two rows.
    let rows = query_rows(&engine, &admin, "SELECT * FROM atest1");
    assert_eq!(
        rows.len(),
        2,
        "writer should see its own committed inserts in same session"
    );
}

#[test]
fn join_respects_row_security_visibility_on_scanned_table() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (admin, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &admin,
            "CREATE ROLE alice SUPERUSER LOGIN; \
             CREATE ROLE bob LOGIN; \
             CREATE TABLE rls_left (id INT, owner TEXT); \
             CREATE TABLE rls_right (id INT); \
             INSERT INTO rls_left VALUES (1, 'bob'), (2, 'alice'); \
             INSERT INTO rls_right VALUES (1), (2); \
             GRANT SELECT ON rls_left TO bob; \
             GRANT SELECT ON rls_right TO bob; \
             ALTER TABLE rls_left ENABLE ROW LEVEL SECURITY; \
             CREATE POLICY rls_left_vis ON rls_left FOR SELECT TO bob USING (owner = current_user)",
        )
        .expect("setup");

    let rows = query_rows(
        &engine,
        &admin,
        "SET ROLE bob; \
         SELECT l.id FROM rls_left l JOIN rls_right r ON l.id = r.id ORDER BY l.id",
    );
    assert_eq!(rows.len(), 1);
    assert_eq!(int_col(&rows[0], 0), 1);
}

#[test]
fn row_security_off_errors_for_non_bypass_role() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (admin, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &admin,
            "CREATE ROLE alice SUPERUSER LOGIN; \
             CREATE ROLE bob LOGIN; \
             CREATE TABLE rls_off (id INT, owner TEXT); \
             INSERT INTO rls_off VALUES (1, 'bob'), (2, 'alice'); \
             GRANT SELECT ON rls_off TO bob; \
             ALTER TABLE rls_off ENABLE ROW LEVEL SECURITY; \
             CREATE POLICY rls_off_vis ON rls_off FOR SELECT TO bob USING (owner = current_user)",
        )
        .expect("setup");

    let err = engine
        .execute_sql(
            &admin,
            "SET ROLE bob; SET row_security TO off; SELECT * FROM rls_off ORDER BY id",
        )
        .expect_err("row_security=off should reject policy-affected query");
    let msg = format!("{err}");
    assert!(
        msg.contains("query would be affected by row-level security policy for table \"rls_off\""),
        "unexpected error: {msg}"
    );
}

#[test]
fn bypassrls_role_can_read_with_row_security_off() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (admin, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &admin,
            "CREATE ROLE alice SUPERUSER LOGIN; \
             CREATE ROLE exempt LOGIN BYPASSRLS; \
             CREATE TABLE rls_bypass (id INT, owner TEXT); \
             INSERT INTO rls_bypass VALUES (1, 'exempt'), (2, 'alice'); \
             GRANT SELECT ON rls_bypass TO exempt; \
             ALTER TABLE rls_bypass ENABLE ROW LEVEL SECURITY; \
             CREATE POLICY rls_bypass_vis ON rls_bypass FOR SELECT TO exempt USING (owner = current_user)",
        )
        .expect("setup");

    let (exempt_session, _) = engine
        .startup(startup_as("exempt"))
        .expect("exempt startup");
    let rows = query_rows(
        &engine,
        &exempt_session,
        "SET row_security TO off; SELECT id FROM rls_bypass ORDER BY id",
    );
    assert_eq!(rows.len(), 2);
    assert_eq!(int_col(&rows[0], 0), 1);
    assert_eq!(int_col(&rows[1], 0), 2);
}

#[test]
fn row_security_active_reports_owner_bypass_and_subject_role() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (admin, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &admin,
            "CREATE ROLE alice SUPERUSER LOGIN; \
             CREATE ROLE bob LOGIN; \
             CREATE TABLE rls_active_tbl (id INT, owner TEXT); \
             INSERT INTO rls_active_tbl VALUES (1, 'bob'), (2, 'alice'); \
             GRANT SELECT ON rls_active_tbl TO bob; \
             ALTER TABLE rls_active_tbl ENABLE ROW LEVEL SECURITY; \
             CREATE POLICY rls_active_vis ON rls_active_tbl FOR SELECT TO bob USING (owner = current_user)",
        )
        .expect("setup");

    let owner_rows = query_rows(
        &engine,
        &admin,
        "SET row_security TO on; SELECT row_security_active('rls_active_tbl')",
    );
    assert_eq!(owner_rows.len(), 1);
    assert_eq!(owner_rows[0].values[0], aiondb_core::Value::Boolean(false));

    let subject_rows = query_rows(
        &engine,
        &admin,
        "SET ROLE bob; SET row_security TO on; SELECT row_security_active('rls_active_tbl')",
    );
    assert_eq!(subject_rows.len(), 1);
    assert_eq!(subject_rows[0].values[0], aiondb_core::Value::Boolean(true));
}

// ===================================================================
// SECURITY: TO list with multiple roles must apply to every listed role.
// Regression for an RLS-bypass on RESTRICTIVE policies caused by the
// session-level options string flattening "to=alice, bob" with comma
// separator and the executor splitting top-level commas to recover pairs;
// the recovery turned the role list into "to=alice" plus a stray "bob"
// pair that doesn't match anything, so bob escaped the policy entirely.
// ===================================================================
#[test]
fn restrictive_policy_to_list_applies_to_every_listed_role() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (admin, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &admin,
            "CREATE ROLE alice SUPERUSER LOGIN; \
             CREATE ROLE bob LOGIN; \
             CREATE ROLE carol LOGIN; \
             CREATE TABLE rls_multi (id INT, label TEXT); \
             INSERT INTO rls_multi VALUES (1, 'public'), (2, 'secret'); \
             GRANT SELECT ON rls_multi TO bob; \
             GRANT SELECT ON rls_multi TO carol; \
             ALTER TABLE rls_multi ENABLE ROW LEVEL SECURITY; \
             CREATE POLICY rls_multi_open ON rls_multi FOR SELECT TO public USING (true); \
             CREATE POLICY rls_multi_pii ON rls_multi AS RESTRICTIVE FOR SELECT \
                 TO bob, carol USING (label <> 'secret')",
        )
        .expect("setup");

    // bob is the second role in the TO list. The restrictive policy must
    // apply to bob and hide the 'secret' row. Before the fix, bob saw
    // both rows because the executor never matched the restrictive
    // policy against bob.
    let (bob_session, _) = engine.startup(startup_as("bob")).expect("bob startup");
    let bob_rows = query_rows(
        &engine,
        &bob_session,
        "SELECT id, label FROM rls_multi ORDER BY id",
    );
    assert_eq!(
        bob_rows.len(),
        1,
        "RESTRICTIVE policy must hide 'secret' row from bob (2nd role in TO list)"
    );
    assert_eq!(int_col(&bob_rows[0], 0), 1);
    assert_eq!(text_col(&bob_rows[0], 1), "public");

    // carol is the third role. Same expectation.
    let (carol_session, _) = engine.startup(startup_as("carol")).expect("carol startup");
    let carol_rows = query_rows(
        &engine,
        &carol_session,
        "SELECT id, label FROM rls_multi ORDER BY id",
    );
    assert_eq!(
        carol_rows.len(),
        1,
        "RESTRICTIVE policy must hide 'secret' row from carol (3rd role in TO list)"
    );
}
