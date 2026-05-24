use aiondb_core::DbResult;
use aiondb_plan::graph::CypherProcedureCall;

use super::{graph_prealloc_capacity, push_graph_binding, BindingRow, SharedBoundValue};
use crate::executor::ExecutionContext;

pub(super) fn merge_procedure_rows_into_bindings(
    context: &ExecutionContext,
    call: &CypherProcedureCall,
    input_bindings: Vec<BindingRow>,
    procedure_rows: &[Vec<SharedBoundValue>],
) -> DbResult<Vec<BindingRow>> {
    let estimated_rows = input_bindings.len().saturating_mul(procedure_rows.len());
    let mut output = Vec::with_capacity(graph_prealloc_capacity(estimated_rows));
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
        // Process every procedure row but the last with a cloned input, then
        // move `input` directly into the binding produced by the last row —
        // saves one BindingRow clone per outer iteration.
        let mut procedure_iter = procedure_rows.iter();
        let Some(last_row) = procedure_iter.next_back() else {
            continue;
        };
        let apply_row = |binding: &mut BindingRow, procedure_row: &Vec<SharedBoundValue>| {
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
        };
        for procedure_row in procedure_iter {
            context.check_deadline()?;
            let mut binding = input.clone();
            apply_row(&mut binding, procedure_row);
            push_graph_binding(context, &mut output, binding)?;
        }
        context.check_deadline()?;
        let mut binding = input;
        apply_row(&mut binding, last_row);
        push_graph_binding(context, &mut output, binding)?;
    }
    Ok(output)
}
