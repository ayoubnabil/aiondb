use super::*;
use std::{path::PathBuf, time::Duration};

mod lifecycle;

use aiondb_catalog::{CatalogReader, CatalogWriter, RoleDescriptor};
use aiondb_core::TxnId;
use aiondb_security::AllowAllAuthorizer;
use aiondb_security::ScramVerifier;
use aiondb_security::SecretString;
use aiondb_security::TransportKind;

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

fn startup_with_password(user: &str, password: &str) -> StartupParams {
    StartupParams {
        database: "default".to_owned(),
        application_name: Some("test".to_owned()),
        options: BTreeMap::new(),
        credential: Credential::CleartextPassword {
            user: user.to_owned(),
            password: SecretString::new(password.to_owned()),
        },
        transport: TransportInfo::in_process(),
    }
}

fn startup_with_network_password(user: &str, password: &str, tls: bool) -> StartupParams {
    StartupParams {
        database: "default".to_owned(),
        application_name: Some("test".to_owned()),
        options: BTreeMap::new(),
        credential: Credential::CleartextPassword {
            user: user.to_owned(),
            password: SecretString::new(password.to_owned()),
        },
        transport: network_transport(tls),
    }
}

fn network_transport(tls: bool) -> TransportInfo {
    TransportInfo {
        kind: TransportKind::Network {
            tls,
            peer_addr: Some("127.0.0.1:5432".to_owned()),
        },
    }
}

/// Bootstrap the ephemeral "alice" identity as a catalog-backed superuser.
///
/// Must be the **first** SQL the session executes, before any other
/// `CREATE ROLE`, so that RBAC is still inactive and the `CREATE` succeeds
/// without superuser authorisation.  After this call, "alice" is a recognised
/// superuser in the catalog and all subsequent admin operations will pass the
/// RBAC checks that now activate the moment any role exists.
fn bootstrap_admin(engine: &Engine, session: &SessionHandle) {
    engine
        .execute_sql(session, "CREATE ROLE alice SUPERUSER LOGIN")
        .expect("bootstrap admin superuser");
}

fn auth_hardened_runtime(max_failures: u32) -> aiondb_config::RuntimeConfig {
    let mut runtime = aiondb_config::RuntimeConfig::default();
    runtime.security =
        aiondb_config::SecurityConfig::from_profile(aiondb_config::SecurityProfile::Development);
    runtime.security.max_auth_failures = max_failures;
    runtime.security.auth_lockout_window = Duration::from_secs(60);
    runtime
}

fn auth_hardened_engine(max_failures: u32) -> Engine {
    EngineBuilder::for_testing()
        .with_runtime_config(auth_hardened_runtime(max_failures))
        .build()
        .unwrap()
}

#[allow(clippy::fn_params_excessive_bools)]
fn password_policy_engine(
    password_min_length: usize,
    reject_role_name_as_password: bool,
    password_require_lowercase: bool,
    password_require_uppercase: bool,
    password_require_digit: bool,
    password_require_symbol: bool,
    catalog: Arc<aiondb_catalog_store::CatalogStore>,
    storage: Arc<aiondb_storage_engine::InMemoryStorage>,
) -> Engine {
    let mut runtime = aiondb_config::RuntimeConfig::default();
    runtime.security =
        aiondb_config::SecurityConfig::from_profile(aiondb_config::SecurityProfile::Development);
    runtime.security.password_min_length = password_min_length;
    runtime.security.reject_role_name_as_password = reject_role_name_as_password;
    runtime.security.password_require_lowercase = password_require_lowercase;
    runtime.security.password_require_uppercase = password_require_uppercase;
    runtime.security.password_require_digit = password_require_digit;
    runtime.security.password_require_symbol = password_require_symbol;
    EngineBuilder::for_testing()
        .with_runtime_config(runtime)
        .with_catalog_txn(catalog.clone())
        .with_catalog_reader(catalog.clone())
        .with_catalog_writer(catalog.clone())
        .with_sequence_manager(catalog)
        .with_storage_ddl(storage.clone())
        .with_storage_dml(storage.clone())
        .with_storage_txn(storage)
        .build()
        .unwrap()
}

fn durable_lockout_engine(
    max_failures: u32,
    state_path: PathBuf,
    catalog: Arc<aiondb_catalog_store::CatalogStore>,
    storage: Arc<aiondb_storage_engine::InMemoryStorage>,
) -> Engine {
    let mut runtime = auth_hardened_runtime(max_failures);
    runtime.security.durable_auth_lockout = true;
    runtime.security.auth_lockout_state_path = Some(state_path);
    EngineBuilder::for_testing()
        .with_runtime_config(runtime)
        .with_catalog_txn(catalog.clone())
        .with_catalog_reader(catalog.clone())
        .with_catalog_writer(catalog.clone())
        .with_sequence_manager(catalog)
        .with_storage_ddl(storage.clone())
        .with_storage_dml(storage.clone())
        .with_storage_txn(storage)
        .build()
        .unwrap()
}

fn tls_required_engine(
    catalog: Arc<aiondb_catalog_store::CatalogStore>,
    storage: Arc<aiondb_storage_engine::InMemoryStorage>,
) -> Engine {
    let mut runtime = aiondb_config::RuntimeConfig::default();
    runtime.security =
        aiondb_config::SecurityConfig::from_profile(aiondb_config::SecurityProfile::Development);
    runtime.security.require_tls_for_password = true;
    EngineBuilder::for_testing()
        .with_runtime_config(runtime)
        .with_catalog_txn(catalog.clone())
        .with_catalog_reader(catalog.clone())
        .with_catalog_writer(catalog.clone())
        .with_sequence_manager(catalog)
        .with_storage_ddl(storage.clone())
        .with_storage_dml(storage.clone())
        .with_storage_txn(storage)
        .build()
        .unwrap()
}

fn durable_lockout_state_path(name: &str) -> PathBuf {
    let path = crate::test_support::unique_temp_path("engine-tests-durable-lockout", name)
        .with_extension("state");
    let _ = std::fs::remove_file(&path);
    let _ = std::fs::remove_file(format!("{}.lock", path.display()));
    let _ = std::fs::remove_file(format!("{}.tmp", path.display()));
    path
}

fn cleanup_lockout_state(path: &PathBuf) {
    let _ = std::fs::remove_file(path);
    let _ = std::fs::remove_file(format!("{}.lock", path.display()));
    let _ = std::fs::remove_file(format!("{}.tmp", path.display()));
}

#[test]
fn create_role_with_password_persists_scram_verifier() {
    let catalog = Arc::new(aiondb_catalog_store::CatalogStore::default());
    let storage = Arc::new(aiondb_storage_engine::InMemoryStorage::new_without_wal());
    let engine = build_engine_with_store(catalog.clone(), storage);
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(&session, "CREATE ROLE testrole LOGIN PASSWORD 'secret'")
        .expect("create role with password");

    let role = catalog
        .get_role(TxnId::default(), "testrole")
        .expect("catalog lookup")
        .expect("role should exist");
    let password_hash = role.password_hash.expect("password hash should exist");
    assert_ne!(password_hash, "secret");
    assert!(password_hash.starts_with("SCRAM-SHA-256$"));
    ScramVerifier::from_password_hash_string(&password_hash).expect("valid SCRAM verifier");
}

#[test]
fn startup_with_role_password_accepts_matching_password() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(&session, "CREATE ROLE testrole LOGIN PASSWORD 'secret'")
        .expect("create role");

    let (role_session, info) = engine
        .startup(startup_with_password("testrole", "secret"))
        .expect("role startup");

    assert_eq!(info.identity.user, "testrole");
    engine
        .terminate(role_session)
        .expect("terminate role session");
}

#[test]
fn startup_with_role_password_rejects_wrong_password() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(&session, "CREATE ROLE testrole LOGIN PASSWORD 'secret'")
        .expect("create role");

    let error = engine
        .startup(startup_with_password("testrole", "wrong"))
        .expect_err("wrong password should fail");
    assert_eq!(
        error.sqlstate(),
        aiondb_core::SqlState::InvalidAuthorizationSpecification
    );
}

#[test]
fn startup_authentication_network_invalid_password_hash_offers_decoy_scram_challenge() {
    let catalog = Arc::new(aiondb_catalog_store::CatalogStore::default());
    let storage = Arc::new(aiondb_storage_engine::InMemoryStorage::new_without_wal());
    let engine = tls_required_engine(catalog.clone(), storage);

    catalog
        .create_role(
            TxnId::default(),
            RoleDescriptor {
                name: "legacy_tls".to_owned(),
                login: true,
                superuser: false,
                password_hash: Some("legacysecret".to_owned()),
                ..RoleDescriptor::default()
            },
        )
        .expect("seed legacy role");

    let auth = engine
        .startup_authentication("legacy_tls", "default", &network_transport(false))
        .expect("invalid network hash should be masked with a decoy SCRAM challenge");
    assert!(matches!(auth, StartupAuthentication::ScramSha256 { .. }));

    let auth = engine
        .startup_authentication("legacy_tls", "default", &network_transport(true))
        .expect("invalid network hash should remain masked with a decoy SCRAM challenge");
    assert!(matches!(auth, StartupAuthentication::ScramSha256 { .. }));
}

#[test]
fn startup_authentication_inprocess_invalid_password_hash_fails_immediately() {
    let catalog = Arc::new(aiondb_catalog_store::CatalogStore::default());
    let storage = Arc::new(aiondb_storage_engine::InMemoryStorage::new_without_wal());
    let engine = build_engine_with_store(catalog.clone(), storage);

    catalog
        .create_role(
            TxnId::default(),
            RoleDescriptor {
                name: "legacy_local".to_owned(),
                login: true,
                superuser: false,
                password_hash: Some("legacysecret".to_owned()),
                ..RoleDescriptor::default()
            },
        )
        .expect("seed legacy role");

    let err = engine
        .startup_authentication("legacy_local", "default", &TransportInfo::in_process())
        .expect_err("invalid local hash should fail immediately");
    let message = format!("{err}");
    assert!(
        message.contains("stored password format is invalid"),
        "unexpected error: {message}"
    );
}

#[test]
fn startup_with_cleartext_password_for_scram_role_requires_tls_when_configured() {
    let catalog = Arc::new(aiondb_catalog_store::CatalogStore::default());
    let storage = Arc::new(aiondb_storage_engine::InMemoryStorage::new_without_wal());
    let engine = tls_required_engine(catalog.clone(), storage);
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(&session, "CREATE ROLE tls_scram LOGIN PASSWORD 'secret'")
        .expect("create role");

    let err = engine
        .startup(startup_with_network_password("tls_scram", "secret", false))
        .expect_err("cleartext startup without TLS should fail");
    assert_eq!(
        err.sqlstate(),
        aiondb_core::SqlState::InvalidAuthorizationSpecification
    );
    assert!(
        format!("{err}").contains("TLS is required"),
        "unexpected error: {err}"
    );

    let (role_session, info) = engine
        .startup(startup_with_network_password("tls_scram", "secret", true))
        .expect("TLS cleartext startup should succeed");
    assert_eq!(info.identity.user, "tls_scram");
    engine
        .terminate(role_session)
        .expect("terminate role session");
}

#[test]
fn startup_authentication_still_offers_scram_without_tls_when_password_is_hashed() {
    let catalog = Arc::new(aiondb_catalog_store::CatalogStore::default());
    let storage = Arc::new(aiondb_storage_engine::InMemoryStorage::new_without_wal());
    let engine = tls_required_engine(catalog.clone(), storage);
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(&session, "CREATE ROLE scram_net LOGIN PASSWORD 'secret'")
        .expect("create role");

    let auth = engine
        .startup_authentication("scram_net", "default", &network_transport(false))
        .expect("SCRAM startup auth should still be offered");
    assert!(matches!(auth, StartupAuthentication::ScramSha256 { .. }));
}

#[test]
fn startup_authentication_unknown_network_role_offers_decoy_scram_challenge() {
    let engine = EngineBuilder::new_in_memory()
        .with_authorizer(Arc::new(AllowAllAuthorizer))
        .build()
        .unwrap();

    let auth = engine
        .startup_authentication("ghost_net", "default", &network_transport(false))
        .expect("unknown network role should receive a decoy SCRAM challenge");
    assert!(matches!(auth, StartupAuthentication::ScramSha256 { .. }));
}

#[test]
fn startup_authentication_network_nologin_role_offers_decoy_scram_challenge() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");
    bootstrap_admin(&engine, &session);

    engine
        .execute_sql(
            &session,
            "CREATE ROLE locked_login LOGIN PASSWORD 'secret'; \
             ALTER ROLE locked_login NOLOGIN",
        )
        .expect("setup role");

    let auth = engine
        .startup_authentication("locked_login", "default", &network_transport(false))
        .expect("network NOLOGIN role should be masked with a decoy SCRAM challenge");
    assert!(matches!(auth, StartupAuthentication::ScramSha256 { .. }));
}

#[test]
fn startup_authentication_network_role_without_password_offers_decoy_scram_challenge() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(&session, "CREATE ROLE nopass_net LOGIN")
        .expect("setup role");

    let auth = engine
        .startup_authentication("nopass_net", "default", &network_transport(false))
        .expect("network role without password should be masked with a decoy SCRAM challenge");
    assert!(matches!(auth, StartupAuthentication::ScramSha256 { .. }));
}

#[test]
fn startup_unknown_network_role_reports_generic_password_error() {
    let engine = EngineBuilder::new_in_memory()
        .with_authorizer(Arc::new(AllowAllAuthorizer))
        .build()
        .unwrap();

    let err = engine
        .startup(startup_with_network_password("ghost_net", "wrong", true))
        .expect_err("unknown network role should fail with a generic password error");
    assert_eq!(
        err.sqlstate(),
        aiondb_core::SqlState::InvalidAuthorizationSpecification
    );
    let message = format!("{err}");
    assert!(
        message.contains("invalid user name or password"),
        "unexpected error: {message}"
    );
    assert!(
        !message.contains("does not exist"),
        "network auth should not reveal role existence: {message}"
    );
}

#[test]
fn startup_network_non_authenticable_roles_report_generic_password_error() {
    let catalog = Arc::new(aiondb_catalog_store::CatalogStore::default());
    let storage = Arc::new(aiondb_storage_engine::InMemoryStorage::new_without_wal());
    let engine = build_engine_with_store(catalog.clone(), storage);
    let (session, _) = engine.startup(startup_params()).expect("startup");
    bootstrap_admin(&engine, &session);

    engine
        .execute_sql(
            &session,
            "CREATE ROLE locked_login LOGIN PASSWORD 'secret'; \
             ALTER ROLE locked_login NOLOGIN; \
             CREATE ROLE nopass_net LOGIN",
        )
        .expect("setup roles");

    catalog
        .create_role(
            TxnId::default(),
            RoleDescriptor {
                name: "legacy_bad_hash".to_owned(),
                login: true,
                superuser: false,
                password_hash: Some("legacysecret".to_owned()),
                ..RoleDescriptor::default()
            },
        )
        .expect("seed legacy hash role");

    for role_name in ["locked_login", "nopass_net", "legacy_bad_hash"] {
        let err = engine
            .startup(startup_with_network_password(role_name, "wrong", true))
            .expect_err("network auth should fail with generic password error");
        assert_eq!(
            err.sqlstate(),
            aiondb_core::SqlState::InvalidAuthorizationSpecification
        );
        let message = format!("{err}");
        assert!(
            message.contains("invalid user name or password"),
            "unexpected error for {role_name}: {message}"
        );
        assert!(
            !message.contains("not permitted to log in"),
            "network auth should not reveal NOLOGIN state for {role_name}: {message}"
        );
        assert!(
            !message.contains("does not have a password configured"),
            "network auth should not reveal password configuration for {role_name}: {message}"
        );
        assert!(
            !message.contains("stored password format is invalid"),
            "network auth should not reveal password hash format for {role_name}: {message}"
        );
    }
}

#[test]
fn create_role_rejects_password_shorter_than_policy() {
    let catalog = Arc::new(aiondb_catalog_store::CatalogStore::default());
    let storage = Arc::new(aiondb_storage_engine::InMemoryStorage::new_without_wal());
    let engine = password_policy_engine(
        10,
        false,
        false,
        false,
        false,
        false,
        catalog.clone(),
        storage,
    );
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let err = engine
        .execute_sql(&session, "CREATE ROLE weak LOGIN PASSWORD 'short'")
        .expect_err("short password should be rejected");
    assert_eq!(
        err.sqlstate(),
        aiondb_core::SqlState::InvalidAuthorizationSpecification
    );
    assert!(
        format!("{err}").contains("at least 10 characters"),
        "unexpected error: {err}"
    );
    assert!(
        catalog
            .get_role(TxnId::default(), "weak")
            .expect("catalog lookup")
            .is_none(),
        "role should not be persisted after policy rejection"
    );
}

#[test]
fn alter_role_rejects_password_matching_role_name_when_policy_enabled() {
    let catalog = Arc::new(aiondb_catalog_store::CatalogStore::default());
    let storage = Arc::new(aiondb_storage_engine::InMemoryStorage::new_without_wal());
    let engine = password_policy_engine(
        0,
        true,
        false,
        false,
        false,
        false,
        catalog.clone(),
        storage,
    );
    let (session, _) = engine.startup(startup_params()).expect("startup");
    bootstrap_admin(&engine, &session);

    engine
        .execute_sql(
            &session,
            "CREATE ROLE app_user LOGIN PASSWORD 'validsecret'",
        )
        .expect("create role");
    let old_hash = catalog
        .get_role(TxnId::default(), "app_user")
        .expect("catalog lookup")
        .expect("role should exist")
        .password_hash
        .expect("password hash should exist");

    let err = engine
        .execute_sql(&session, "ALTER ROLE app_user WITH PASSWORD 'APP_USER'")
        .expect_err("role-name password should be rejected");
    assert_eq!(
        err.sqlstate(),
        aiondb_core::SqlState::InvalidAuthorizationSpecification
    );
    assert!(
        format!("{err}").contains("must not match role name"),
        "unexpected error: {err}"
    );

    let new_hash = catalog
        .get_role(TxnId::default(), "app_user")
        .expect("catalog lookup")
        .expect("role should exist")
        .password_hash
        .expect("password hash should exist");
    assert_eq!(old_hash, new_hash);

    let (role_session, _) = engine
        .startup(startup_with_password("app_user", "validsecret"))
        .expect("original password should still work");
    engine
        .terminate(role_session)
        .expect("terminate role session");
}

#[test]
fn create_role_rejects_password_missing_required_character_classes() {
    let catalog = Arc::new(aiondb_catalog_store::CatalogStore::default());
    let storage = Arc::new(aiondb_storage_engine::InMemoryStorage::new_without_wal());
    let engine = password_policy_engine(0, false, true, true, true, true, catalog.clone(), storage);
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let missing_uppercase = engine
        .execute_sql(
            &session,
            "CREATE ROLE lower_only LOGIN PASSWORD 'password1!'",
        )
        .expect_err("missing uppercase should be rejected");
    assert!(
        format!("{missing_uppercase}").contains("at least one uppercase letter"),
        "unexpected error: {missing_uppercase}"
    );

    let missing_lowercase = engine
        .execute_sql(
            &session,
            "CREATE ROLE upper_only LOGIN PASSWORD 'PASSWORD1!'",
        )
        .expect_err("missing lowercase should be rejected");
    assert!(
        format!("{missing_lowercase}").contains("at least one lowercase letter"),
        "unexpected error: {missing_lowercase}"
    );

    let missing_digit = engine
        .execute_sql(
            &session,
            "CREATE ROLE digit_only LOGIN PASSWORD 'Password!'",
        )
        .expect_err("missing digit should be rejected");
    assert!(
        format!("{missing_digit}").contains("at least one digit"),
        "unexpected error: {missing_digit}"
    );

    let missing_symbol = engine
        .execute_sql(
            &session,
            "CREATE ROLE symbol_only LOGIN PASSWORD 'Password1'",
        )
        .expect_err("missing symbol should be rejected");
    assert!(
        format!("{missing_symbol}").contains("at least one symbol"),
        "unexpected error: {missing_symbol}"
    );

    engine
        .execute_sql(&session, "CREATE ROLE strong LOGIN PASSWORD 'Password1!'")
        .expect("strong password should satisfy complexity policy");
}

#[test]
fn alter_role_rejects_password_missing_required_character_classes() {
    let catalog = Arc::new(aiondb_catalog_store::CatalogStore::default());
    let storage = Arc::new(aiondb_storage_engine::InMemoryStorage::new_without_wal());
    let engine = password_policy_engine(0, false, true, true, true, true, catalog.clone(), storage);
    let (session, _) = engine.startup(startup_params()).expect("startup");
    bootstrap_admin(&engine, &session);

    engine
        .execute_sql(
            &session,
            "CREATE ROLE complex_user LOGIN PASSWORD 'Password1!'",
        )
        .expect("create role");

    let err = engine
        .execute_sql(
            &session,
            "ALTER ROLE complex_user WITH PASSWORD 'password1!'",
        )
        .expect_err("missing uppercase should be rejected");
    assert!(
        format!("{err}").contains("at least one uppercase letter"),
        "unexpected error: {err}"
    );
}

#[test]
fn alter_role_rotates_password_and_rejects_old_secret() {
    let catalog = Arc::new(aiondb_catalog_store::CatalogStore::default());
    let storage = Arc::new(aiondb_storage_engine::InMemoryStorage::new_without_wal());
    let engine = build_engine_with_store(catalog.clone(), storage);
    let (session, _) = engine.startup(startup_params()).expect("startup");
    bootstrap_admin(&engine, &session);

    engine
        .execute_sql(&session, "CREATE ROLE testrole LOGIN PASSWORD 'secret'")
        .expect("create role");

    let old_hash = catalog
        .get_role(TxnId::default(), "testrole")
        .expect("catalog lookup")
        .expect("role should exist")
        .password_hash
        .expect("password hash should exist");

    engine
        .execute_sql(&session, "ALTER ROLE testrole WITH PASSWORD 'rotated'")
        .expect("alter role");

    let new_hash = catalog
        .get_role(TxnId::default(), "testrole")
        .expect("catalog lookup")
        .expect("role should exist")
        .password_hash
        .expect("password hash should exist");
    assert_ne!(old_hash, new_hash);
    ScramVerifier::from_password_hash_string(&new_hash).expect("valid SCRAM verifier");

    let err = engine
        .startup(startup_with_password("testrole", "secret"))
        .expect_err("old password should fail");
    assert_eq!(
        err.sqlstate(),
        aiondb_core::SqlState::InvalidAuthorizationSpecification
    );

    let (role_session, _) = engine
        .startup(startup_with_password("testrole", "rotated"))
        .expect("new password should work");
    engine
        .terminate(role_session)
        .expect("terminate role session");
}

#[test]
fn alter_role_nologin_blocks_startup() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");
    bootstrap_admin(&engine, &session);

    engine
        .execute_sql(&session, "CREATE ROLE testrole LOGIN")
        .expect("create role");
    engine
        .execute_sql(&session, "ALTER ROLE testrole NOLOGIN")
        .expect("alter role");

    let err = engine
        .startup(startup_as("testrole"))
        .expect_err("nologin role should not start");
    assert_eq!(
        err.sqlstate(),
        aiondb_core::SqlState::InvalidAuthorizationSpecification
    );
}

#[test]
fn alter_role_nosuperuser_removes_acl_bypass() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (admin, _) = engine.startup(startup_params()).expect("startup");
    bootstrap_admin(&engine, &admin);

    engine
        .execute_sql(
            &admin,
            "CREATE TABLE secret (id INT NOT NULL); \
             CREATE ROLE superadmin SUPERUSER LOGIN",
        )
        .expect("setup");

    let (superuser_session, _) = engine
        .startup(startup_as("superadmin"))
        .expect("superuser startup");
    engine
        .execute_sql(&superuser_session, "SELECT * FROM secret")
        .expect("superuser should bypass ACL");
    engine
        .terminate(superuser_session)
        .expect("terminate superuser session");

    engine
        .execute_sql(&admin, "ALTER ROLE superadmin NOSUPERUSER")
        .expect("alter role");

    let (user_session, _) = engine
        .startup(startup_as("superadmin"))
        .expect("startup after losing superuser");
    let err = engine
        .execute_sql(&user_session, "SELECT * FROM secret")
        .expect_err("ACL bypass should be removed");
    let msg = format!("{err}");
    assert!(msg.contains("permission denied"), "unexpected error: {msg}");
    engine
        .terminate(user_session)
        .expect("terminate downgraded session");
}

#[test]
fn startup_lockout_triggers_after_configured_failures() {
    let engine = auth_hardened_engine(2);
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(&session, "CREATE ROLE locked LOGIN PASSWORD 'secret'")
        .expect("create role");

    for _ in 0..2 {
        let err = engine
            .startup(startup_with_password("locked", "wrong"))
            .expect_err("wrong password should fail");
        assert_eq!(
            err.sqlstate(),
            aiondb_core::SqlState::InvalidAuthorizationSpecification
        );
    }

    let err = engine
        .startup(startup_with_password("locked", "wrong"))
        .expect_err("user should be locked out");
    assert_eq!(
        err.sqlstate(),
        aiondb_core::SqlState::TooManyAuthenticationFailures
    );
    assert!(
        format!("{err}").contains("too many authentication failures"),
        "unexpected error: {err}"
    );
}

#[test]
fn default_builder_rejects_unknown_ephemeral_user() {
    let engine = EngineBuilder::new_in_memory()
        .with_authorizer(Arc::new(AllowAllAuthorizer))
        .build()
        .unwrap();

    let err = engine
        .startup(startup_as("ghost"))
        .expect_err("default builder should reject unknown roles");
    assert_eq!(
        err.sqlstate(),
        aiondb_core::SqlState::InvalidAuthorizationSpecification
    );
    assert!(
        format!("{err}").contains("role \"ghost\" does not exist"),
        "unexpected error: {err}"
    );
}

#[test]
fn for_testing_preserves_ephemeral_users_with_custom_runtime_config() {
    let engine = EngineBuilder::for_testing()
        .with_runtime_config(aiondb_config::RuntimeConfig::default())
        .build()
        .unwrap();

    engine
        .startup(startup_params())
        .expect("for_testing should still allow ephemeral startup");
}

#[test]
fn successful_startup_clears_failure_counter() {
    let engine = auth_hardened_engine(2);
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(&session, "CREATE ROLE resetme LOGIN PASSWORD 'secret'")
        .expect("create role");

    let err = engine
        .startup(startup_with_password("resetme", "wrong"))
        .expect_err("wrong password should fail");
    assert_eq!(
        err.sqlstate(),
        aiondb_core::SqlState::InvalidAuthorizationSpecification
    );

    let (role_session, _) = engine
        .startup(startup_with_password("resetme", "secret"))
        .expect("correct password should succeed");
    engine
        .terminate(role_session)
        .expect("terminate role session");

    let err = engine
        .startup(startup_with_password("resetme", "wrong"))
        .expect_err("wrong password should still fail normally");
    assert_eq!(
        err.sqlstate(),
        aiondb_core::SqlState::InvalidAuthorizationSpecification
    );

    let (role_session, _) = engine
        .startup(startup_with_password("resetme", "secret"))
        .expect("success should have reset failures");
    engine
        .terminate(role_session)
        .expect("terminate role session");
}

#[test]
fn durable_lockout_survives_engine_restart() {
    let state_path = durable_lockout_state_path("survives-restart");
    let catalog = Arc::new(aiondb_catalog_store::CatalogStore::default());
    let storage = Arc::new(aiondb_storage_engine::InMemoryStorage::new_without_wal());
    let engine = durable_lockout_engine(2, state_path.clone(), catalog.clone(), storage.clone());
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(&session, "CREATE ROLE durable LOGIN PASSWORD 'secret'")
        .expect("create role");

    for _ in 0..2 {
        let err = engine
            .startup(startup_with_password("durable", "wrong"))
            .expect_err("wrong password should fail");
        assert_eq!(
            err.sqlstate(),
            aiondb_core::SqlState::InvalidAuthorizationSpecification
        );
    }
    drop(engine);

    let restarted = durable_lockout_engine(2, state_path.clone(), catalog, storage);
    let err = restarted
        .startup(startup_with_password("durable", "wrong"))
        .expect_err("lockout should survive restart");
    assert_eq!(
        err.sqlstate(),
        aiondb_core::SqlState::TooManyAuthenticationFailures
    );
    cleanup_lockout_state(&state_path);
}

#[test]
fn durable_lockout_reset_persists_after_success() {
    let state_path = durable_lockout_state_path("reset-after-success");
    let catalog = Arc::new(aiondb_catalog_store::CatalogStore::default());
    let storage = Arc::new(aiondb_storage_engine::InMemoryStorage::new_without_wal());
    let engine = durable_lockout_engine(2, state_path.clone(), catalog.clone(), storage.clone());
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(
            &session,
            "CREATE ROLE durable_reset LOGIN PASSWORD 'secret'",
        )
        .expect("create role");

    let err = engine
        .startup(startup_with_password("durable_reset", "wrong"))
        .expect_err("wrong password should fail");
    assert_eq!(
        err.sqlstate(),
        aiondb_core::SqlState::InvalidAuthorizationSpecification
    );

    let (role_session, _) = engine
        .startup(startup_with_password("durable_reset", "secret"))
        .expect("correct password should succeed");
    engine
        .terminate(role_session)
        .expect("terminate role session");
    drop(engine);

    let restarted = durable_lockout_engine(2, state_path.clone(), catalog, storage);
    let err = restarted
        .startup(startup_with_password("durable_reset", "wrong"))
        .expect_err("wrong password should still fail normally after reset");
    assert_eq!(
        err.sqlstate(),
        aiondb_core::SqlState::InvalidAuthorizationSpecification
    );
    cleanup_lockout_state(&state_path);
}
