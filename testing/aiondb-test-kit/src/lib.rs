pub mod assertions;
pub mod embedded;
pub mod fault_injection;
pub mod pgwire;
pub mod scenario;
pub mod semantic;

pub use assertions::{assert_same_outcome, assert_scenario_matches};
pub use embedded::run_embedded;
pub use fault_injection::FaultInjectionHarness;
pub use pgwire::run_pgwire;
pub use scenario::{
    ColumnOutcome, ExecutionBatchOutcome, PortalOutcome, PreparedOutcome, PreparedStatementOutcome,
    ScenarioError, ScenarioExpectation, ScenarioOperation, ScenarioOutcome, ScenarioResult,
    ScenarioValue, SqlScenario, StatementOutcome,
};

#[cfg(test)]
mod tests;
