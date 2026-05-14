use std::sync::Arc;

use aiondb_core::DatabaseId;
use aiondb_engine::{
    AuthenticatedIdentity, Authenticator, Credential, DbError, DbResult, EngineBuilder,
    TransportInfo,
};

use super::{
    assert_same_outcome, assert_scenario_matches, embedded::run_embedded_with_engine,
    pgwire::run_pgwire_with_engine, ScenarioExpectation, ScenarioResult, ScenarioValue,
    SqlScenario,
};

#[derive(Debug)]
struct FixedPasswordAuthenticator;

impl Authenticator for FixedPasswordAuthenticator {
    fn authenticate(
        &self,
        credential: &Credential,
        _database: &str,
        _transport: &TransportInfo,
    ) -> DbResult<AuthenticatedIdentity> {
        match credential {
            Credential::CleartextPassword { user, password }
                if user == "alice" && password.as_str() == "s3cret" =>
            {
                Ok(AuthenticatedIdentity {
                    user: user.clone(),
                    database_id: DatabaseId::new(1),
                    roles: vec![user.clone()],
                })
            }
            Credential::CleartextPassword { .. } => Err(DbError::invalid_authorization(
                "invalid user name or password",
            )),
            Credential::Anonymous { .. } | Credential::Token { .. } => Err(
                DbError::invalid_authorization("cleartext password required"),
            ),
            _ => Err(DbError::invalid_authorization(
                "unsupported credential type",
            )),
        }
    }
}

fn build_password_engine() -> Arc<aiondb_engine::Engine> {
    Arc::new(
        EngineBuilder::for_testing()
            .with_authenticator(Arc::new(FixedPasswordAuthenticator))
            .build()
            .unwrap(),
    )
}

async fn assert_scenario_matches_with_password_auth(scenario: &SqlScenario) -> DbResult<()> {
    let embedded = run_embedded_with_engine(build_password_engine(), scenario)?;
    let pgwire = run_pgwire_with_engine(build_password_engine(), scenario).await?;
    assert_same_outcome(scenario, &embedded, &pgwire);
    match (scenario.expectation, &embedded) {
        (ScenarioExpectation::Success, ScenarioResult::Success(_))
        | (ScenarioExpectation::Error, ScenarioResult::Error(_)) => {}
        (ScenarioExpectation::Success, ScenarioResult::Error(error)) => {
            panic!(
                "scenario '{}' unexpectedly failed with {}: {}",
                scenario.name, error.sqlstate, error.message
            );
        }
        (ScenarioExpectation::Error, ScenarioResult::Success(_)) => {
            panic!("scenario '{}' unexpectedly succeeded", scenario.name);
        }
    }
    Ok(())
}

mod aggregates_and_windows;
mod baseline;
mod ddl;
mod ddl_advanced;
mod dml;
mod errors_and_prepared;
mod expressions;
mod joins_advanced;
mod joins_and_explain;
mod limits;
mod prepared_advanced;
mod security;
mod sql_edge_cases;
mod stress_and_stability;
mod subqueries_and_ctes;
mod system_catalog;
mod transactions;
mod transactions_advanced;
mod type_system;
