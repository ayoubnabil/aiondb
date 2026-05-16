use aiondb_core::DbResult;
use aiondb_plan::graph::CypherProcedureCall;

use super::{push_graph_binding, BindingRow, SharedBoundValue};
use crate::executor::ExecutionContext;

pub(super) fn merge_procedure_rows_into_bindings(
    context: &ExecutionContext,
    call: &CypherProcedureCall,
    input_bindings: Vec<BindingRow>,
    procedure_rows: &[Vec<SharedBoundValue>],
) -> DbResult<Vec<BindingRow>> {
    let mut output = Vec::with_capacity(input_bindings.len().saturating_mul(procedure_rows.len()));
    for input in input_bindings {
        let yield_slots = call
            .yields
            .iter()
            .map(|name| {
                input
                    .entries
                    .iter()
                    .position(|(entry_name, _)| entry_name == name)
            })
            .collect::<Vec<_>>();
        let missing_yields = yield_slots.iter().filter(|slot| slot.is_none()).count();
        for procedure_row in procedure_rows {
            context.check_deadline()?;
            let mut binding = input.clone();
            if missing_yields > 0 {
                binding.entries.reserve(missing_yields);
            }
            for ((slot, name), value) in yield_slots
                .iter()
                .zip(call.yields.iter())
                .zip(procedure_row.iter())
            {
                if let Some(slot) = slot {
                    binding.entries[*slot].1 = value.clone();
                } else {
                    binding.entries.push((name.clone(), value.clone()));
                }
            }
            push_graph_binding(context, &mut output, binding)?;
        }
    }
    Ok(output)
}
