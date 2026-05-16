//! Native Cypher `CALL graph.*` procedure execution.

use aiondb_core::DbResult;
use aiondb_plan::graph::CypherProcedureCall;

use super::graph_procedure_render::merge_procedure_rows_into_bindings;
use super::graph_procedure_results::procedure_result_bindings;
use super::BindingRow;
use crate::executor::{ExecutionContext, Executor};

impl Executor {
    pub(super) fn execute_cypher_procedure_call(
        &self,
        context: &ExecutionContext,
        call: &CypherProcedureCall,
        input_bindings: Vec<BindingRow>,
    ) -> DbResult<Vec<BindingRow>> {
        let (input, prebuilt_weighted_edges) =
            self.build_current_graph_algorithm_runtime_entry_for_call(context, call)?;
        let config = self.prepare_current_graph_algorithm_config_with_prebuilt_weighted_edges(
            context,
            call,
            &input,
            prebuilt_weighted_edges,
        )?;
        let results = self.execute_current_graph_algorithm(&input, &call.procedure, &config)?;
        let procedure_rows =
            procedure_result_bindings(&call.procedure, &call.yields, &results, &input.node_ids)?;

        merge_procedure_rows_into_bindings(context, call, input_bindings, &procedure_rows)
    }
}
