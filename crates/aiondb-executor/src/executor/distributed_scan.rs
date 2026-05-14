//! Distributed scan execution.
//!
//! Fans out a table scan across multiple cluster nodes by creating
//! per-node `ProjectTable` fragments and executing them in parallel
//! via the fragment dispatcher.

use aiondb_core::{DbResult, RelationId};
use aiondb_plan::{PhysicalPlan, ProjectionExpr, ResultField, ScanAccessPath, TypedExpr};

use super::{DistributedFragment, ExecutionContext, ExecutionResult, Executor};

#[allow(clippy::ref_option)]
pub(super) fn execute_distributed_scan_plan(
    executor: &Executor,
    table_id: RelationId,
    outputs: &[ProjectionExpr],
    filter: &Option<TypedExpr>,
    output_fields: &[ResultField],
    node_count: usize,
    context: &ExecutionContext,
) -> DbResult<ExecutionResult> {
    if context.distributed_loopback_remote_nodes.is_empty() {
        return executor.execute(&build_scan_fragment(table_id, outputs, filter), context);
    }

    let use_hash_partitioning = context.distributed_hash_partitioning_enabled();
    let mut fragments: Vec<DistributedFragment> = (0..node_count)
        .map(|i| {
            let fragment =
                DistributedFragment::local(build_scan_fragment(table_id, outputs, filter));
            if use_hash_partitioning {
                fragment.with_partition(i, node_count)
            } else {
                fragment
            }
        })
        .collect();

    super::assign_distributed_fragment_targets(
        &mut fragments,
        node_count,
        &context.distributed_loopback_remote_nodes,
    );

    let result = executor.execute_distributed_fragments_targeted(&fragments, context)?;

    // If the merged result already carries an output schema, return it
    // directly. Otherwise, attach the expected output fields so the
    // caller receives a well-formed `Query` result.
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

#[allow(clippy::ref_option)]
fn build_scan_fragment(
    table_id: RelationId,
    outputs: &[ProjectionExpr],
    filter: &Option<TypedExpr>,
) -> PhysicalPlan {
    PhysicalPlan::ProjectTable {
        table_id,
        outputs: outputs.to_vec(),
        filter: filter.clone(),
        order_by: Vec::new(),
        limit: None,
        offset: None,
        distinct: false,
        distinct_on: Vec::new(),
        access_path: ScanAccessPath::SeqScan,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fragment_count_matches_node_count() {
        let table_id = RelationId::new(1);
        let outputs: Vec<ProjectionExpr> = Vec::new();
        let filter: Option<TypedExpr> = None;

        for node_count in [1, 3, 8, 16] {
            let fragments: Vec<DistributedFragment> = (0..node_count)
                .map(|i| {
                    DistributedFragment::local(build_scan_fragment(table_id, &outputs, &filter))
                        .with_partition(i, node_count)
                })
                .collect();

            assert_eq!(
                fragments.len(),
                node_count,
                "expected {node_count} fragments, got {}",
                fragments.len()
            );
        }
    }

    #[test]
    fn fragments_have_distinct_partitions() {
        let table_id = RelationId::new(1);
        let outputs: Vec<ProjectionExpr> = Vec::new();
        let filter: Option<TypedExpr> = None;

        for node_count in [2, 4, 7, 16] {
            let fragments: Vec<DistributedFragment> = (0..node_count)
                .map(|i| {
                    DistributedFragment::local(build_scan_fragment(table_id, &outputs, &filter))
                        .with_partition(i, node_count)
                })
                .collect();

            let mut seen_indices = std::collections::HashSet::new();
            for (i, frag) in fragments.iter().enumerate() {
                let partition = frag
                    .partition
                    .as_ref()
                    .unwrap_or_else(|| panic!("fragment {i} has no partition set"));

                assert_eq!(
                    partition.count, node_count,
                    "fragment {i}: expected partition count {node_count}, got {}",
                    partition.count
                );

                assert!(
                    seen_indices.insert(partition.index),
                    "fragment {i}: duplicate partition index {}",
                    partition.index
                );
            }

            assert_eq!(
                seen_indices.len(),
                node_count,
                "expected {node_count} distinct partition indices, got {}",
                seen_indices.len()
            );
        }
    }

    #[test]
    fn build_scan_fragment_produces_seq_scan_project_table() {
        let table_id = RelationId::new(42);
        let outputs: Vec<ProjectionExpr> = Vec::new();
        let filter: Option<TypedExpr> = None;

        let plan = build_scan_fragment(table_id, &outputs, &filter);

        match plan {
            PhysicalPlan::ProjectTable {
                table_id: tid,
                outputs: o,
                filter: f,
                order_by,
                limit,
                offset,
                distinct,
                distinct_on,
                access_path,
            } => {
                assert_eq!(tid, table_id);
                assert!(o.is_empty());
                assert!(f.is_none());
                assert!(order_by.is_empty());
                assert!(limit.is_none());
                assert!(offset.is_none());
                assert!(!distinct);
                assert!(distinct_on.is_empty());
                assert_eq!(access_path, ScanAccessPath::SeqScan);
            }
            other => panic!("expected ProjectTable, got {other:?}"),
        }
    }
}
