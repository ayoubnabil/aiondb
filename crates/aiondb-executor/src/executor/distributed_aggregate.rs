//! Distributed aggregation with partial/final phases.
//!
//! **Partial aggregate**: runs on each node, producing intermediate
//! aggregate results (e.g., partial SUM, COUNT per group).
//!
//! **Final aggregate**: merges partial results from all nodes into
//! the final aggregate output with HAVING, ORDER BY, LIMIT/OFFSET.

use aiondb_core::{DbError, DbResult};
use aiondb_plan::{
    AggregateExpr, PhysicalPlan, ProjectionExpr, ResultField, SortExpr, TypedExpr, TypedExprKind,
};

use super::{
    assign_distributed_fragment_targets, DistributedFragment, ExecutionContext, ExecutionResult,
    Executor,
};

// ---------------------------------------------------------------------------
// Partial aggregate - runs on a single node to produce grouped intermediates
// ---------------------------------------------------------------------------

/// Build and execute a partial aggregate plan over the given `source`.
///
/// The partial phase wraps `source` in an `AggregateSource` with the
/// supplied `group_by` and `aggregates`, but **no** HAVING, ORDER BY,
/// or LIMIT - those are deferred to the final phase.
pub(super) fn execute_partial_aggregate_plan(
    executor: &Executor,
    source: &PhysicalPlan,
    group_by: &[TypedExpr],
    aggregates: &[AggregateExpr],
    output_fields: &[ResultField],
    context: &ExecutionContext,
) -> DbResult<ExecutionResult> {
    let aggregate_plan =
        build_partial_aggregate_source(source, group_by, aggregates, output_fields);
    executor.execute(&aggregate_plan, context)
}

/// Construct the `AggregateSource` plan node for the partial phase.
///
/// The output fields are translated into `ProjectionExpr` items using
/// `ColumnRef` expressions pointing at each ordinal position. HAVING,
/// ORDER BY, LIMIT and OFFSET are intentionally omitted - those are
/// applied during the final phase only.
fn build_partial_aggregate_source(
    source: &PhysicalPlan,
    group_by: &[TypedExpr],
    aggregates: &[AggregateExpr],
    output_fields: &[ResultField],
) -> PhysicalPlan {
    if let Some(plan) = build_project_table_partial_aggregate(source, group_by, aggregates) {
        return plan;
    }

    let projection_aggregates = build_identity_projections(output_fields);

    PhysicalPlan::AggregateSource {
        source: Box::new(source.clone()),
        group_by: group_by.to_vec(),
        grouping_sets: Vec::new(),
        aggregates: projection_aggregates,
        having: None,
        filter: None,
        order_by: Vec::new(),
        limit: None,
        offset: None,
        distinct: false,
        distinct_on: Vec::new(),
    }
}

fn build_project_table_partial_aggregate(
    source: &PhysicalPlan,
    group_by: &[TypedExpr],
    aggregates: &[AggregateExpr],
) -> Option<PhysicalPlan> {
    let PhysicalPlan::ProjectTable {
        table_id,
        outputs,
        filter,
        access_path,
        ..
    } = source
    else {
        return None;
    };

    // The distributed optimizer currently builds PartialAggregate sources as
    // ProjectTable nodes that already hold the true aggregate expressions.
    // Running those rows through an AggregateSource pass-through drops
    // aggregate semantics (e.g., COUNT becomes 0/NULL-ish). Rebuild an
    // Aggregate plan over the base table instead.
    let has_aggregate_expr = outputs.iter().any(|projection| {
        matches!(
            projection.expr.kind,
            TypedExprKind::AggCount { .. }
                | TypedExprKind::AggSum { .. }
                | TypedExprKind::AggAvg { .. }
                | TypedExprKind::AggAnyValue { .. }
                | TypedExprKind::AggMin { .. }
                | TypedExprKind::AggMax { .. }
                | TypedExprKind::AggStringAgg { .. }
                | TypedExprKind::AggArrayAgg { .. }
                | TypedExprKind::AggBoolAnd { .. }
                | TypedExprKind::AggBoolOr { .. }
                | TypedExprKind::AggStddevPop { .. }
                | TypedExprKind::AggStddevSamp { .. }
                | TypedExprKind::AggVarPop { .. }
                | TypedExprKind::AggVarSamp { .. }
        )
    });
    if !has_aggregate_expr {
        return None;
    }

    if !aggregates.is_empty() && aggregates.len() != outputs.len() {
        return None;
    }

    Some(PhysicalPlan::Aggregate {
        table_id: *table_id,
        group_by: group_by.to_vec(),
        grouping_sets: Vec::new(),
        aggregates: outputs.clone(),
        having: None,
        filter: filter.clone(),
        order_by: Vec::new(),
        limit: None,
        offset: None,
        distinct: false,
        distinct_on: Vec::new(),
        access_path: access_path.clone(),
    })
}

// ---------------------------------------------------------------------------
// Final aggregate - merges partial results from all nodes
// ---------------------------------------------------------------------------

/// Distribute partial plans across nodes, collect partial results, and
/// re-aggregate them locally with HAVING / ORDER BY / LIMIT / OFFSET.
#[allow(clippy::ref_option)]
pub(super) fn execute_final_aggregate_plan(
    executor: &Executor,
    partials: &[PhysicalPlan],
    group_by: &[TypedExpr],
    _aggregates: &[AggregateExpr],
    having: &Option<TypedExpr>,
    output_fields: &[ResultField],
    order_by: &[SortExpr],
    limit: &Option<TypedExpr>,
    offset: &Option<TypedExpr>,
    context: &ExecutionContext,
) -> DbResult<ExecutionResult> {
    if partials.is_empty() {
        return Ok(ExecutionResult::Query {
            columns: output_fields.to_vec(),
            rows: Vec::new(),
        });
    }

    // 1. Create distributed fragments from each partial plan.
    let worker_count = context.parallel_workers_for(partials.len());
    let use_hash_partitioning = context.distributed_hash_partitioning_enabled();
    let mut fragments: Vec<DistributedFragment> = partials
        .iter()
        .cloned()
        .enumerate()
        .map(|(i, plan)| {
            let fragment = DistributedFragment::local(plan);
            if use_hash_partitioning {
                fragment.with_partition(i, partials.len())
            } else {
                fragment
            }
        })
        .collect();

    // 2. Assign targets (local / remote) across the available workers.
    assign_distributed_fragment_targets(
        &mut fragments,
        worker_count,
        context.distributed_loopback_remote_nodes.as_ref(),
    );

    // 3. Execute all fragments and merge their row sets.
    let distributed = executor.execute_distributed_fragments_targeted(&fragments, context)?;

    let ExecutionResult::Query {
        rows: partial_rows, ..
    } = distributed
    else {
        return Err(DbError::internal(
            "distributed partial aggregate fragments must produce query rows",
        ));
    };

    if partial_rows.is_empty() {
        return Ok(ExecutionResult::Query {
            columns: output_fields.to_vec(),
            rows: Vec::new(),
        });
    }

    // 4. Build a `ProjectValues` plan that materializes all partial rows
    //    so they can be fed into a local re-aggregation.
    // Consume `partial_rows` so each per-row `Value` moves directly into the
    // `TypedExpr::literal` it backs; the previous iter().cloned() path copied
    // every cell on the wire one extra time before re-aggregation.
    let value_rows: Vec<Vec<TypedExpr>> = partial_rows
        .into_iter()
        .map(|row| {
            row.values
                .into_iter()
                .zip(output_fields.iter())
                .map(|(value, field)| {
                    TypedExpr::literal(value, field.data_type.clone(), field.nullable)
                })
                .collect()
        })
        .collect();

    let values_plan = PhysicalPlan::ProjectValues {
        output_fields: output_fields.to_vec(),
        rows: value_rows,
        order_by: Vec::new(),
        limit: None,
        offset: None,
    };

    // 5. Build the final `AggregateSource` over the materialised partial
    //    rows, now including HAVING, ORDER BY, LIMIT and OFFSET.
    let final_aggregates = build_identity_projections(output_fields);
    let final_order_by =
        super::projection_plans::rebase_order_by_to_output_ordinals(&final_aggregates, order_by);

    let final_plan = PhysicalPlan::AggregateSource {
        source: Box::new(values_plan),
        group_by: group_by.to_vec(),
        grouping_sets: Vec::new(),
        aggregates: final_aggregates,
        having: having.clone(),
        filter: None,
        order_by: final_order_by,
        limit: limit.clone(),
        offset: offset.clone(),
        distinct: false,
        distinct_on: Vec::new(),
    };

    executor.execute(&final_plan, context)
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Build identity `ProjectionExpr` items for each output field: each one
/// is a `ColumnRef` that simply passes through the value at its ordinal.
fn build_identity_projections(output_fields: &[ResultField]) -> Vec<ProjectionExpr> {
    output_fields
        .iter()
        .enumerate()
        .map(|(ordinal, field)| ProjectionExpr {
            field: field.clone(),
            expr: TypedExpr::column_ref(
                field.name.clone(),
                ordinal,
                field.data_type.clone(),
                field.nullable,
            ),
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use aiondb_core::DataType;

    fn sample_output_fields() -> Vec<ResultField> {
        vec![
            ResultField {
                name: "x".to_owned(),
                data_type: DataType::Int,
                text_type_modifier: None,
                nullable: false,
            },
            ResultField {
                name: "sum".to_owned(),
                data_type: DataType::BigInt,
                text_type_modifier: None,
                nullable: true,
            },
        ]
    }

    /// Verify that `build_partial_aggregate_source` produces an
    /// `AggregateSource` plan with no HAVING, ORDER BY, LIMIT, or OFFSET.
    #[test]
    fn partial_plan_has_no_finalization_clauses() {
        let source = PhysicalPlan::ProjectValues {
            output_fields: vec![ResultField {
                name: "x".to_owned(),
                data_type: DataType::Int,
                text_type_modifier: None,
                nullable: false,
            }],
            rows: Vec::new(),
            order_by: Vec::new(),
            limit: None,
            offset: None,
        };

        let group_by = vec![TypedExpr::column_ref("x", 0, DataType::Int, false)];
        let output_fields = sample_output_fields();

        let aggregates = Vec::new();
        let plan = build_partial_aggregate_source(&source, &group_by, &aggregates, &output_fields);

        match &plan {
            PhysicalPlan::AggregateSource {
                group_by: gb,
                having,
                order_by,
                limit,
                offset,
                distinct,
                aggregates,
                ..
            } => {
                assert_eq!(gb.len(), 1, "group_by should have one expression");
                assert!(having.is_none(), "partial plan must not have HAVING");
                assert!(order_by.is_empty(), "partial plan must not have ORDER BY");
                assert!(limit.is_none(), "partial plan must not have LIMIT");
                assert!(offset.is_none(), "partial plan must not have OFFSET");
                assert!(!distinct, "partial plan must not be DISTINCT");
                assert_eq!(
                    aggregates.len(),
                    output_fields.len(),
                    "projection count must match output field count"
                );
            }
            _ => panic!("expected AggregateSource plan"),
        }
    }

    /// Verify that the final plan wraps a `ProjectValues` source and
    /// carries the full clause set (HAVING, ORDER BY, LIMIT, OFFSET).
    #[test]
    fn final_plan_wraps_project_values_with_clauses() {
        let output_fields = vec![
            ResultField {
                name: "region".to_owned(),
                data_type: DataType::Text,
                text_type_modifier: None,
                nullable: false,
            },
            ResultField {
                name: "total".to_owned(),
                data_type: DataType::BigInt,
                text_type_modifier: None,
                nullable: true,
            },
        ];

        let group_by = vec![TypedExpr::column_ref("region", 0, DataType::Text, false)];
        let having_expr =
            TypedExpr::literal(aiondb_core::Value::Boolean(true), DataType::Boolean, false);
        let order_by = vec![SortExpr {
            expr: TypedExpr::column_ref("total", 1, DataType::BigInt, true),
            descending: true,
            nulls_first: None,
        }];
        let limit_expr =
            TypedExpr::literal(aiondb_core::Value::BigInt(10), DataType::BigInt, false);

        // Replicate the re-aggregation construction that
        // `execute_final_aggregate_plan` performs after collecting partials.
        let values_plan = PhysicalPlan::ProjectValues {
            output_fields: output_fields.clone(),
            rows: Vec::new(),
            order_by: Vec::new(),
            limit: None,
            offset: None,
        };

        let final_aggregates = build_identity_projections(&output_fields);

        let plan = PhysicalPlan::AggregateSource {
            source: Box::new(values_plan),
            group_by: group_by.clone(),
            grouping_sets: Vec::new(),
            aggregates: final_aggregates,
            having: Some(having_expr),
            filter: None,
            order_by,
            limit: Some(limit_expr),
            offset: None,
            distinct: false,
            distinct_on: Vec::new(),
        };

        match &plan {
            PhysicalPlan::AggregateSource {
                source,
                group_by: gb,
                having,
                order_by: ob,
                limit,
                offset,
                ..
            } => {
                assert!(
                    matches!(source.as_ref(), PhysicalPlan::ProjectValues { .. }),
                    "source must be a ProjectValues plan"
                );
                assert_eq!(gb.len(), 1);
                assert!(having.is_some(), "final plan must include HAVING");
                assert_eq!(ob.len(), 1, "final plan must include ORDER BY");
                assert!(limit.is_some(), "final plan must include LIMIT");
                assert!(offset.is_none(), "offset was not provided");
            }
            _ => panic!("expected AggregateSource plan"),
        }
    }

    /// The identity projections should produce one `ProjectionExpr` per
    /// output field with matching name, data type, and ordinal.
    #[test]
    fn identity_projections_match_output_fields() {
        let fields = sample_output_fields();
        let projections = build_identity_projections(&fields);

        assert_eq!(projections.len(), fields.len());
        for (idx, (proj, field)) in projections.iter().zip(fields.iter()).enumerate() {
            assert_eq!(proj.field.name, field.name);
            assert_eq!(proj.field.data_type, field.data_type);
            assert_eq!(proj.field.nullable, field.nullable);
            match &proj.expr.kind {
                aiondb_plan::TypedExprKind::ColumnRef { name, ordinal } => {
                    assert_eq!(name, &field.name);
                    assert_eq!(*ordinal, idx);
                }
                other => panic!("expected ColumnRef, got {other:?}"),
            }
        }
    }
}
