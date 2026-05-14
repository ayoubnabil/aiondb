//! Optimizer rules for distributed query execution.
//!
//! When the cluster has multiple nodes, these rules transform local
//! query plans into distributed variants:
//! - Table scans -> distributed scans (fan-out across nodes)
//! - Aggregates -> partial/final aggregates (map-reduce)
//! - Hash joins -> broadcast hash joins (small side broadcast)

use aiondb_plan::{AggregateExpr, PhysicalPlan, ResultField, ScanAccessPath, TypedExpr};

/// Return `true` when the plan is a simple small-side node that is
/// suitable for broadcasting (e.g. `ProjectOnce`, `ProjectValues`,
/// or a `ProjectSource` over a small plan / with a LIMIT).
fn is_small_plan(plan: &PhysicalPlan) -> bool {
    match plan {
        PhysicalPlan::ProjectOnce { .. } | PhysicalPlan::ProjectValues { .. } => true,
        PhysicalPlan::ProjectSource { source, limit, .. } => {
            // A ProjectSource over a small plan, or with a small LIMIT
            is_small_plan(source) || limit.is_some()
        }
        _ => false,
    }
}

/// Extract output fields from a plan node's own outputs.
fn plan_output_fields(plan: &PhysicalPlan) -> Vec<ResultField> {
    plan.output_fields()
}

/// Rewrite a `ProjectTable` plan as a `DistributedScan` when multiple nodes
/// are available and the scan uses a sequential access path.
pub fn try_distribute_scan(plan: &PhysicalPlan, node_count: usize) -> Option<PhysicalPlan> {
    if node_count <= 1 {
        return None;
    }

    if let PhysicalPlan::ProjectTable {
        table_id,
        outputs,
        filter,
        order_by,
        limit,
        offset,
        distinct,
        distinct_on,
        access_path: ScanAccessPath::SeqScan,
        ..
    } = plan
    {
        if !order_by.is_empty()
            || limit.is_some()
            || offset.is_some()
            || *distinct
            || !distinct_on.is_empty()
        {
            return None;
        }
        // Identity scans (no explicit output projections) emit full storage
        // rows whose column count is only known at execution time. The
        // `DistributedScan` form materialises `output_fields` eagerly, so
        // distributing an identity scan would erase that width information
        // and break downstream operators that read it (e.g. equi-join key
        // validation and aggregate result metadata). Leave the original
        // `ProjectTable` in place for those cases.
        if outputs.is_empty() {
            return None;
        }

        let output_fields: Vec<ResultField> =
            outputs.iter().map(|output| output.field.clone()).collect();
        return Some(PhysicalPlan::DistributedScan {
            table_id: *table_id,
            outputs: outputs.clone(),
            filter: filter.clone(),
            output_fields,
            node_count,
        });
    }

    None
}

/// Rewrite an `Aggregate` plan as `FinalAggregate` over `PartialAggregate`
/// fragments when multiple nodes are available.
pub fn try_distribute_aggregate(plan: &PhysicalPlan, node_count: usize) -> Option<PhysicalPlan> {
    if node_count <= 1 {
        return None;
    }

    if let PhysicalPlan::Aggregate {
        table_id,
        group_by,
        aggregates,
        having,
        filter,
        order_by,
        limit,
        offset,
        ..
    } = plan
    {
        let aggregate_exprs: Vec<AggregateExpr> = aggregates
            .iter()
            .map(|agg| AggregateExpr {
                name: agg.field.name.clone(),
            })
            .collect();

        let output_fields: Vec<ResultField> =
            aggregates.iter().map(|agg| agg.field.clone()).collect();

        let partials: Vec<PhysicalPlan> = (0..node_count)
            .map(|_| PhysicalPlan::PartialAggregate {
                source: Box::new(PhysicalPlan::ProjectTable {
                    table_id: *table_id,
                    outputs: aggregates.clone(),
                    filter: filter.clone(),
                    order_by: Vec::new(),
                    limit: None,
                    offset: None,
                    distinct: false,
                    distinct_on: Vec::new(),
                    access_path: ScanAccessPath::SeqScan,
                }),
                group_by: group_by.clone(),
                aggregates: aggregate_exprs.clone(),
                output_fields: output_fields.clone(),
            })
            .collect();

        return Some(PhysicalPlan::FinalAggregate {
            partials,
            group_by: group_by.clone(),
            aggregates: aggregate_exprs,
            having: having.clone(),
            output_fields,
            order_by: order_by.clone(),
            limit: limit.clone(),
            offset: offset.clone(),
        });
    }

    None
}

/// Rewrite a `HashJoin` as a `BroadcastHashJoin` when one side is
/// significantly smaller. The smaller side is broadcast to all nodes.
///
/// Heuristic: if the left side is a simple `ProjectOnce`/`ProjectValues`
/// (likely small), broadcast it; otherwise if right is simple, broadcast right.
pub fn try_distribute_hash_join(plan: &PhysicalPlan, node_count: usize) -> Option<PhysicalPlan> {
    if node_count <= 1 {
        return None;
    }

    if let PhysicalPlan::HashJoin {
        left,
        right,
        join_type,
        left_keys,
        right_keys,
        condition,
        outputs,
        ..
    } = plan
    {
        let left_small = is_small_plan(left);
        let right_small = is_small_plan(right);

        let (broadcast, local) = if left_small {
            (left, right)
        } else if right_small {
            (right, left)
        } else {
            return None;
        };

        let output_fields: Vec<ResultField> =
            outputs.iter().map(|output| output.field.clone()).collect();

        let left_fields = plan_output_fields(left);
        let right_fields = plan_output_fields(right);

        let typed_left_keys: Vec<TypedExpr> = left_keys
            .iter()
            .map(|&ordinal| {
                let (data_type, nullable) = left_fields
                    .get(ordinal)
                    .map_or((aiondb_core::DataType::Int, false), |f| {
                        (f.data_type.clone(), f.nullable)
                    });
                TypedExpr::column_ref(format!("key_{ordinal}"), ordinal, data_type, nullable)
            })
            .collect();

        let typed_right_keys: Vec<TypedExpr> = right_keys
            .iter()
            .map(|&ordinal| {
                let (data_type, nullable) = right_fields
                    .get(ordinal)
                    .map_or((aiondb_core::DataType::Int, false), |f| {
                        (f.data_type.clone(), f.nullable)
                    });
                TypedExpr::column_ref(format!("key_{ordinal}"), ordinal, data_type, nullable)
            })
            .collect();

        return Some(PhysicalPlan::BroadcastHashJoin {
            broadcast: broadcast.clone(),
            local: local.clone(),
            join_type: *join_type,
            left_keys: typed_left_keys,
            right_keys: typed_right_keys,
            condition: condition.clone(),
            outputs: outputs.clone(),
            output_fields,
        });
    }

    None
}

/// Apply all distribution rules to a plan tree recursively.
/// Returns the original plan unchanged if no rule applies.
pub fn distribute_plan(plan: &PhysicalPlan, node_count: usize) -> PhysicalPlan {
    distribute_plan_with_partial_aggregates(plan, node_count, true)
}

/// Apply distribution rules while allowing the execution layer to disable
/// partial/final aggregate rewrites for topologies where the current aggregate
/// merge protocol is not yet safe.
pub fn distribute_plan_with_partial_aggregates(
    plan: &PhysicalPlan,
    node_count: usize,
    allow_partial_aggregates: bool,
) -> PhysicalPlan {
    if node_count <= 1 {
        return plan.clone();
    }

    if let Some(distributed) = try_distribute_scan(plan, node_count) {
        return distributed;
    }
    if allow_partial_aggregates {
        if let Some(distributed) = try_distribute_aggregate(plan, node_count) {
            return distributed;
        }
    }
    if let Some(distributed) = try_distribute_hash_join(plan, node_count) {
        return distributed;
    }

    // Recurse into wrapper plan nodes.
    match plan {
        PhysicalPlan::ProjectSource {
            source,
            outputs,
            filter,
            order_by,
            limit,
            offset,
            distinct,
            distinct_on,
        } => {
            let distributed_source = distribute_plan_with_partial_aggregates(
                source,
                node_count,
                allow_partial_aggregates,
            );
            if distributed_source != **source {
                return PhysicalPlan::ProjectSource {
                    source: Box::new(distributed_source),
                    outputs: outputs.clone(),
                    filter: filter.clone(),
                    order_by: order_by.clone(),
                    limit: limit.clone(),
                    offset: offset.clone(),
                    distinct: *distinct,
                    distinct_on: distinct_on.clone(),
                };
            }
        }
        PhysicalPlan::AggregateSource {
            source,
            group_by,
            grouping_sets,
            aggregates,
            having,
            filter,
            order_by,
            limit,
            offset,
            distinct,
            distinct_on,
        } => {
            let distributed_source = distribute_plan_with_partial_aggregates(
                source,
                node_count,
                allow_partial_aggregates,
            );
            if distributed_source != **source {
                return PhysicalPlan::AggregateSource {
                    source: Box::new(distributed_source),
                    group_by: group_by.clone(),
                    grouping_sets: grouping_sets.clone(),
                    aggregates: aggregates.clone(),
                    having: having.clone(),
                    filter: filter.clone(),
                    order_by: order_by.clone(),
                    limit: limit.clone(),
                    offset: offset.clone(),
                    distinct: *distinct,
                    distinct_on: distinct_on.clone(),
                };
            }
        }
        PhysicalPlan::HashJoin {
            left,
            right,
            join_type,
            left_keys,
            right_keys,
            condition,
            outputs,
            filter,
            order_by,
            limit,
            offset,
            distinct,
            distinct_on,
        } => {
            let dist_left =
                distribute_plan_with_partial_aggregates(left, node_count, allow_partial_aggregates);
            let dist_right = distribute_plan_with_partial_aggregates(
                right,
                node_count,
                allow_partial_aggregates,
            );
            if dist_left != **left || dist_right != **right {
                return PhysicalPlan::HashJoin {
                    left: Box::new(dist_left),
                    right: Box::new(dist_right),
                    join_type: *join_type,
                    left_keys: left_keys.clone(),
                    right_keys: right_keys.clone(),
                    condition: condition.clone(),
                    outputs: outputs.clone(),
                    filter: filter.clone(),
                    order_by: order_by.clone(),
                    limit: limit.clone(),
                    offset: offset.clone(),
                    distinct: *distinct,
                    distinct_on: distinct_on.clone(),
                };
            }
        }
        PhysicalPlan::NestedLoopJoin {
            left,
            right,
            join_type,
            condition,
            outputs,
            filter,
            order_by,
            limit,
            offset,
            distinct,
            distinct_on,
        } => {
            let dist_left =
                distribute_plan_with_partial_aggregates(left, node_count, allow_partial_aggregates);
            let dist_right = distribute_plan_with_partial_aggregates(
                right,
                node_count,
                allow_partial_aggregates,
            );
            if dist_left != **left || dist_right != **right {
                return PhysicalPlan::NestedLoopJoin {
                    left: Box::new(dist_left),
                    right: Box::new(dist_right),
                    join_type: *join_type,
                    condition: condition.clone(),
                    outputs: outputs.clone(),
                    filter: filter.clone(),
                    order_by: order_by.clone(),
                    limit: limit.clone(),
                    offset: offset.clone(),
                    distinct: *distinct,
                    distinct_on: distinct_on.clone(),
                };
            }
        }
        PhysicalPlan::MergeJoin {
            left,
            right,
            join_type,
            left_keys,
            right_keys,
            residual,
            outputs,
            filter,
            order_by,
            limit,
            offset,
            distinct,
            distinct_on,
        } => {
            let dist_left =
                distribute_plan_with_partial_aggregates(left, node_count, allow_partial_aggregates);
            let dist_right = distribute_plan_with_partial_aggregates(
                right,
                node_count,
                allow_partial_aggregates,
            );
            if dist_left != **left || dist_right != **right {
                return PhysicalPlan::MergeJoin {
                    left: Box::new(dist_left),
                    right: Box::new(dist_right),
                    join_type: *join_type,
                    left_keys: left_keys.clone(),
                    right_keys: right_keys.clone(),
                    residual: residual.clone(),
                    outputs: outputs.clone(),
                    filter: filter.clone(),
                    order_by: order_by.clone(),
                    limit: limit.clone(),
                    offset: offset.clone(),
                    distinct: *distinct,
                    distinct_on: distinct_on.clone(),
                };
            }
        }
        _ => {}
    }

    plan.clone()
}

#[cfg(test)]
mod tests {
    use super::*;
    use aiondb_core::{DataType, RelationId, Value};
    use aiondb_plan::{JoinType, ProjectionExpr, ResultField};

    fn make_result_field(name: &str, dt: DataType, nullable: bool) -> ResultField {
        ResultField {
            name: name.to_string(),
            data_type: dt,
            text_type_modifier: None,
            nullable,
        }
    }

    fn make_projection(name: &str, ordinal: usize, dt: DataType) -> ProjectionExpr {
        ProjectionExpr {
            field: make_result_field(name, dt.clone(), false),
            expr: TypedExpr::column_ref(name, ordinal, dt, false),
        }
    }

    fn make_seq_scan_plan(table_id: RelationId) -> PhysicalPlan {
        PhysicalPlan::ProjectTable {
            table_id,
            outputs: vec![
                make_projection("id", 0, DataType::Int),
                make_projection("name", 1, DataType::Text),
            ],
            filter: None,
            order_by: Vec::new(),
            limit: None,
            offset: None,
            distinct: false,
            distinct_on: Vec::new(),
            access_path: ScanAccessPath::SeqScan,
        }
    }

    fn make_aggregate_plan(table_id: RelationId) -> PhysicalPlan {
        PhysicalPlan::Aggregate {
            table_id,
            group_by: vec![TypedExpr::column_ref(
                "department",
                0,
                DataType::Text,
                false,
            )],
            grouping_sets: Vec::new(),
            aggregates: vec![make_projection("count", 0, DataType::BigInt)],
            having: None,
            filter: None,
            order_by: Vec::new(),
            limit: None,
            offset: None,
            distinct: false,
            distinct_on: Vec::new(),
            access_path: ScanAccessPath::SeqScan,
        }
    }

    // -----------------------------------------------------------------
    // Scan distribution
    // -----------------------------------------------------------------

    #[test]
    fn scan_distributed_with_three_nodes() {
        let plan = make_seq_scan_plan(RelationId::new(1));
        let result = try_distribute_scan(&plan, 3);
        assert!(result.is_some(), "expected scan to be distributed");

        let distributed = result.unwrap();
        if let PhysicalPlan::DistributedScan {
            table_id,
            node_count,
            output_fields,
            ..
        } = &distributed
        {
            assert_eq!(*table_id, RelationId::new(1));
            assert_eq!(*node_count, 3);
            assert_eq!(output_fields.len(), 2);
            assert_eq!(output_fields[0].name, "id");
            assert_eq!(output_fields[1].name, "name");
        } else {
            panic!("expected DistributedScan variant");
        }
    }

    #[test]
    fn scan_not_distributed_when_single_node() {
        let plan = make_seq_scan_plan(RelationId::new(1));
        let result = try_distribute_scan(&plan, 1);
        assert!(
            result.is_none(),
            "expected scan not to be distributed for node_count=1"
        );
    }

    #[test]
    fn scan_not_distributed_when_zero_nodes() {
        let plan = make_seq_scan_plan(RelationId::new(1));
        let result = try_distribute_scan(&plan, 0);
        assert!(result.is_none());
    }

    #[test]
    fn scan_not_distributed_for_index_scan() {
        let plan = PhysicalPlan::ProjectTable {
            table_id: RelationId::new(1),
            outputs: vec![make_projection("id", 0, DataType::Int)],
            filter: None,
            order_by: Vec::new(),
            limit: None,
            offset: None,
            distinct: false,
            distinct_on: Vec::new(),
            access_path: ScanAccessPath::IndexEq {
                index_id: aiondb_core::IndexId::new(1),
                value: Value::Int(42),
            },
        };
        let result = try_distribute_scan(&plan, 3);
        assert!(
            result.is_none(),
            "expected index scan not to be distributed"
        );
    }

    // -----------------------------------------------------------------
    // Aggregate distribution
    // -----------------------------------------------------------------

    #[test]
    fn aggregate_distributed_with_multiple_nodes() {
        let plan = make_aggregate_plan(RelationId::new(2));
        let result = try_distribute_aggregate(&plan, 4);
        assert!(result.is_some(), "expected aggregate to be distributed");

        let distributed = result.unwrap();
        if let PhysicalPlan::FinalAggregate {
            partials,
            group_by,
            aggregates,
            output_fields,
            ..
        } = &distributed
        {
            assert_eq!(partials.len(), 4);
            assert_eq!(group_by.len(), 1);
            assert_eq!(aggregates.len(), 1);
            assert_eq!(aggregates[0].name, "count");
            assert_eq!(output_fields.len(), 1);
            assert_eq!(output_fields[0].name, "count");

            // Verify each partial is a PartialAggregate wrapping a ProjectTable
            for partial in partials {
                if let PhysicalPlan::PartialAggregate { source, .. } = partial {
                    assert!(
                        matches!(**source, PhysicalPlan::ProjectTable { .. }),
                        "expected ProjectTable source in partial"
                    );
                } else {
                    panic!("expected PartialAggregate variant in partials");
                }
            }
        } else {
            panic!("expected FinalAggregate variant");
        }
    }

    #[test]
    fn aggregate_not_distributed_when_single_node() {
        let plan = make_aggregate_plan(RelationId::new(2));
        let result = try_distribute_aggregate(&plan, 1);
        assert!(result.is_none());
    }

    #[test]
    fn distribute_plan_can_disable_partial_aggregate_rewrite() {
        let plan = make_aggregate_plan(RelationId::new(2));
        let result = distribute_plan_with_partial_aggregates(&plan, 4, false);

        assert_eq!(result, plan);
    }

    // -----------------------------------------------------------------
    // Hash join distribution
    // -----------------------------------------------------------------

    #[test]
    fn hash_join_distributed_with_small_left() {
        let small_left = PhysicalPlan::ProjectOnce {
            outputs: vec![make_projection("val", 0, DataType::Int)],
            filter: None,
            order_by: Vec::new(),
            limit: None,
            offset: None,
            distinct: false,
            distinct_on: Vec::new(),
        };
        let big_right = make_seq_scan_plan(RelationId::new(5));

        let plan = PhysicalPlan::HashJoin {
            left: Box::new(small_left),
            right: Box::new(big_right),
            join_type: JoinType::Inner,
            left_keys: vec![0],
            right_keys: vec![0],
            condition: None,
            outputs: vec![
                make_projection("val", 0, DataType::Int),
                make_projection("id", 1, DataType::Int),
            ],
            filter: None,
            order_by: Vec::new(),
            limit: None,
            offset: None,
            distinct: false,
            distinct_on: Vec::new(),
        };

        let result = try_distribute_hash_join(&plan, 3);
        assert!(result.is_some(), "expected hash join to be distributed");

        let distributed = result.unwrap();
        if let PhysicalPlan::BroadcastHashJoin {
            broadcast,
            local,
            join_type,
            output_fields,
            ..
        } = &distributed
        {
            assert_eq!(*join_type, JoinType::Inner);
            assert_eq!(output_fields.len(), 2);
            // The broadcast side should be the small ProjectOnce
            assert!(
                matches!(**broadcast, PhysicalPlan::ProjectOnce { .. }),
                "expected broadcast to be the small plan"
            );
            // The local side should be the larger ProjectTable
            assert!(
                matches!(**local, PhysicalPlan::ProjectTable { .. }),
                "expected local to be the large plan"
            );
        } else {
            panic!("expected BroadcastHashJoin variant");
        }
    }

    #[test]
    fn hash_join_not_distributed_when_neither_side_small() {
        let left = make_seq_scan_plan(RelationId::new(1));
        let right = make_seq_scan_plan(RelationId::new(2));

        let plan = PhysicalPlan::HashJoin {
            left: Box::new(left),
            right: Box::new(right),
            join_type: JoinType::Inner,
            left_keys: vec![0],
            right_keys: vec![0],
            condition: None,
            outputs: vec![make_projection("id", 0, DataType::Int)],
            filter: None,
            order_by: Vec::new(),
            limit: None,
            offset: None,
            distinct: false,
            distinct_on: Vec::new(),
        };

        let result = try_distribute_hash_join(&plan, 3);
        assert!(
            result.is_none(),
            "expected no distribution when neither side is small"
        );
    }

    // -----------------------------------------------------------------
    // distribute_plan top-level
    // -----------------------------------------------------------------

    #[test]
    fn distribute_plan_returns_clone_for_single_node() {
        let plan = make_seq_scan_plan(RelationId::new(1));
        let result = distribute_plan(&plan, 1);
        assert_eq!(result, plan);
    }

    #[test]
    fn distribute_plan_applies_scan_rule() {
        let plan = make_seq_scan_plan(RelationId::new(1));
        let result = distribute_plan(&plan, 2);
        assert!(
            matches!(result, PhysicalPlan::DistributedScan { .. }),
            "expected DistributedScan from distribute_plan"
        );
    }

    #[test]
    fn distribute_plan_falls_through_for_non_matching_plan() {
        let plan = PhysicalPlan::ProjectOnce {
            outputs: Vec::new(),
            filter: None,
            order_by: Vec::new(),
            limit: None,
            offset: None,
            distinct: false,
            distinct_on: Vec::new(),
        };
        let result = distribute_plan(&plan, 3);
        assert_eq!(
            result, plan,
            "expected unchanged plan for non-matching type"
        );
    }

    // -----------------------------------------------------------------
    // Recursive distribution
    // -----------------------------------------------------------------

    #[test]
    fn distribute_plan_recurses_into_project_source() {
        let inner_scan = make_seq_scan_plan(RelationId::new(10));
        let wrapper = PhysicalPlan::ProjectSource {
            source: Box::new(inner_scan),
            outputs: vec![
                make_projection("id", 0, DataType::Int),
                make_projection("name", 1, DataType::Text),
            ],
            filter: None,
            order_by: Vec::new(),
            limit: None,
            offset: None,
            distinct: false,
            distinct_on: Vec::new(),
        };

        let result = distribute_plan(&wrapper, 3);

        // The outer node should still be a ProjectSource, but its source
        // should now be a DistributedScan instead of a ProjectTable.
        if let PhysicalPlan::ProjectSource {
            source, outputs, ..
        } = &result
        {
            assert!(
                matches!(
                    **source,
                    PhysicalPlan::DistributedScan { node_count: 3, .. }
                ),
                "expected inner source to be DistributedScan, got {source:?}"
            );
            assert_eq!(outputs.len(), 2);
        } else {
            panic!("expected ProjectSource wrapper, got {result:?}");
        }
    }

    // -----------------------------------------------------------------
    // Join key type inference
    // -----------------------------------------------------------------

    #[test]
    fn hash_join_key_types_inferred_from_plan() {
        // Left side has a Text column at ordinal 0.
        let small_left = PhysicalPlan::ProjectOnce {
            outputs: vec![make_projection("code", 0, DataType::Text)],
            filter: None,
            order_by: Vec::new(),
            limit: None,
            offset: None,
            distinct: false,
            distinct_on: Vec::new(),
        };
        // Right side has a Text column at ordinal 0.
        let big_right = PhysicalPlan::ProjectValues {
            output_fields: vec![make_result_field("code", DataType::Text, false)],
            rows: Vec::new(),
            order_by: Vec::new(),
            limit: None,
            offset: None,
        };

        let plan = PhysicalPlan::HashJoin {
            left: Box::new(small_left),
            right: Box::new(big_right),
            join_type: JoinType::Inner,
            left_keys: vec![0],
            right_keys: vec![0],
            condition: None,
            outputs: vec![make_projection("code", 0, DataType::Text)],
            filter: None,
            order_by: Vec::new(),
            limit: None,
            offset: None,
            distinct: false,
            distinct_on: Vec::new(),
        };

        let result = try_distribute_hash_join(&plan, 2);
        assert!(result.is_some(), "expected hash join to be distributed");

        if let PhysicalPlan::BroadcastHashJoin {
            left_keys,
            right_keys,
            ..
        } = result.unwrap()
        {
            assert_eq!(left_keys.len(), 1);
            assert_eq!(
                left_keys[0].data_type,
                DataType::Text,
                "left key should have Text type, not hardcoded Int"
            );
            assert_eq!(right_keys.len(), 1);
            assert_eq!(
                right_keys[0].data_type,
                DataType::Text,
                "right key should have Text type, not hardcoded Int"
            );
        } else {
            panic!("expected BroadcastHashJoin");
        }
    }
}
