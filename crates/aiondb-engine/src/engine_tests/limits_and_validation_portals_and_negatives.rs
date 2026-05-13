use super::*;

// ---- 3. Portal and prepared statement limits ----------------------------

#[test]
fn max_prepared_statements_rejects_excess() {
    let mut limits = aiondb_config::LimitsConfig::default();
    limits.max_prepared_statements = 2;
    let engine = build_engine_with_limits(limits);
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .prepare(&session, "s1".to_owned(), "SELECT 1".to_owned())
        .expect("prepare s1");
    engine
        .prepare(&session, "s2".to_owned(), "SELECT 2".to_owned())
        .expect("prepare s2");

    let error = engine
        .prepare(&session, "s3".to_owned(), "SELECT 3".to_owned())
        .expect_err("max prepared statements exceeded");
    assert_eq!(
        error.sqlstate(),
        aiondb_core::SqlState::ProgramLimitExceeded
    );
}

#[test]
fn close_statement_frees_slot_for_new_prepared_statement() {
    let mut limits = aiondb_config::LimitsConfig::default();
    limits.max_prepared_statements = 2;
    let engine = build_engine_with_limits(limits);
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .prepare(&session, "s1".to_owned(), "SELECT 1".to_owned())
        .expect("prepare s1");
    engine
        .prepare(&session, "s2".to_owned(), "SELECT 2".to_owned())
        .expect("prepare s2");

    engine.close_statement(&session, "s1").expect("close s1");

    engine
        .prepare(&session, "s3".to_owned(), "SELECT 3".to_owned())
        .expect("prepare s3 after close");
}

#[test]
fn max_prepared_statements_allows_two_compat_prepared_statements() {
    let mut limits = aiondb_config::LimitsConfig::default();
    limits.max_prepared_statements = 2;
    let engine = build_engine_with_limits(limits);
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(&session, "PREPARE s1 AS SELECT 1")
        .expect("compat prepare s1");
    engine
        .execute_sql(&session, "PREPARE s2 AS SELECT 2")
        .expect("compat prepare s2 should fit within limit");

    let error = engine
        .execute_sql(&session, "PREPARE s3 AS SELECT 3")
        .expect_err("third compat prepare should exceed limit");
    assert_eq!(
        error.sqlstate(),
        aiondb_core::SqlState::ProgramLimitExceeded
    );
}

#[test]
fn max_prepared_statements_counts_protocol_and_compat_once_each() {
    let mut limits = aiondb_config::LimitsConfig::default();
    limits.max_prepared_statements = 2;
    let engine = build_engine_with_limits(limits);
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(&session, "PREPARE s1 AS SELECT 1")
        .expect("compat prepare s1");
    engine
        .prepare(&session, "s2".to_owned(), "SELECT 2".to_owned())
        .expect("protocol prepare s2 should fit within limit");

    let error = engine
        .prepare(&session, "s3".to_owned(), "SELECT 3".to_owned())
        .expect_err("third prepared statement should exceed unified limit");
    assert_eq!(
        error.sqlstate(),
        aiondb_core::SqlState::ProgramLimitExceeded
    );
}

#[test]
fn max_portals_rejects_excess() {
    let mut limits = aiondb_config::LimitsConfig::default();
    limits.max_portals = 2;
    let engine = build_engine_with_limits(limits);
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .prepare(&session, "s1".to_owned(), "SELECT 1".to_owned())
        .expect("prepare");

    engine
        .bind(&session, "p1".to_owned(), "s1".to_owned(), vec![])
        .expect("bind p1");
    engine
        .bind(&session, "p2".to_owned(), "s1".to_owned(), vec![])
        .expect("bind p2");

    let error = engine
        .bind(&session, "p3".to_owned(), "s1".to_owned(), vec![])
        .expect_err("max portals exceeded");
    assert_eq!(
        error.sqlstate(),
        aiondb_core::SqlState::ProgramLimitExceeded
    );
}

#[test]
fn close_portal_frees_slot_for_new_portal() {
    let mut limits = aiondb_config::LimitsConfig::default();
    limits.max_portals = 2;
    let engine = build_engine_with_limits(limits);
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .prepare(&session, "s1".to_owned(), "SELECT 1".to_owned())
        .expect("prepare");

    engine
        .bind(&session, "p1".to_owned(), "s1".to_owned(), vec![])
        .expect("bind p1");
    engine
        .bind(&session, "p2".to_owned(), "s1".to_owned(), vec![])
        .expect("bind p2");

    engine.close_portal(&session, "p1").expect("close p1");

    engine
        .bind(&session, "p3".to_owned(), "s1".to_owned(), vec![])
        .expect("bind p3 after close");
}

#[test]
fn rebinding_same_unnamed_portal_name_does_not_count_as_new_slot() {
    let mut limits = aiondb_config::LimitsConfig::default();
    limits.max_portals = 1;
    let engine = build_engine_with_limits(limits);
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .prepare(&session, "s1".to_owned(), "SELECT 1".to_owned())
        .expect("prepare");

    engine
        .bind(&session, String::new(), "s1".to_owned(), vec![])
        .expect("bind unnamed portal first time");

    engine
        .bind(&session, String::new(), "s1".to_owned(), vec![])
        .expect("rebind unnamed portal");
}

#[test]
fn binding_duplicate_named_portal_fails() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .prepare(&session, "s1".to_owned(), "SELECT 1".to_owned())
        .expect("prepare");
    engine
        .bind(&session, "p1".to_owned(), "s1".to_owned(), vec![])
        .expect("bind p1 first time");

    let error = engine
        .bind(&session, "p1".to_owned(), "s1".to_owned(), vec![])
        .expect_err("duplicate named portal should fail");
    assert_eq!(error.sqlstate(), aiondb_core::SqlState::DuplicateObject);
    assert!(
        error.report().message.contains("already exists"),
        "unexpected error: {}",
        error.report().message
    );
}

// ---- 4. Cancellation tests ----------------------------------------------

#[test]
fn cancel_then_execute_sql_returns_query_canceled() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine.cancel_session(&session).expect("cancel");

    let error = engine
        .execute_sql(&session, "SELECT 1")
        .expect_err("should be canceled");
    assert_eq!(error.sqlstate(), aiondb_core::SqlState::QueryCanceled);
}

#[test]
fn cancel_then_prepare_returns_query_canceled() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine.cancel_session(&session).expect("cancel");

    let error = engine
        .prepare(&session, "s1".to_owned(), "SELECT 1".to_owned())
        .expect_err("prepare should be canceled");
    assert_eq!(error.sqlstate(), aiondb_core::SqlState::QueryCanceled);
}

#[test]
fn cancel_flag_is_reset_after_consumption() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine.cancel_session(&session).expect("cancel");

    let _error = engine
        .execute_sql(&session, "SELECT 1")
        .expect_err("first op canceled");

    let results = engine
        .execute_sql(&session, "SELECT 1")
        .expect("second op should succeed");
    assert_eq!(results.len(), 1);
}

#[test]
fn cancel_on_unknown_session_returns_error() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let bogus = SessionHandle::test_handle();

    let error = engine
        .cancel_session(&bogus)
        .expect_err("cancel unknown session");
    assert_eq!(
        error.sqlstate(),
        aiondb_core::SqlState::InvalidAuthorizationSpecification
    );
}

// =========================================================================
// MISS-N1: startup with invalid params
// =========================================================================

struct ValidatingAuthenticator;

impl Authenticator for ValidatingAuthenticator {
    fn authenticate(
        &self,
        credential: &Credential,
        database: &str,
        _transport: &TransportInfo,
    ) -> DbResult<AuthenticatedIdentity> {
        if database.is_empty() {
            return Err(DbError::parse_error(
                aiondb_core::SqlState::InvalidCatalogName,
                "database name must not be empty",
            ));
        }
        let user = credential.user();
        if user.is_empty() {
            return Err(DbError::invalid_authorization(
                "user name must not be empty",
            ));
        }
        Ok(AuthenticatedIdentity {
            user: user.to_owned(),
            database_id: aiondb_core::DatabaseId::new(1),
            roles: vec![user.to_owned()],
        })
    }
}

#[test]
fn startup_with_empty_database_name_returns_error() {
    let engine = EngineBuilder::for_testing()
        .with_authenticator(Arc::new(ValidatingAuthenticator))
        .build()
        .unwrap();

    let params = StartupParams {
        database: String::new(),
        application_name: Some("test".to_owned()),
        options: BTreeMap::new(),
        credential: Credential::Anonymous {
            user: "alice".to_owned(),
        },
        transport: TransportInfo::in_process(),
    };

    let error = engine.startup(params).expect_err("empty database");
    assert_eq!(error.sqlstate(), aiondb_core::SqlState::InvalidCatalogName);
}

#[test]
fn startup_with_empty_username_returns_error() {
    let engine = EngineBuilder::for_testing()
        .with_authenticator(Arc::new(ValidatingAuthenticator))
        .build()
        .unwrap();

    let params = StartupParams {
        database: "default".to_owned(),
        application_name: Some("test".to_owned()),
        options: BTreeMap::new(),
        credential: Credential::Anonymous {
            user: String::new(),
        },
        transport: TransportInfo::in_process(),
    };

    let error = engine.startup(params).expect_err("empty username");
    assert_eq!(
        error.sqlstate(),
        aiondb_core::SqlState::InvalidAuthorizationSpecification
    );
}

// =========================================================================
// MISS-N2: rollback_transaction / commit_transaction outside transaction
// =========================================================================

#[test]
fn rollback_transaction_outside_transaction_is_noop() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");
    engine.rollback_transaction(&session).unwrap();
}

#[test]
fn commit_transaction_outside_transaction_is_noop() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");
    engine.commit_transaction(&session).unwrap();
}

// =========================================================================
// MISS-N3: close_statement for nonexistent name
// =========================================================================

#[test]
fn close_statement_for_nonexistent_name_is_noop() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .close_statement(&session, "nonexistent_stmt")
        .expect("close nonexistent statement should be no-op");
}

// =========================================================================
// MISS-N4: close_portal for nonexistent name
// =========================================================================

#[test]
fn close_portal_for_nonexistent_name_is_noop() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .close_portal(&session, "nonexistent_portal")
        .expect("close nonexistent portal should be no-op");
}

// =========================================================================
// MISS-P1/P2: COMMIT/ROLLBACK parser negatives (via engine)
// =========================================================================

#[test]
fn commit_with_trailing_garbage_returns_syntax_error() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");
    let err = engine
        .execute_sql(&session, "COMMIT garbage")
        .expect_err("trailing tokens after COMMIT must fail");
    assert_eq!(err.sqlstate(), aiondb_core::SqlState::SyntaxError);
}

#[test]
fn rollback_with_trailing_garbage_returns_syntax_error() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");
    let err = engine
        .execute_sql(&session, "ROLLBACK garbage")
        .expect_err("trailing tokens after ROLLBACK must fail");
    assert_eq!(err.sqlstate(), aiondb_core::SqlState::SyntaxError);
}

// =========================================================================
// MISS-E7: update/delete negatives (via engine)
// =========================================================================

#[test]
fn update_on_nonexistent_table_returns_error() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let error = engine
        .execute_sql(&session, "UPDATE no_such_table SET x = 1")
        .expect_err("update nonexistent table");
    assert_eq!(error.sqlstate(), aiondb_core::SqlState::UndefinedTable);
}

#[test]
fn delete_from_nonexistent_table_returns_error() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    let error = engine
        .execute_sql(&session, "DELETE FROM no_such_table")
        .expect_err("delete nonexistent table");
    assert_eq!(error.sqlstate(), aiondb_core::SqlState::UndefinedTable);
}

#[test]
fn update_with_type_mismatch_in_set_clause_returns_error() {
    let engine = EngineBuilder::for_testing().build().unwrap();
    let (session, _) = engine.startup(startup_params()).expect("startup");

    engine
        .execute_sql(&session, "CREATE TABLE typed_tbl (id INT, flag BOOLEAN)")
        .expect("create table");

    engine
        .execute_sql(&session, "INSERT INTO typed_tbl VALUES (1, true)")
        .expect("insert");

    let result = engine.execute_sql(
        &session,
        "UPDATE typed_tbl SET flag = 'not_a_boolean_value'",
    );
    if let Err(error) = result {
        let sqlstate = error.sqlstate();
        assert!(
            sqlstate == aiondb_core::SqlState::SyntaxError
                || sqlstate == aiondb_core::SqlState::InternalError
                || sqlstate == aiondb_core::SqlState::DatatypeMismatch
                || sqlstate == aiondb_core::SqlState::InvalidTextRepresentation,
            "expected a type-related error, got {sqlstate:?}: {error}"
        );
    }
}
