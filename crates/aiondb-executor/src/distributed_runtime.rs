//! Runtime bridge for serialized distributed physical plans.
//!
//! The executor currently supports a single targeted root fragment and the
//! first useful distributed DAG shape: local, shard-local, or remote source
//! fragments merged into a coordinator root. Other exchanges stay explicit
//! errors until exchange source/sink operators are wired.

use std::sync::Arc;

use aiondb_core::{DbError, DbResult, Row, Value};
use aiondb_plan::{
    DistributedPhysicalPlan, ExchangeKind, FragmentPlacement, PlanFragment, ResultField, SortExpr,
    TypedExpr, TypedExprKind,
};

use crate::executor::helpers::{sort_query_rows, SortedQueryRow};
use crate::{DistributedFragment, ExecutionContext, ExecutionResult, Executor, FragmentTarget};

#[derive(Debug, Default)]
pub struct DistributedQueryRuntime;

impl DistributedQueryRuntime {
    pub fn execute(
        executor: &Executor,
        plan: &DistributedPhysicalPlan,
        context: &ExecutionContext,
    ) -> DbResult<ExecutionResult> {
        plan.validate()?;
        let root = root_fragment(plan)?;
        let fragment_context = context.clone();
        if plan.fragments.len() == 1 && plan.edges.is_empty() {
            return execute_single_root(executor, plan, root, &fragment_context);
        }

        execute_gather_dag(executor, plan, root, &fragment_context)
    }
}

impl Executor {
    pub fn execute_distributed(
        &self,
        plan: &DistributedPhysicalPlan,
        context: &ExecutionContext,
    ) -> DbResult<ExecutionResult> {
        DistributedQueryRuntime::execute(self, plan, context)
    }
}

fn root_fragment(plan: &DistributedPhysicalPlan) -> DbResult<&PlanFragment> {
    plan.fragment(plan.root_fragment_id).ok_or_else(|| {
        DbError::internal(format!(
            "distributed plan root fragment {} is missing",
            plan.root_fragment_id.get()
        ))
    })
}

fn execute_single_root(
    executor: &Executor,
    plan: &DistributedPhysicalPlan,
    root: &PlanFragment,
    context: &ExecutionContext,
) -> DbResult<ExecutionResult> {
    let target = runtime_fragment_target(root, plan, context);
    if target == FragmentTarget::Local {
        let result = executor.execute(&root.plan, context)?;
        let ExecutionResult::Query { columns, rows } = result else {
            return Ok(result);
        };
        ensure_gather_schema_matches_root(root, &columns)?;

        return Ok(ExecutionResult::Query {
            columns: root.output_fields.clone(),
            rows,
        });
    }

    let fragment = DistributedFragment {
        target,
        fragment_id: Some(fragment_id_u32(root.fragment_id)?),
        shard_id: root.shard_id().map(|shard_id| shard_id.get()),
        plan: root.plan.clone(),
        partition: None,
    };
    let result = executor.execute_distributed_fragments_targeted(&[fragment], context)?;
    let ExecutionResult::Query { columns, rows } = result else {
        return Ok(result);
    };
    ensure_gather_schema_matches_root(root, &columns)?;

    Ok(ExecutionResult::Query {
        columns: root.output_fields.clone(),
        rows,
    })
}

#[cfg(test)]
fn is_single_process_root(root: &PlanFragment) -> bool {
    if root.is_local_coordinator() {
        return true;
    }

    match (&root.target, &root.placement) {
        (aiondb_plan::FragmentTarget::Node { node_id }, FragmentPlacement::Local) => {
            node_id == &aiondb_cluster::NodeId::local()
        }
        (
            aiondb_plan::FragmentTarget::ShardLeader { .. }
            | aiondb_plan::FragmentTarget::AnyShardReplica { .. },
            FragmentPlacement::Shard { .. },
        ) => true,
        _ => false,
    }
}

fn ensure_gather_root(root: &PlanFragment) -> DbResult<()> {
    if root.is_local_coordinator() {
        return Ok(());
    }

    Err(DbError::feature_not_supported(
        "Gather execution currently requires a local coordinator root fragment",
    ))
}

fn execute_gather_dag(
    executor: &Executor,
    plan: &DistributedPhysicalPlan,
    root: &PlanFragment,
    context: &ExecutionContext,
) -> DbResult<ExecutionResult> {
    ensure_gather_root(root)?;
    if plan.edges.is_empty() {
        return Err(DbError::feature_not_supported(
            "distributed fragment DAG without edges is not executable",
        ));
    }

    let merge_sort_order_by = root_merge_sort_order_by(plan, root)?;
    let mut fragments = Vec::with_capacity(plan.edges.len());
    for edge in &plan.edges {
        if edge.target_fragment_id != root.fragment_id || !is_root_merge_exchange(&edge.exchange) {
            return Err(DbError::feature_not_supported(
                "only root-directed exchange edges are executable",
            ));
        }
        let source = plan.fragment(edge.source_fragment_id).ok_or_else(|| {
            DbError::internal(format!(
                "distributed plan source fragment {} is missing",
                edge.source_fragment_id.get()
            ))
        })?;
        validate_edge_against_source(edge, source)?;
        let mut fragment = DistributedFragment {
            target: runtime_fragment_target(source, plan, context),
            fragment_id: Some(fragment_id_u32(source.fragment_id)?),
            shard_id: source.shard_id().map(|shard_id| shard_id.get()),
            plan: source.plan.clone(),
            partition: None,
        };
        if let Some(partition) = &source.partition {
            fragment = fragment.with_partition(partition.index, partition.count);
        }
        fragments.push(fragment);
    }

    let result = executor.execute_distributed_fragments_targeted(&fragments, context)?;
    let ExecutionResult::Query { columns, mut rows } = result else {
        return Ok(result);
    };
    ensure_gather_schema_matches_root(root, &columns)?;
    if let Some(order_by) = merge_sort_order_by.as_deref() {
        rows = merge_sort_rows(rows, order_by, context)?;
    }
    rows = apply_root_project_values_bounds(root, rows, context)?;

    Ok(ExecutionResult::Query {
        columns: root.output_fields.clone(),
        rows,
    })
}

fn is_root_merge_exchange(exchange: &ExchangeKind) -> bool {
    matches!(
        exchange,
        ExchangeKind::Gather
            | ExchangeKind::Broadcast
            | ExchangeKind::Repartition { .. }
            | ExchangeKind::MergeSortGather { .. }
    )
}

fn root_merge_sort_order_by(
    plan: &DistributedPhysicalPlan,
    root: &PlanFragment,
) -> DbResult<Option<Vec<SortExpr>>> {
    let mut order_by: Option<Vec<SortExpr>> = None;
    let mut saw_plain_merge = false;
    for edge in &plan.edges {
        if edge.target_fragment_id != root.fragment_id {
            continue;
        }
        match &edge.exchange {
            ExchangeKind::MergeSortGather {
                order_by: edge_order_by,
            } => {
                if saw_plain_merge {
                    return Err(DbError::feature_not_supported(
                        "MergeSortGather cannot be mixed with Gather/Broadcast for the same root",
                    ));
                }
                if let Some(existing) = &order_by {
                    if existing != edge_order_by {
                        return Err(DbError::feature_not_supported(
                            "MergeSortGather edges into the same root require identical ordering",
                        ));
                    }
                } else {
                    order_by = Some(edge_order_by.clone());
                }
            }
            ExchangeKind::Gather | ExchangeKind::Broadcast | ExchangeKind::Repartition { .. } => {
                if order_by.is_some() {
                    return Err(DbError::feature_not_supported(
                        "MergeSortGather cannot be mixed with other exchanges for the same root",
                    ));
                }
                saw_plain_merge = true;
            }
        }
    }
    Ok(order_by)
}

fn merge_sort_rows(
    rows: Vec<Row>,
    order_by: &[SortExpr],
    context: &ExecutionContext,
) -> DbResult<Vec<Row>> {
    let mut sorted_rows = Vec::with_capacity(rows.len());
    for row in rows {
        let sort_keys = order_by
            .iter()
            .map(|sort| merge_sort_key(&row, sort))
            .collect::<DbResult<Vec<_>>>()?;
        sorted_rows.push(SortedQueryRow {
            row,
            sort_keys: Arc::new(sort_keys),
        });
    }
    sort_query_rows(&mut sorted_rows, order_by, context)?;
    Ok(sorted_rows.into_iter().map(|row| row.row).collect())
}

fn merge_sort_key(row: &Row, sort: &SortExpr) -> DbResult<Value> {
    match &sort.expr.kind {
        TypedExprKind::ColumnRef { ordinal, .. } => {
            row.values.get(*ordinal).cloned().ok_or_else(|| {
                DbError::internal(format!(
                    "MergeSortGather sort ordinal {ordinal} is outside row width {}",
                    row.values.len()
                ))
            })
        }
        TypedExprKind::Literal(value) => Ok(value.clone()),
        _ => Err(DbError::feature_not_supported(
            "MergeSortGather currently supports only output column or literal sort keys",
        )),
    }
}

fn apply_root_project_values_bounds(
    root: &PlanFragment,
    mut rows: Vec<Row>,
    context: &ExecutionContext,
) -> DbResult<Vec<Row>> {
    let aiondb_plan::PhysicalPlan::ProjectValues { limit, offset, .. } = &root.plan else {
        return Ok(rows);
    };

    let plan_limit = limit
        .as_ref()
        .map(|expr| literal_limit_offset(expr, "LIMIT"))
        .transpose()?;
    let effective_limit = match (plan_limit, context.collect_row_limit) {
        (Some(limit), Some(collect_limit)) => Some(limit.min(collect_limit)),
        (Some(limit), None) => Some(limit),
        (None, Some(collect_limit)) => Some(collect_limit),
        (None, None) => None,
    };
    let total_offset = offset
        .as_ref()
        .map(|expr| literal_limit_offset(expr, "OFFSET"))
        .transpose()?
        .unwrap_or(0)
        .saturating_add(context.collect_row_offset);

    if total_offset > 0 {
        let skip = clamp_u64_to_len(total_offset, rows.len());
        rows.drain(0..skip);
    }
    if let Some(limit) = effective_limit {
        rows.truncate(clamp_u64_to_len(limit, rows.len()));
    }
    Ok(rows)
}

fn literal_limit_offset(expr: &TypedExpr, clause: &str) -> DbResult<u64> {
    match &expr.kind {
        TypedExprKind::Literal(Value::Int(value)) if *value >= 0 => u64::try_from(*value)
            .map_err(|_| DbError::internal(format!("{clause} is out of range for u64"))),
        TypedExprKind::Literal(Value::BigInt(value)) if *value >= 0 => u64::try_from(*value)
            .map_err(|_| DbError::internal(format!("{clause} is out of range for u64"))),
        TypedExprKind::Literal(Value::Null) if clause.eq_ignore_ascii_case("LIMIT") => Ok(u64::MAX),
        TypedExprKind::Literal(Value::Null) if clause.eq_ignore_ascii_case("OFFSET") => Ok(0),
        TypedExprKind::Literal(Value::Int(_) | Value::BigInt(_)) => Err(DbError::parse_error(
            aiondb_core::SqlState::InvalidParameterValue,
            format!("{clause} must not be negative"),
        )),
        _ => Err(DbError::feature_not_supported(format!(
            "distributed root {clause} currently requires a non-negative integer literal"
        ))),
    }
}

fn clamp_u64_to_len(value: u64, upper_bound: usize) -> usize {
    usize::try_from(value)
        .unwrap_or(usize::MAX)
        .min(upper_bound)
}

fn validate_edge_against_source(
    edge: &aiondb_plan::FragmentEdge,
    source: &PlanFragment,
) -> DbResult<()> {
    match &edge.exchange {
        ExchangeKind::Repartition { key_ordinals } => {
            for ordinal in key_ordinals {
                if *ordinal >= source.output_fields.len() {
                    return Err(DbError::internal(format!(
                        "Repartition edge from fragment {} references key ordinal {} outside source width {}",
                        source.fragment_id.get(),
                        ordinal,
                        source.output_fields.len()
                    )));
                }
            }
            Ok(())
        }
        ExchangeKind::MergeSortGather { order_by } => {
            for sort in order_by {
                validate_merge_sort_expr(source, sort)?;
            }
            Ok(())
        }
        ExchangeKind::Gather | ExchangeKind::Broadcast => Ok(()),
    }
}

fn validate_merge_sort_expr(source: &PlanFragment, sort: &SortExpr) -> DbResult<()> {
    if let TypedExprKind::ColumnRef { ordinal, .. } = &sort.expr.kind {
        if *ordinal >= source.output_fields.len() {
            return Err(DbError::internal(format!(
                "MergeSortGather edge from fragment {} references sort ordinal {} outside source width {}",
                source.fragment_id.get(),
                ordinal,
                source.output_fields.len()
            )));
        }
    }
    Ok(())
}

fn fragment_id_u32(fragment_id: aiondb_cluster::FragmentId) -> DbResult<u32> {
    u32::try_from(fragment_id.get()).map_err(|_| {
        DbError::internal(format!(
            "distributed fragment id {} exceeds transport fragment id range",
            fragment_id.get()
        ))
    })
}

fn runtime_fragment_target(
    fragment: &PlanFragment,
    plan: &DistributedPhysicalPlan,
    context: &ExecutionContext,
) -> FragmentTarget {
    match &fragment.placement {
        FragmentPlacement::Local => FragmentTarget::Local,
        FragmentPlacement::Remote { node_id } => {
            FragmentTarget::Remote(node_id.as_str().to_owned())
        }
        FragmentPlacement::Shard { shard_id } => {
            let plan_leader = plan
                .shard_leader_node(*shard_id)
                .map(aiondb_cluster::NodeId::as_str);
            match plan_leader.or_else(|| context.distributed_shard_leader_node(shard_id.get())) {
                Some(node_id) if node_id.eq_ignore_ascii_case("local") => FragmentTarget::Local,
                Some(node_id) => FragmentTarget::Remote(node_id.to_owned()),
                None => FragmentTarget::Local,
            }
        }
    }
}

fn ensure_gather_schema_matches_root(root: &PlanFragment, actual: &[ResultField]) -> DbResult<()> {
    if schema_compatible(&root.output_fields, actual) {
        return Ok(());
    }

    Err(DbError::internal(format!(
        "Gather result schema is incompatible with root fragment {}",
        root.fragment_id.get()
    )))
}

fn schema_compatible(expected: &[ResultField], actual: &[ResultField]) -> bool {
    expected.len() == actual.len()
        && expected.iter().zip(actual).all(|(expected, actual)| {
            expected.data_type == actual.data_type
                && expected.text_type_modifier == actual.text_type_modifier
                && expected.nullable == actual.nullable
        })
}

#[cfg(test)]
mod tests {
    use aiondb_cluster::FragmentId;
    use aiondb_core::DataType;
    use aiondb_plan::{DistributedPhysicalPlan, PhysicalPlan, ResultField};
    use aiondb_tx::{IsolationLevel, Snapshot};

    use super::*;

    fn test_context() -> ExecutionContext {
        ExecutionContext::new(
            aiondb_core::TxnId::default(),
            IsolationLevel::ReadCommitted,
            Snapshot::new(
                aiondb_core::TxnId::default(),
                aiondb_core::TxnId::default(),
                Vec::new(),
            ),
            u64::MAX,
            None,
            0,
            u64::MAX,
            u64::MAX,
            u64::MAX,
            None,
            None,
        )
    }

    #[test]
    fn root_fragment_rejects_missing_root() {
        let plan = DistributedPhysicalPlan::new(
            None,
            Default::default(),
            Default::default(),
            Default::default(),
            FragmentId::new(9),
            Vec::new(),
            Vec::new(),
        );

        assert!(root_fragment(&plan).is_err());
    }

    #[test]
    fn single_local_root_validation_accepts_single_local_fragment() {
        let plan = DistributedPhysicalPlan::single_local(PhysicalPlan::InternalNoOp {
            tag: "SELECT".to_owned(),
            notice: None,
        });
        let root = root_fragment(&plan).unwrap();

        assert!(is_single_process_root(root));
        assert!(ensure_gather_root(root).is_ok());
    }

    #[test]
    fn single_process_root_accepts_shard_local_fragment_but_not_gather_root() {
        let mut plan = DistributedPhysicalPlan::single_local(PhysicalPlan::InternalNoOp {
            tag: "SELECT".to_owned(),
            notice: None,
        });
        plan.fragments[0].target = aiondb_plan::FragmentTarget::ShardLeader {
            shard_id: aiondb_cluster::ShardId::new(1),
        };
        plan.fragments[0].placement = aiondb_plan::FragmentPlacement::Shard {
            shard_id: aiondb_cluster::ShardId::new(1),
        };
        let root = root_fragment(&plan).unwrap();

        assert!(is_single_process_root(root));
        assert!(ensure_gather_root(root).is_err());
    }

    #[test]
    fn runtime_fragment_target_executes_shard_placement_locally() {
        let mut plan = DistributedPhysicalPlan::single_local(PhysicalPlan::InternalNoOp {
            tag: "SELECT".to_owned(),
            notice: None,
        });
        plan.fragments[0].placement = aiondb_plan::FragmentPlacement::Shard {
            shard_id: aiondb_cluster::ShardId::new(1),
        };

        assert_eq!(
            runtime_fragment_target(&plan.fragments[0], &plan, &test_context()),
            FragmentTarget::Local
        );
    }

    #[test]
    fn runtime_fragment_target_routes_shard_placement_to_leader_node() {
        let mut plan = DistributedPhysicalPlan::single_local(PhysicalPlan::InternalNoOp {
            tag: "SELECT".to_owned(),
            notice: None,
        });
        plan.fragments[0].placement = aiondb_plan::FragmentPlacement::Shard {
            shard_id: aiondb_cluster::ShardId::new(7),
        };
        let context =
            test_context().with_distributed_shard_leader_nodes(vec![(7, "node-b".to_owned())]);

        assert_eq!(
            runtime_fragment_target(&plan.fragments[0], &plan, &context),
            FragmentTarget::Remote("node-b".to_owned())
        );
    }

    #[test]
    fn runtime_fragment_target_prefers_plan_shard_leader_over_context_map() {
        let mut plan = DistributedPhysicalPlan::single_local(PhysicalPlan::InternalNoOp {
            tag: "SELECT".to_owned(),
            notice: None,
        })
        .with_shard_leader_nodes(vec![(
            aiondb_cluster::ShardId::new(7),
            aiondb_cluster::NodeId::new("node-c"),
        )]);
        plan.fragments[0].placement = aiondb_plan::FragmentPlacement::Shard {
            shard_id: aiondb_cluster::ShardId::new(7),
        };
        let context =
            test_context().with_distributed_shard_leader_nodes(vec![(7, "node-b".to_owned())]);

        assert_eq!(
            runtime_fragment_target(&plan.fragments[0], &plan, &context),
            FragmentTarget::Remote("node-c".to_owned())
        );
    }

    #[test]
    fn gather_schema_compatibility_ignores_column_names_but_rejects_type_drift() {
        let expected = vec![ResultField {
            name: "root_name".to_owned(),
            data_type: DataType::Int,
            text_type_modifier: None,
            nullable: false,
        }];
        let renamed = vec![ResultField {
            name: "source_name".to_owned(),
            data_type: DataType::Int,
            text_type_modifier: None,
            nullable: false,
        }];
        let type_drift = vec![ResultField {
            name: "source_name".to_owned(),
            data_type: DataType::Text,
            text_type_modifier: None,
            nullable: false,
        }];

        assert!(schema_compatible(&expected, &renamed));
        assert!(!schema_compatible(&expected, &type_drift));
    }
}
