use std::sync::Arc;

use aiondb_catalog::CatalogReader;
use aiondb_core::{DataType, DbResult, TxnId, Value};
use aiondb_plan::{LogicalPlan, ResultField};

use super::{list_user_tables, query_helpers::rows_to_typed};

pub(super) fn triggers_output_fields() -> Vec<ResultField> {
    vec![
        ResultField {
            name: "trigger_catalog".to_owned(),
            data_type: DataType::Text,
            text_type_modifier: None,
            nullable: true,
        },
        ResultField {
            name: "trigger_schema".to_owned(),
            data_type: DataType::Text,
            text_type_modifier: None,
            nullable: true,
        },
        ResultField {
            name: "trigger_name".to_owned(),
            data_type: DataType::Text,
            text_type_modifier: None,
            nullable: false,
        },
        ResultField {
            name: "event_manipulation".to_owned(),
            data_type: DataType::Text,
            text_type_modifier: None,
            nullable: false,
        },
        ResultField {
            name: "event_object_catalog".to_owned(),
            data_type: DataType::Text,
            text_type_modifier: None,
            nullable: true,
        },
        ResultField {
            name: "event_object_schema".to_owned(),
            data_type: DataType::Text,
            text_type_modifier: None,
            nullable: false,
        },
        ResultField {
            name: "event_object_table".to_owned(),
            data_type: DataType::Text,
            text_type_modifier: None,
            nullable: false,
        },
        ResultField {
            name: "action_order".to_owned(),
            data_type: DataType::Int,
            text_type_modifier: None,
            nullable: false,
        },
        ResultField {
            name: "action_condition".to_owned(),
            data_type: DataType::Text,
            text_type_modifier: None,
            nullable: true,
        },
        ResultField {
            name: "action_statement".to_owned(),
            data_type: DataType::Text,
            text_type_modifier: None,
            nullable: false,
        },
        ResultField {
            name: "action_orientation".to_owned(),
            data_type: DataType::Text,
            text_type_modifier: None,
            nullable: false,
        },
        ResultField {
            name: "action_timing".to_owned(),
            data_type: DataType::Text,
            text_type_modifier: None,
            nullable: false,
        },
        ResultField {
            name: "action_reference_old_table".to_owned(),
            data_type: DataType::Text,
            text_type_modifier: None,
            nullable: true,
        },
        ResultField {
            name: "action_reference_new_table".to_owned(),
            data_type: DataType::Text,
            text_type_modifier: None,
            nullable: true,
        },
        ResultField {
            name: "action_reference_old_row".to_owned(),
            data_type: DataType::Text,
            text_type_modifier: None,
            nullable: true,
        },
        ResultField {
            name: "action_reference_new_row".to_owned(),
            data_type: DataType::Text,
            text_type_modifier: None,
            nullable: true,
        },
        ResultField {
            name: "created".to_owned(),
            data_type: DataType::Text,
            text_type_modifier: None,
            nullable: true,
        },
    ]
}

pub(super) fn build_triggers_rows(
    catalog: &Arc<dyn CatalogReader>,
    txn_id: TxnId,
    default_schema: Option<&str>,
    database_name: Option<&str>,
) -> DbResult<Vec<Vec<Value>>> {
    use aiondb_catalog::{TriggerEventDescriptor, TriggerTimingDescriptor};

    let db_name = database_name.unwrap_or("aiondb");
    let tables = list_user_tables(catalog, txn_id, default_schema)?;

    let mut raw_rows: Vec<Vec<Value>> = Vec::new();

    for table in &tables {
        let schema_name = table.name.schema.as_deref().unwrap_or("public");
        let table_name = table.name.object_name();
        let mut triggers = catalog.list_triggers(txn_id, &table.name.to_string())?;
        // PostgreSQL fires (and reports) triggers alphabetically by name within
        // a given (table, event, timing) group. action_order is the 1-based
        // index of the trigger inside that group, so we sort by name first
        // before assigning orders - otherwise creation order leaks through and
        // the information_schema view diverges from PG.
        triggers.sort_by(|a, b| a.name.cmp(&b.name));

        // Group triggers by name to assign action_order per event
        let mut order_map: std::collections::HashMap<(String, String, String), i32> =
            std::collections::HashMap::new();

        for trigger in &triggers {
            let timing_str = match trigger.timing {
                TriggerTimingDescriptor::Before => "BEFORE",
                TriggerTimingDescriptor::After => "AFTER",
                TriggerTimingDescriptor::InsteadOf => "INSTEAD OF",
            };
            let orientation = if trigger.for_each_row {
                "ROW"
            } else {
                "STATEMENT"
            };
            let action_stmt = if trigger.function_args.is_empty() {
                format!("EXECUTE FUNCTION {}()", trigger.function_name)
            } else {
                let args_str: Vec<String> = trigger
                    .function_args
                    .iter()
                    .map(|a| format!("'{}'", a.replace('\'', "''")))
                    .collect();
                format!(
                    "EXECUTE FUNCTION {}({})",
                    trigger.function_name,
                    args_str.join(", ")
                )
            };

            // Emit one row per event (primary + extra_events)
            let all_events = std::iter::once(&trigger.event).chain(trigger.extra_events.iter());
            for evt in all_events {
                let event_str = match evt {
                    TriggerEventDescriptor::Insert => "INSERT",
                    TriggerEventDescriptor::Update => "UPDATE",
                    TriggerEventDescriptor::Delete => "DELETE",
                };

                let key = (
                    event_str.to_owned(),
                    timing_str.to_owned(),
                    orientation.to_owned(),
                );
                let order = order_map.entry(key).or_insert(0);
                *order += 1;

                raw_rows.push(vec![
                    Value::Text(db_name.to_owned()),     // trigger_catalog
                    Value::Text(schema_name.to_owned()), // trigger_schema
                    Value::Text(trigger.name.clone()),   // trigger_name
                    Value::Text(event_str.to_owned()),   // event_manipulation
                    Value::Text(db_name.to_owned()),     // event_object_catalog
                    Value::Text(schema_name.to_owned()), // event_object_schema
                    Value::Text(table_name.to_owned()),  // event_object_table
                    Value::Int(*order),                  // action_order
                    Value::Null,                         // action_condition
                    Value::Text(action_stmt.clone()),    // action_statement
                    Value::Text(orientation.to_owned()), // action_orientation
                    Value::Text(timing_str.to_owned()),  // action_timing
                    Value::Null,                         // action_reference_old_table
                    Value::Null,                         // action_reference_new_table
                    Value::Null,                         // action_reference_old_row
                    Value::Null,                         // action_reference_new_row
                    Value::Null,                         // created
                ]);
            }
        }
    }

    Ok(raw_rows)
}

#[cfg_attr(not(test), allow(dead_code))]
pub(super) fn build_triggers_plan(
    catalog: &Arc<dyn CatalogReader>,
    txn_id: TxnId,
    default_schema: Option<&str>,
    database_name: Option<&str>,
) -> DbResult<LogicalPlan> {
    let output_fields = triggers_output_fields();
    let raw_rows = build_triggers_rows(catalog, txn_id, default_schema, database_name)?;
    let rows = rows_to_typed(&output_fields, raw_rows);

    Ok(LogicalPlan::ProjectValues {
        output_fields,
        rows,
        order_by: Vec::new(),
        limit: None,
        offset: None,
    })
}
