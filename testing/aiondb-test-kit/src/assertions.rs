use aiondb_engine::DbResult;

use crate::{
    embedded::run_embedded,
    pgwire::run_pgwire,
    scenario::{ScenarioExpectation, ScenarioResult, SqlScenario},
};

pub fn assert_same_outcome(
    scenario: &SqlScenario,
    embedded: &ScenarioResult,
    pgwire: &ScenarioResult,
) {
    assert_eq!(
        embedded, pgwire,
        "embedded and pgwire diverged for scenario '{}'",
        scenario.name
    );
}

pub async fn assert_scenario_matches(scenario: &SqlScenario) -> DbResult<()> {
    let embedded = run_embedded(scenario)?;
    let pgwire = run_pgwire(scenario).await?;
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
