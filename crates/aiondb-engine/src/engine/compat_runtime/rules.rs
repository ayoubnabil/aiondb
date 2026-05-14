//! `compat/rules.rs`: remaining typed compat dispatch for user operators.

use super::router_helpers::CompatHandlerPlan;
use super::*;

impl Engine {
    /// ADR-0004 typed dispatcher: CREATE/DROP OPERATOR.
    ///
    /// Runtime evaluation of user-defined operators is not implemented.
    /// Reject explicitly instead of pretending success via compat registries.
    pub(in crate::engine) fn execute_compat_operator_command(
        &self,
        command: TypedCompatCommand,
        _session: &SessionHandle,
        _statement_sql: &str,
        _statement: &Statement,
    ) -> DbResult<CompatHandlerPlan> {
        match command {
            TypedCompatCommand::CreateOperator => {
                Err(super::unsupported_compat_command("CREATE OPERATOR"))
            }
            TypedCompatCommand::DropOperator => {
                Err(super::unsupported_compat_command("DROP OPERATOR"))
            }
            _ => Ok(CompatHandlerPlan::unhandled()),
        }
    }
}
