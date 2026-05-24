//! Broadcast hash join execution.
//!
//! The smaller relation (broadcast side) is materialized on the
//! coordinator, then broadcast as a `ProjectValues` plan to each node.
//! Each node joins the broadcast data with its local partition of the
//! larger relation using a regular `HashJoin`.

use aiondb_core::{DbError, DbResult, Row};
use aiondb_plan::{JoinType, PhysicalPlan, ProjectionExpr, ResultField, TypedExpr, TypedExprKind};

use super::{
    assign_distributed_fragment_targets, DistributedFragment, ExecutionContext, ExecutionResult,
    Executor,
};

/// Extract a column ordinal from a `TypedExpr` that is expected to be a
/// `ColumnRef`.  Returns an error when the expression is not a simple
/// column reference.
fn extract_key_ordinal(expr: &TypedExpr) -> DbResult<usize> {
    match &expr.kind {
        TypedExprKind::ColumnRef { ordinal, .. } => Ok(*ordinal),
        _ => Err(DbError::internal(
            "broadcast hash join key is not a simple column reference",
        )),
    }
}

/// Convert a slice of `TypedExpr` column-reference keys into a `Vec<usize>`
/// of ordinals suitable for the `HashJoin` plan node.
fn extract_key_ordinals(keys: &[TypedExpr]) -> DbResult<Vec<usize>> {
    keys.iter().map(extract_key_ordinal).collect()
}

/// Build a `PhysicalPlan::ProjectValues` that emits the given rows as
/// literal values.  Each `Value` in each `Row` is wrapped in a
/// `TypedExpr::literal` using the type information from `output_fields`.
fn build_values_plan(rows: Vec<Row>, output_fields: &[ResultField]) -> PhysicalPlan {
    // Consume the row vector so each `Value` moves directly into its
    // `TypedExpr::literal`; the previous &[Row] signature forced a per-cell
    // clone for every broadcast row.
    let expr_rows: Vec<Vec<TypedExpr>> = rows
        .into_iter()
        .map(|row| {
            row.values
                .into_iter()
                .enumerate()
                .map(|(col_idx, value)| {
                    let field = output_fields.get(col_idx);
                    let data_type = field
                        .map(|f| f.data_type.clone())
                        .or_else(|| value.data_type())
                        .unwrap_or(aiondb_core::DataType::Text);
                    let nullable = field.map_or(true, |f| f.nullable);
                    TypedExpr::literal(value, data_type, nullable)
                })
                .collect()
        })
        .collect();

    PhysicalPlan::ProjectValues {
        output_fields: output_fields.to_vec(),
        rows: expr_rows,
        order_by: Vec::new(),
        limit: None,
        offset: None,
    }
}

/// Determine how many nodes to distribute the join across.
///
/// Uses the configured loopback remote nodes (plus 1 for the local
/// coordinator) when available, otherwise falls back to
/// `max_parallel_workers_per_query`.
fn node_count(context: &ExecutionContext) -> usize {
    let remote_count = context.distributed_loopback_remote_nodes.len();
    if remote_count > 0 {
        remote_count + 1
    } else {
        context.max_parallel_workers_per_query.max(1)
    }
}

pub(super) fn execute_broadcast_hash_join_plan(
    executor: &Executor,
    broadcast: &PhysicalPlan,
    local: &PhysicalPlan,
    join_type: &JoinType,
    left_keys: &[TypedExpr],
    right_keys: &[TypedExpr],
    condition: Option<&TypedExpr>,
    outputs: &[ProjectionExpr],
    output_fields: &[ResultField],
    context: &ExecutionContext,
) -> DbResult<ExecutionResult> {
    // ---------------------------------------------------------------
    // 1. Materialize the broadcast side locally.
    // ---------------------------------------------------------------
    let broadcast_result = executor.execute(broadcast, context)?;
    let (broadcast_columns, broadcast_rows) = match broadcast_result {
        ExecutionResult::Query { columns, rows } => (columns, rows),
        _ => {
            return Err(DbError::internal(
                "broadcast side of broadcast hash join did not produce a query result",
            ));
        }
    };

    // ---------------------------------------------------------------
    // 2. Convert TypedExpr keys to ordinals for HashJoin.
    // ---------------------------------------------------------------
    let left_ordinals = extract_key_ordinals(left_keys)?;
    let right_ordinals = extract_key_ordinals(right_keys)?;

    // ---------------------------------------------------------------
    // 3. Build per-node join fragments.
    // ---------------------------------------------------------------
    let values_plan = build_values_plan(broadcast_rows, &broadcast_columns);
    let nodes = node_count(context);
    let use_hash_partitioning = context.distributed_hash_partitioning_enabled();

    let mut fragments: Vec<DistributedFragment> = (0..nodes)
        .map(|i| {
            let join_plan = PhysicalPlan::HashJoin {
                left: Box::new(values_plan.clone()),
                right: Box::new(local.clone()),
                join_type: *join_type,
                left_keys: left_ordinals.clone(),
                right_keys: right_ordinals.clone(),
                condition: condition.cloned(),
                outputs: outputs.to_vec(),
                filter: None,
                order_by: Vec::new(),
                limit: None,
                offset: None,
                distinct: false,
                distinct_on: Vec::new(),
            };
            let fragment = DistributedFragment::local(join_plan);
            if use_hash_partitioning {
                fragment.with_partition(i, nodes)
            } else {
                fragment
            }
        })
        .collect();

    // ---------------------------------------------------------------
    // 4. Assign fragment targets and execute.
    // ---------------------------------------------------------------
    assign_distributed_fragment_targets(
        &mut fragments,
        nodes,
        &context.distributed_loopback_remote_nodes,
    );

    let result = executor.execute_distributed_fragments_targeted(&fragments, context)?;

    // Attach expected output schema if the merged result is missing one.
    match result {
        ExecutionResult::Query { columns, rows } => {
            let columns = if columns.is_empty() {
                output_fields.to_vec()
            } else {
                columns
            };
            Ok(ExecutionResult::Query { columns, rows })
        }
        other => Ok(other),
    }
}

// =========================================================================
// Tests
// =========================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use aiondb_core::{DataType, Value};

    fn make_output_fields() -> Vec<ResultField> {
        vec![
            ResultField {
                name: "id".to_owned(),
                data_type: DataType::Int,
                text_type_modifier: None,
                nullable: false,
            },
            ResultField {
                name: "name".to_owned(),
                data_type: DataType::Text,
                text_type_modifier: None,
                nullable: true,
            },
        ]
    }

    fn make_broadcast_rows() -> Vec<Row> {
        vec![
            Row::new(vec![Value::Int(1), Value::Text("alice".to_owned())]),
            Row::new(vec![Value::Int(2), Value::Text("bob".to_owned())]),
        ]
    }

    #[test]
    fn build_values_plan_produces_project_values_with_correct_row_count() {
        let fields = make_output_fields();
        let rows = make_broadcast_rows();
        let plan = build_values_plan(rows, &fields);

        match plan {
            PhysicalPlan::ProjectValues {
                output_fields,
                rows: expr_rows,
                order_by,
                limit,
                offset,
            } => {
                assert_eq!(output_fields.len(), 2);
                assert_eq!(expr_rows.len(), 2);
                assert!(order_by.is_empty());
                assert!(limit.is_none());
                assert!(offset.is_none());

                // Verify each row has the right number of columns.
                for row in &expr_rows {
                    assert_eq!(row.len(), 2);
                }

                // Verify the first row's first column is a literal Int(1).
                match &expr_rows[0][0].kind {
                    TypedExprKind::Literal(Value::Int(1)) => {}
                    other => panic!("expected Literal(Int(1)), got {other:?}"),
                }

                // Verify the second row's second column is a literal Text("bob").
                match &expr_rows[1][1].kind {
                    TypedExprKind::Literal(Value::Text(s)) if s == "bob" => {}
                    other => panic!("expected Literal(Text(\"bob\")), got {other:?}"),
                }
            }
            other => panic!("expected ProjectValues, got {other:?}"),
        }
    }

    #[test]
    fn build_values_plan_empty_rows() {
        let fields = make_output_fields();
        let plan = build_values_plan(Vec::new(), &fields);

        match plan {
            PhysicalPlan::ProjectValues { rows, .. } => {
                assert!(rows.is_empty());
            }
            other => panic!("expected ProjectValues, got {other:?}"),
        }
    }

    #[test]
    fn extract_key_ordinals_from_column_refs() {
        let keys = vec![
            TypedExpr::column_ref("id", 0, DataType::Int, false),
            TypedExpr::column_ref("code", 3, DataType::Text, true),
        ];
        let ordinals = extract_key_ordinals(&keys).unwrap();
        assert_eq!(ordinals, vec![0, 3]);
    }

    #[test]
    fn extract_key_ordinals_rejects_non_column_ref() {
        let keys = vec![TypedExpr::literal(Value::Int(42), DataType::Int, false)];
        let result = extract_key_ordinals(&keys);
        assert!(result.is_err());
    }

    #[test]
    fn node_count_defaults_to_one_when_no_remotes() {
        let context = ExecutionContext::default();
        assert_eq!(node_count(&context), 1);
    }

    #[test]
    fn node_count_includes_local_plus_remotes() {
        let context = ExecutionContext::default()
            .with_distributed_loopback_remote_nodes(vec!["node-1".to_owned(), "node-2".to_owned()]);
        assert_eq!(node_count(&context), 3);
    }

    #[test]
    fn join_fragment_structure_is_hash_join_with_values_left() {
        let fields = make_output_fields();
        let rows = make_broadcast_rows();
        let values_plan = build_values_plan(rows, &fields);
        let local_plan = PhysicalPlan::ProjectValues {
            output_fields: fields.clone(),
            rows: Vec::new(),
            order_by: Vec::new(),
            limit: None,
            offset: None,
        };
        let left_ordinals = vec![0_usize];
        let right_ordinals = vec![0_usize];
        let outputs: Vec<ProjectionExpr> = Vec::new();

        let join_plan = PhysicalPlan::HashJoin {
            left: Box::new(values_plan),
            right: Box::new(local_plan),
            join_type: JoinType::Inner,
            left_keys: left_ordinals,
            right_keys: right_ordinals,
            condition: None,
            outputs,
            filter: None,
            order_by: Vec::new(),
            limit: None,
            offset: None,
            distinct: false,
            distinct_on: Vec::new(),
        };

        // Verify the structure: a HashJoin whose left child is ProjectValues.
        match &join_plan {
            PhysicalPlan::HashJoin {
                left,
                right,
                join_type,
                left_keys,
                right_keys,
                filter,
                order_by,
                limit,
                offset,
                distinct,
                distinct_on,
                ..
            } => {
                assert!(matches!(left.as_ref(), PhysicalPlan::ProjectValues { .. }));
                assert!(matches!(right.as_ref(), PhysicalPlan::ProjectValues { .. }));
                assert_eq!(*join_type, JoinType::Inner);
                assert_eq!(left_keys, &[0]);
                assert_eq!(right_keys, &[0]);
                assert!(filter.is_none());
                assert!(order_by.is_empty());
                assert!(limit.is_none());
                assert!(offset.is_none());
                assert!(!distinct);
                assert!(distinct_on.is_empty());
            }
            other => panic!("expected HashJoin, got {other:?}"),
        }
    }

    #[test]
    fn fragment_targets_assigned_correctly() {
        let fields = make_output_fields();
        let rows = make_broadcast_rows();
        let values_plan = build_values_plan(rows, &fields);
        let local_plan = PhysicalPlan::ProjectValues {
            output_fields: fields.clone(),
            rows: Vec::new(),
            order_by: Vec::new(),
            limit: None,
            offset: None,
        };

        let fragment_count = 3;
        let remote_nodes = vec!["node-1".to_owned(), "node-2".to_owned()];

        let mut fragments: Vec<DistributedFragment> = (0..fragment_count)
            .map(|i| {
                let join_plan = PhysicalPlan::HashJoin {
                    left: Box::new(values_plan.clone()),
                    right: Box::new(local_plan.clone()),
                    join_type: JoinType::Inner,
                    left_keys: vec![0],
                    right_keys: vec![0],
                    condition: None,
                    outputs: Vec::new(),
                    filter: None,
                    order_by: Vec::new(),
                    limit: None,
                    offset: None,
                    distinct: false,
                    distinct_on: Vec::new(),
                };
                DistributedFragment::local(join_plan).with_partition(i, fragment_count)
            })
            .collect();

        assign_distributed_fragment_targets(&mut fragments, fragment_count, &remote_nodes);

        assert_eq!(fragments.len(), 3);
        assert_eq!(fragments[0].target, super::super::FragmentTarget::Local,);
        assert_eq!(
            fragments[1].target,
            super::super::FragmentTarget::Remote("node-1".to_owned()),
        );
        assert_eq!(
            fragments[2].target,
            super::super::FragmentTarget::Remote("node-2".to_owned()),
        );

        // Verify each fragment has the correct hash partition assigned.
        for (i, fragment) in fragments.iter().enumerate() {
            let partition = fragment
                .partition
                .as_ref()
                .unwrap_or_else(|| panic!("fragment {i} should have a partition set"));
            assert_eq!(partition.index, i, "fragment {i} partition index mismatch");
            assert_eq!(
                partition.count, fragment_count,
                "fragment {i} partition count mismatch"
            );
        }
    }
}
