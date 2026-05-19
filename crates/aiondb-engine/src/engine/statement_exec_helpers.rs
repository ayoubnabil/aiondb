//! Statement-execution helper free functions.
//!
//! Split out of `statement_exec.rs` (post-`impl Engine` free fns):
//! distributed-aggregate merge/finalize, statement classification,
//! access-control mapping. Parent scope reached via `use super::*`.
#![allow(clippy::pedantic, clippy::too_many_lines, clippy::wildcard_imports)]

use super::*;

pub(in crate::engine) fn remote_fragment_client_config(
    runtime_config: &RuntimeConfig,
    node_config: &aiondb_config::RemoteNodeConfig,
) -> FragmentClientConfig {
    FragmentClientConfig {
        addr: node_config.addr.clone(),
        auth_token: aiondb_fragment_transport::AuthToken::new(
            runtime_config
                .distributed
                .inter_node_auth_token
                .clone()
                .unwrap_or_default(),
        ),
        tls: runtime_config
            .distributed
            .tls_ca_cert_path
            .as_ref()
            .map(
                |ca_cert_path| aiondb_fragment_transport::tls::TlsClientConfig {
                    ca_cert_path: ca_cert_path.clone(),
                    client_cert_path: runtime_config.distributed.tls_cert_path.clone(),
                    client_key_path: runtime_config.distributed.tls_key_path.clone(),
                },
            ),
        connect_timeout: runtime_config.distributed.remote_connect_timeout,
        max_retries: runtime_config.distributed.remote_max_retries,
        retry_backoff: runtime_config.distributed.remote_retry_backoff,
    }
}

pub(in crate::engine) fn execute_remote_internal_plan_blocking(
    client_config: FragmentClientConfig,
    physical_plan: &aiondb_plan::PhysicalPlan,
    context: FragmentContext,
) -> DbResult<ExecutionResult> {
    if tokio::runtime::Handle::try_current().is_ok() {
        let (result_tx, result_rx) = std::sync::mpsc::sync_channel(1);
        let plan = physical_plan.clone();
        std::thread::Builder::new()
            .name("aiondb-remote-internal-plan-client".to_owned())
            .spawn(move || {
                let client = aiondb_fragment_transport::FragmentClient::new(client_config);
                let result = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .map_err(|error| {
                        DbError::internal(format!(
                            "failed to create tokio runtime for remote internal plan execution: {error}"
                        ))
                    })
                    .and_then(|runtime| runtime.block_on(client.execute(&plan, context)));
                let _ = result_tx.send(result);
            })
            .map_err(|error| {
                DbError::internal(format!(
                    "failed to spawn remote internal plan client thread: {error}"
                ))
            })?;
        return result_rx.recv().map_err(|error| {
            DbError::internal(format!(
                "remote internal plan client thread ended without result: {error}"
            ))
        })?;
    }

    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|error| {
            DbError::internal(format!(
                "failed to create tokio runtime for remote internal plan execution: {error}"
            ))
        })?;
    let client = aiondb_fragment_transport::FragmentClient::new(client_config);
    runtime.block_on(client.execute(physical_plan, context))
}

pub(in crate::engine) fn compute_insert_row_shard_id(
    row: &[aiondb_plan::TypedExpr],
    shard_key_ordinals: &[usize],
    shard_count: u32,
) -> DbResult<u32> {
    if shard_count == 0 {
        return Err(DbError::internal("shard_count is 0 in shard config"));
    }
    let mut shard_values = Vec::with_capacity(shard_key_ordinals.len());
    for ordinal in shard_key_ordinals {
        let Some(expr) = row.get(*ordinal) else {
            return Err(DbError::internal(format!(
                "INSERT row is missing shard key value at ordinal {ordinal}"
            )));
        };
        let TypedExprKind::Literal(value) = &expr.kind else {
            return Err(DbError::feature_not_supported(
                "remote sharded INSERT currently requires literal shard-key values",
            ));
        };
        shard_values.push(value);
    }
    aiondb_shard::shard_index_for_values(shard_values, shard_count)
}

pub(in crate::engine) fn distributed_plan_leader_nodes(
    nodes: Vec<(u32, String)>,
) -> Vec<(aiondb_cluster::ShardId, aiondb_cluster::NodeId)> {
    nodes
        .into_iter()
        .map(|(shard_id, node_id)| {
            (
                aiondb_cluster::ShardId::new(shard_id),
                aiondb_cluster::NodeId::new(node_id),
            )
        })
        .collect()
}

pub(in crate::engine) fn classify_distributed_scalar_aggregate_projection(
    projection: &ProjectionExpr,
) -> Option<DistributedScalarAggregateKind> {
    match &projection.expr.kind {
        TypedExprKind::AggCount {
            distinct: false, ..
        } => Some(DistributedScalarAggregateKind::Count),
        TypedExprKind::AggSum {
            distinct: false, ..
        } => Some(DistributedScalarAggregateKind::Sum),
        TypedExprKind::AggMin { .. } => Some(DistributedScalarAggregateKind::Min),
        TypedExprKind::AggMax { .. } => Some(DistributedScalarAggregateKind::Max),
        _ => None,
    }
}

pub(in crate::engine) fn build_distributed_aggregate_plan_shape(
    physical_plan: &aiondb_plan::PhysicalPlan,
    group_by: &[aiondb_plan::TypedExpr],
    projections: &[ProjectionExpr],
) -> Option<DistributedAggregatePlanShape> {
    let aiondb_plan::PhysicalPlan::Aggregate {
        table_id,
        grouping_sets,
        having,
        filter,
        distinct,
        distinct_on,
        access_path,
        ..
    } = physical_plan
    else {
        return None;
    };

    let mut source_projections = Vec::new();
    let mut output_plans = Vec::with_capacity(projections.len());
    for (output_index, projection) in projections.iter().enumerate() {
        if group_by
            .iter()
            .any(|group_expr| group_expr == &projection.expr)
        {
            let source_index = source_projections.len();
            source_projections.push(projection.clone());
            output_plans.push(DistributedAggregateOutputPlan::GroupKey { source_index });
            continue;
        }

        if let Some(kind) = classify_distributed_scalar_aggregate_projection(projection) {
            let source_index = source_projections.len();
            source_projections.push(projection.clone());
            output_plans.push(DistributedAggregateOutputPlan::Aggregate { kind, source_index });
            continue;
        }

        let TypedExprKind::AggAvg {
            expr,
            distinct: false,
            filter,
        } = &projection.expr.kind
        else {
            return None;
        };

        let sum_source_index = source_projections.len();
        source_projections.push(ProjectionExpr {
            field: ResultField {
                name: format!("__avg_sum_{output_index}"),
                data_type: expr.data_type.clone(),
                text_type_modifier: None,
                nullable: true,
            },
            expr: aiondb_plan::TypedExpr {
                kind: TypedExprKind::AggSum {
                    expr: expr.clone(),
                    distinct: false,
                    filter: filter.clone(),
                },
                data_type: expr.data_type.clone(),
                nullable: true,
            },
        });

        let count_source_index = source_projections.len();
        source_projections.push(ProjectionExpr {
            field: ResultField {
                name: format!("__avg_count_{output_index}"),
                data_type: DataType::BigInt,
                text_type_modifier: None,
                nullable: false,
            },
            expr: aiondb_plan::TypedExpr {
                kind: TypedExprKind::AggCount {
                    expr: Some(expr.clone()),
                    distinct: false,
                    filter: filter.clone(),
                },
                data_type: DataType::BigInt,
                nullable: false,
            },
        });
        output_plans.push(DistributedAggregateOutputPlan::Avg {
            sum_source_index,
            count_source_index,
        });
    }

    let source_output_fields = source_projections
        .iter()
        .map(|projection| projection.field.clone())
        .collect::<Vec<_>>();
    let source_plan = aiondb_plan::PhysicalPlan::Aggregate {
        table_id: *table_id,
        group_by: group_by.to_vec(),
        grouping_sets: grouping_sets.clone(),
        aggregates: source_projections,
        having: having.clone(),
        filter: filter.clone(),
        order_by: Vec::new(),
        limit: None,
        offset: None,
        distinct: *distinct,
        distinct_on: distinct_on.clone(),
        access_path: access_path.clone(),
    };

    Some(DistributedAggregatePlanShape {
        source_plan,
        source_output_fields,
        output_plans,
    })
}

pub(in crate::engine) fn resolve_distributed_aggregate_sort_indices(
    order_by: &[aiondb_plan::SortExpr],
    projections: &[aiondb_plan::ProjectionExpr],
) -> Option<Vec<usize>> {
    order_by
        .iter()
        .map(|sort| {
            projections
                .iter()
                .position(|projection| projection.expr == sort.expr)
                .or(match &sort.expr.kind {
                    TypedExprKind::ColumnRef { ordinal, .. } if *ordinal < projections.len() => {
                        Some(*ordinal)
                    }
                    _ => None,
                })
        })
        .collect()
}

pub(in crate::engine) fn apply_distributed_aggregate_order_and_bounds(
    rows: &mut Vec<Row>,
    order_by: &[aiondb_plan::SortExpr],
    sort_indices: &[usize],
    limit: Option<u64>,
    offset: u64,
) -> DbResult<()> {
    if !order_by.is_empty() {
        let sort_error = std::cell::RefCell::new(None);
        rows.sort_by(|left, right| {
            if sort_error.borrow().is_some() {
                return std::cmp::Ordering::Equal;
            }
            for (sort, index) in order_by.iter().zip(sort_indices.iter()) {
                let left_value = left.values.get(*index).unwrap_or(&Value::Null);
                let right_value = right.values.get(*index).unwrap_or(&Value::Null);
                match compare_distributed_sort_values(
                    left_value,
                    right_value,
                    sort.descending,
                    sort.nulls_first,
                ) {
                    Ok(std::cmp::Ordering::Equal) => {}
                    Ok(ordering) => return ordering,
                    Err(error) => {
                        *sort_error.borrow_mut() = Some(error);
                        return std::cmp::Ordering::Equal;
                    }
                }
            }
            std::cmp::Ordering::Equal
        });
        if let Some(error) = sort_error.into_inner() {
            return Err(error);
        }
    }

    if offset > 0 {
        let skip = clamp_u64_to_len(offset, rows.len());
        rows.drain(..skip);
    }
    if let Some(limit) = limit {
        rows.truncate(clamp_u64_to_len(limit, rows.len()));
    }
    Ok(())
}

pub(in crate::engine) fn compare_distributed_sort_values(
    left: &Value,
    right: &Value,
    descending: bool,
    nulls_first: Option<bool>,
) -> DbResult<std::cmp::Ordering> {
    let nulls_first = nulls_first.unwrap_or(descending);
    let ordering = match (left, right) {
        (Value::Null, Value::Null) => std::cmp::Ordering::Equal,
        (Value::Null, _) => {
            if nulls_first {
                std::cmp::Ordering::Less
            } else {
                std::cmp::Ordering::Greater
            }
        }
        (_, Value::Null) => {
            if nulls_first {
                std::cmp::Ordering::Greater
            } else {
                std::cmp::Ordering::Less
            }
        }
        _ => compare_runtime_values(left, right)?.unwrap_or(std::cmp::Ordering::Equal),
    };
    Ok(if descending {
        ordering.reverse()
    } else {
        ordering
    })
}

pub(in crate::engine) fn literal_limit_offset_u64(expr: &aiondb_plan::TypedExpr, clause: &str) -> DbResult<Option<u64>> {
    match &expr.kind {
        TypedExprKind::Literal(Value::Int(value)) if *value >= 0 => {
            Ok(Some(u64::try_from(*value).map_err(|_| {
                DbError::internal(format!("{clause} is out of range for u64"))
            })?))
        }
        TypedExprKind::Literal(Value::BigInt(value)) if *value >= 0 => {
            Ok(Some(u64::try_from(*value).map_err(|_| {
                DbError::internal(format!("{clause} is out of range for u64"))
            })?))
        }
        TypedExprKind::Literal(Value::Int(_) | Value::BigInt(_)) => {
            Err(DbError::Internal(Box::new(aiondb_core::ErrorReport::new(
                aiondb_core::SqlState::InvalidParameterValue,
                format!("{clause} must not be negative"),
            ))))
        }
        TypedExprKind::Literal(Value::Null) if clause.eq_ignore_ascii_case("LIMIT") => Ok(None),
        TypedExprKind::Literal(Value::Null) if clause.eq_ignore_ascii_case("OFFSET") => Ok(Some(0)),
        TypedExprKind::Literal(Value::Null) => {
            Err(DbError::internal(format!("{clause} does not accept NULL")))
        }
        _ => Err(DbError::feature_not_supported(format!(
            "remote sharded aggregate currently requires literal {clause}"
        ))),
    }
}

pub(in crate::engine) fn clamp_u64_to_len(value: u64, upper_bound: usize) -> usize {
    usize::try_from(value)
        .unwrap_or(usize::MAX)
        .min(upper_bound)
}

pub(in crate::engine) fn command_rows_affected(result: ExecutionResult, source: &str) -> DbResult<u64> {
    match result {
        ExecutionResult::Command { rows_affected, .. } => Ok(rows_affected),
        other => Err(DbError::internal(format!(
            "{source} returned non-command result: {other:?}"
        ))),
    }
}

pub(in crate::engine) fn remote_sharded_insert_on_conflict_is_shard_local(
    on_conflict: &aiondb_plan::InsertOnConflict,
    table: &aiondb_catalog::TableDescriptor,
    shard_config: &aiondb_catalog::CatalogShardConfig,
) -> bool {
    let conflict_covers_shard_key = shard_config.shard_key_columns.iter().all(|shard_key| {
        on_conflict
            .columns
            .iter()
            .any(|conflict_column| conflict_column.eq_ignore_ascii_case(shard_key))
    });
    if !conflict_covers_shard_key {
        return false;
    }

    match &on_conflict.action {
        OnConflictActionPlan::DoNothing => true,
        OnConflictActionPlan::DoUpdate { assignments, .. } => {
            let shard_key_ordinals = shard_config
                .shard_key_columns
                .iter()
                .filter_map(|name| {
                    table
                        .columns
                        .iter()
                        .position(|column| column.name.eq_ignore_ascii_case(name))
                })
                .collect::<BTreeSet<_>>();
            shard_key_ordinals.len() == shard_config.shard_key_columns.len()
                && !assignments
                    .iter()
                    .any(|assignment| shard_key_ordinals.contains(&assignment.column_ordinal))
        }
    }
}

pub(in crate::engine) fn table_column_ordinal(
    table: &aiondb_catalog::TableDescriptor,
    column_name: &str,
    source: &str,
) -> DbResult<usize> {
    table
        .columns
        .iter()
        .position(|column| column.name.eq_ignore_ascii_case(column_name))
        .ok_or_else(|| {
            DbError::internal(format!(
                "{source} column \"{column_name}\" is missing from table {}",
                table.name
            ))
        })
}

pub(in crate::engine) fn compat_relation_runtime_width(table: &aiondb_catalog::TableDescriptor) -> usize {
    const SYSTEM_COLUMNS: [&str; 7] = ["ctid", "tableoid", "xmin", "xmax", "cmin", "cmax", "oid"];

    table.columns.len()
        + SYSTEM_COLUMNS
            .iter()
            .filter(|system_column| {
                table
                    .columns
                    .iter()
                    .all(|column| !column.name.eq_ignore_ascii_case(system_column))
            })
            .count()
}

pub(in crate::engine) fn expr_contains_column_equality(
    expr: &aiondb_plan::TypedExpr,
    left_ordinal: usize,
    right_ordinal: usize,
) -> bool {
    match &expr.kind {
        TypedExprKind::BinaryEq { left, right } => {
            matches_column_equality(left, right, left_ordinal, right_ordinal)
                || matches_column_equality(right, left, left_ordinal, right_ordinal)
        }
        TypedExprKind::LogicalAnd { left, right } => {
            expr_contains_column_equality(left, left_ordinal, right_ordinal)
                || expr_contains_column_equality(right, left_ordinal, right_ordinal)
        }
        _ => false,
    }
}

pub(in crate::engine) fn matches_column_equality(
    left: &aiondb_plan::TypedExpr,
    right: &aiondb_plan::TypedExpr,
    left_ordinal: usize,
    right_ordinal: usize,
) -> bool {
    column_ref_ordinal(left) == Some(left_ordinal)
        && column_ref_ordinal(right) == Some(right_ordinal)
}

pub(in crate::engine) fn column_ref_ordinal(expr: &aiondb_plan::TypedExpr) -> Option<usize> {
    match &expr.kind {
        TypedExprKind::ColumnRef { ordinal, .. } => Some(*ordinal),
        _ => None,
    }
}

pub(in crate::engine) fn evaluate_simple_insert_select_assignment(
    expr: &aiondb_plan::TypedExpr,
    row: &Row,
) -> DbResult<Value> {
    ensure_insert_select_assignment_columns_exist(expr, row)?;
    ExpressionEvaluator.evaluate_with_row(expr, row)
}

pub(in crate::engine) fn ensure_insert_select_assignment_columns_exist(
    expr: &aiondb_plan::TypedExpr,
    row: &Row,
) -> DbResult<()> {
    match &expr.kind {
        TypedExprKind::ColumnRef { ordinal, .. } => {
            if *ordinal >= row.values.len() {
                return Err(DbError::internal(format!(
                    "remote sharded INSERT SELECT source column ordinal {ordinal} is out of range"
                )));
            }
            Ok(())
        }
        TypedExprKind::BinaryEq { left, right }
        | TypedExprKind::BinaryNe { left, right }
        | TypedExprKind::BinaryGe { left, right }
        | TypedExprKind::BinaryGt { left, right }
        | TypedExprKind::BinaryLe { left, right }
        | TypedExprKind::BinaryLt { left, right }
        | TypedExprKind::LogicalAnd { left, right }
        | TypedExprKind::LogicalOr { left, right }
        | TypedExprKind::ArithAdd { left, right }
        | TypedExprKind::ArithSub { left, right }
        | TypedExprKind::ArithMul { left, right }
        | TypedExprKind::ArithDiv { left, right }
        | TypedExprKind::ArithMod { left, right }
        | TypedExprKind::Concat { left, right }
        | TypedExprKind::JsonGet { left, right }
        | TypedExprKind::JsonGetText { left, right }
        | TypedExprKind::JsonPathGet { left, right }
        | TypedExprKind::JsonPathGetText { left, right }
        | TypedExprKind::JsonContains { left, right }
        | TypedExprKind::JsonContainedBy { left, right }
        | TypedExprKind::JsonKeyExists { left, right }
        | TypedExprKind::JsonAnyKeyExists { left, right }
        | TypedExprKind::JsonAllKeysExist { left, right }
        | TypedExprKind::ArrayConcat { left, right }
        | TypedExprKind::ArrayContains { left, right }
        | TypedExprKind::ArrayContainedBy { left, right }
        | TypedExprKind::ArrayOverlap { left, right }
        | TypedExprKind::IsDistinctFrom { left, right, .. }
        | TypedExprKind::Nullif { left, right } => {
            ensure_insert_select_assignment_columns_exist(left, row)?;
            ensure_insert_select_assignment_columns_exist(right, row)
        }
        TypedExprKind::LogicalNot { expr }
        | TypedExprKind::Negate { expr }
        | TypedExprKind::IsNull { expr, .. }
        | TypedExprKind::Cast { expr, .. } => {
            ensure_insert_select_assignment_columns_exist(expr, row)
        }
        TypedExprKind::Like { expr, pattern, .. } => {
            ensure_insert_select_assignment_columns_exist(expr, row)?;
            ensure_insert_select_assignment_columns_exist(pattern, row)
        }
        TypedExprKind::InList { expr, list, .. } => {
            ensure_insert_select_assignment_columns_exist(expr, row)?;
            list.iter()
                .try_for_each(|item| ensure_insert_select_assignment_columns_exist(item, row))
        }
        TypedExprKind::Between {
            expr, low, high, ..
        } => {
            ensure_insert_select_assignment_columns_exist(expr, row)?;
            ensure_insert_select_assignment_columns_exist(low, row)?;
            ensure_insert_select_assignment_columns_exist(high, row)
        }
        TypedExprKind::CaseWhen {
            conditions,
            results,
            else_result,
        } => {
            conditions.iter().try_for_each(|condition| {
                ensure_insert_select_assignment_columns_exist(condition, row)
            })?;
            results.iter().try_for_each(|result| {
                ensure_insert_select_assignment_columns_exist(result, row)
            })?;
            if let Some(else_result) = else_result {
                ensure_insert_select_assignment_columns_exist(else_result, row)?;
            }
            Ok(())
        }
        TypedExprKind::Coalesce { args }
        | TypedExprKind::ScalarFunction { args, .. }
        | TypedExprKind::ArrayConstruct { elements: args }
        | TypedExprKind::UserFunction { args, .. } => args
            .iter()
            .try_for_each(|arg| ensure_insert_select_assignment_columns_exist(arg, row)),
        TypedExprKind::InSubquery { expr, .. } => {
            ensure_insert_select_assignment_columns_exist(expr, row)
        }
        TypedExprKind::WindowFunction {
            args,
            partition_by,
            order_by,
            ..
        } => {
            args.iter()
                .try_for_each(|arg| ensure_insert_select_assignment_columns_exist(arg, row))?;
            partition_by
                .iter()
                .try_for_each(|arg| ensure_insert_select_assignment_columns_exist(arg, row))?;
            order_by
                .iter()
                .try_for_each(|sort| ensure_insert_select_assignment_columns_exist(&sort.expr, row))
        }
        TypedExprKind::AggCount { expr, filter, .. } => {
            if let Some(expr) = expr {
                ensure_insert_select_assignment_columns_exist(expr, row)?;
            }
            if let Some(filter) = filter {
                ensure_insert_select_assignment_columns_exist(filter, row)?;
            }
            Ok(())
        }
        TypedExprKind::AggSum { expr, filter, .. }
        | TypedExprKind::AggAvg { expr, filter, .. }
        | TypedExprKind::AggAnyValue { expr, filter }
        | TypedExprKind::AggMin { expr, filter }
        | TypedExprKind::AggMax { expr, filter }
        | TypedExprKind::AggBoolAnd { expr, filter }
        | TypedExprKind::AggBoolOr { expr, filter }
        | TypedExprKind::AggStddevPop { expr, filter }
        | TypedExprKind::AggStddevSamp { expr, filter }
        | TypedExprKind::AggVarPop { expr, filter }
        | TypedExprKind::AggVarSamp { expr, filter } => {
            ensure_insert_select_assignment_columns_exist(expr, row)?;
            if let Some(filter) = filter {
                ensure_insert_select_assignment_columns_exist(filter, row)?;
            }
            Ok(())
        }
        TypedExprKind::AggStringAgg {
            expr,
            delimiter,
            filter,
            ..
        } => {
            ensure_insert_select_assignment_columns_exist(expr, row)?;
            ensure_insert_select_assignment_columns_exist(delimiter, row)?;
            if let Some(filter) = filter {
                ensure_insert_select_assignment_columns_exist(filter, row)?;
            }
            Ok(())
        }
        TypedExprKind::AggArrayAgg { expr, filter, .. } => {
            ensure_insert_select_assignment_columns_exist(expr, row)?;
            if let Some(filter) = filter {
                ensure_insert_select_assignment_columns_exist(filter, row)?;
            }
            Ok(())
        }
        _ => Ok(()),
    }
}

pub(in crate::engine) fn append_query_result(
    result: ExecutionResult,
    expected_columns: &mut Option<Vec<ResultField>>,
    target_rows: &mut Vec<Row>,
    source: &str,
) -> DbResult<()> {
    match result {
        ExecutionResult::Query { columns, mut rows } => {
            if let Some(expected_columns) = expected_columns {
                if expected_columns != &columns {
                    return Err(DbError::internal(format!(
                        "{source} returned incompatible query columns"
                    )));
                }
            } else {
                *expected_columns = Some(columns);
            }
            target_rows.append(&mut rows);
            Ok(())
        }
        other => Err(DbError::internal(format!(
            "{source} returned non-query result: {other:?}"
        ))),
    }
}

pub(in crate::engine) fn merge_distributed_aggregate_output_state(
    state: &mut DistributedAggregateMergeState,
    output_plan: &DistributedAggregateOutputPlan,
    source_values: &[Value],
) -> DbResult<()> {
    match (state, output_plan) {
        (
            DistributedAggregateMergeState::Value(_),
            DistributedAggregateOutputPlan::GroupKey { .. },
        ) => Ok(()),
        (
            DistributedAggregateMergeState::Value(value),
            DistributedAggregateOutputPlan::Aggregate { kind, source_index },
        ) => {
            let source_value = source_values.get(*source_index).ok_or_else(|| {
                DbError::internal("distributed aggregate source index out of range")
            })?;
            merge_distributed_scalar_aggregate_value(value, *kind, source_value.clone())
        }
        (
            DistributedAggregateMergeState::Avg { sum, count },
            DistributedAggregateOutputPlan::Avg {
                sum_source_index,
                count_source_index,
            },
        ) => {
            let source_sum = source_values.get(*sum_source_index).ok_or_else(|| {
                DbError::internal("distributed AVG sum source index out of range")
            })?;
            if !source_sum.is_null() {
                *sum = Some(distributed_agg_add_value(sum.take(), source_sum)?);
            }
            let source_count = source_values.get(*count_source_index).ok_or_else(|| {
                DbError::internal("distributed AVG count source index out of range")
            })?;
            *count = count.saturating_add(value_to_i64_count(source_count)?);
            Ok(())
        }
        _ => Err(DbError::internal(
            "distributed aggregate merge state does not match output plan",
        )),
    }
}

pub(in crate::engine) fn finalize_distributed_aggregate_state(state: DistributedAggregateMergeState) -> DbResult<Value> {
    match state {
        DistributedAggregateMergeState::Value(value) => Ok(value.unwrap_or(Value::Null)),
        DistributedAggregateMergeState::Avg { sum, count } => finalize_distributed_avg(sum, count),
    }
}

pub(in crate::engine) fn finalize_distributed_avg(sum: Option<Value>, count: i64) -> DbResult<Value> {
    if count <= 0 {
        return Ok(Value::Null);
    }
    let Some(sum) = sum else {
        return Ok(Value::Null);
    };
    match sum {
        Value::Int(value) => {
            let numerator = NumericValue::from_i32(value);
            let divisor = NumericValue::from_i64(count);
            Ok(numerator
                .div_with_scale(&divisor, 16)
                .map(Value::Numeric)
                .unwrap_or(Value::Null))
        }
        Value::BigInt(value) => {
            let numerator = NumericValue::from_i64(value);
            let divisor = NumericValue::from_i64(count);
            Ok(numerator
                .div_with_scale(&divisor, 16)
                .map(Value::Numeric)
                .unwrap_or(Value::Null))
        }
        Value::Numeric(value) => {
            let divisor = NumericValue::from_i64(count);
            Ok(value
                .div(&divisor)
                .map(Value::Numeric)
                .unwrap_or(Value::Null))
        }
        Value::Interval(value) => Ok(Value::Interval(aiondb_eval::scale_interval(
            &value,
            1.0 / i64_to_f64(count),
        )?)),
        other => Ok(Value::Double(
            value_to_f64_for_distributed_avg(&other)? / i64_to_f64(count),
        )),
    }
}

pub(in crate::engine) fn value_to_i64_count(value: &Value) -> DbResult<i64> {
    match value {
        Value::BigInt(value) => Ok(*value),
        Value::Int(value) => Ok(i64::from(*value)),
        other => Err(DbError::internal(format!(
            "distributed AVG count produced non-integer value: {other:?}"
        ))),
    }
}

pub(in crate::engine) fn value_to_f64_for_distributed_avg(value: &Value) -> DbResult<f64> {
    match value {
        Value::Int(value) => Ok(f64::from(*value)),
        Value::BigInt(value) => Ok(i64_to_f64(*value)),
        Value::Real(value) => Ok(f64::from(*value)),
        Value::Double(value) => Ok(*value),
        Value::Numeric(value) => Ok(value.to_f64()),
        Value::Null => Ok(0.0),
        other => Err(DbError::internal(format!(
            "cannot convert {:?} to double for distributed AVG",
            other.data_type()
        ))),
    }
}

pub(in crate::engine) fn strip_sharded_aggregate_global_bounds(
    physical_plan: &aiondb_plan::PhysicalPlan,
) -> aiondb_plan::PhysicalPlan {
    match physical_plan {
        aiondb_plan::PhysicalPlan::Aggregate {
            table_id,
            group_by,
            grouping_sets,
            aggregates,
            having,
            filter,
            distinct,
            distinct_on,
            access_path,
            ..
        } => aiondb_plan::PhysicalPlan::Aggregate {
            table_id: *table_id,
            group_by: group_by.clone(),
            grouping_sets: grouping_sets.clone(),
            aggregates: aggregates.clone(),
            having: having.clone(),
            filter: filter.clone(),
            order_by: Vec::new(),
            limit: None,
            offset: None,
            distinct: *distinct,
            distinct_on: distinct_on.clone(),
            access_path: access_path.clone(),
        },
        other => other.clone(),
    }
}

pub(in crate::engine) fn merge_distributed_scalar_aggregate_value(
    merged: &mut Option<Value>,
    kind: DistributedScalarAggregateKind,
    value: Value,
) -> DbResult<()> {
    match kind {
        DistributedScalarAggregateKind::Count => {
            let current = match merged.take().unwrap_or(Value::BigInt(0)) {
                Value::BigInt(value) => value,
                Value::Int(value) => i64::from(value),
                other => {
                    return Err(DbError::internal(format!(
                        "distributed count merge state is non-integer: {other:?}"
                    )));
                }
            };
            let next = match value {
                Value::BigInt(value) => value,
                Value::Int(value) => i64::from(value),
                other => {
                    return Err(DbError::internal(format!(
                        "distributed count aggregate produced non-integer value: {other:?}"
                    )));
                }
            };
            *merged = Some(Value::BigInt(current.saturating_add(next)));
        }
        DistributedScalarAggregateKind::Sum => {
            if !value.is_null() {
                *merged = Some(distributed_agg_add_value(merged.take(), &value)?);
            }
        }
        DistributedScalarAggregateKind::Min => {
            if !value.is_null() {
                match merged.as_ref() {
                    Some(current)
                        if compare_runtime_values(&value, current)?
                            .unwrap_or(std::cmp::Ordering::Equal)
                            != std::cmp::Ordering::Less => {}
                    _ => *merged = Some(value),
                }
            }
        }
        DistributedScalarAggregateKind::Max => {
            if !value.is_null() {
                match merged.as_ref() {
                    Some(current)
                        if compare_runtime_values(&value, current)?
                            .unwrap_or(std::cmp::Ordering::Equal)
                            != std::cmp::Ordering::Greater => {}
                    _ => *merged = Some(value),
                }
            }
        }
    }
    Ok(())
}

pub(in crate::engine) fn distributed_agg_add_value(current: Option<Value>, new_val: &Value) -> DbResult<Value> {
    match current {
        None => Ok(new_val.clone()),
        Some(cur) => match (&cur, new_val) {
            (Value::Int(a), Value::Int(b)) => match a.checked_add(*b) {
                Some(value) => Ok(Value::Int(value)),
                None => Ok(Value::BigInt(i64::from(*a) + i64::from(*b))),
            },
            (Value::Int(a), Value::BigInt(b)) | (Value::BigInt(b), Value::Int(a)) => i64::from(*a)
                .checked_add(*b)
                .map(Value::BigInt)
                .ok_or_else(|| DbError::internal("integer overflow in distributed SUM merge")),
            (Value::BigInt(a), Value::BigInt(b)) => a
                .checked_add(*b)
                .map(Value::BigInt)
                .ok_or_else(|| DbError::internal("integer overflow in distributed SUM merge")),
            (Value::Real(a), Value::Real(b)) => {
                let value = a + b;
                if value.is_infinite() && !a.is_infinite() && !b.is_infinite() {
                    return Err(DbError::internal(
                        "value out of range: overflow in distributed SUM merge",
                    ));
                }
                Ok(Value::Real(value))
            }
            (Value::Double(a), Value::Double(b)) => {
                let value = a + b;
                if value.is_infinite() && !a.is_infinite() && !b.is_infinite() {
                    return Err(DbError::internal(
                        "value out of range: overflow in distributed SUM merge",
                    ));
                }
                Ok(Value::Double(value))
            }
            (Value::Int(a), Value::Real(b)) | (Value::Real(b), Value::Int(a)) => {
                Ok(Value::Double(f64::from(*a) + f64::from(*b)))
            }
            (Value::Int(a), Value::Double(b)) | (Value::Double(b), Value::Int(a)) => {
                Ok(Value::Double(f64::from(*a) + b))
            }
            (Value::BigInt(a), Value::Double(b)) | (Value::Double(b), Value::BigInt(a)) => {
                Ok(Value::Double(i64_to_f64(*a) + b))
            }
            (Value::BigInt(a), Value::Real(b)) | (Value::Real(b), Value::BigInt(a)) => {
                Ok(Value::Double(i64_to_f64(*a) + f64::from(*b)))
            }
            (Value::Real(a), Value::Double(b)) | (Value::Double(b), Value::Real(a)) => {
                Ok(Value::Double(f64::from(*a) + b))
            }
            (Value::Numeric(a), Value::Numeric(b)) => Ok(Value::Numeric(a.add(b))),
            (Value::Interval(a), Value::Interval(b)) => {
                let months = a.months.checked_add(b.months).ok_or_else(|| {
                    DbError::internal("interval field value out of range in distributed SUM merge")
                })?;
                let days = a.days.checked_add(b.days).ok_or_else(|| {
                    DbError::internal("interval field value out of range in distributed SUM merge")
                })?;
                let micros = a.micros.checked_add(b.micros).ok_or_else(|| {
                    DbError::internal("interval field value out of range in distributed SUM merge")
                })?;
                Ok(Value::Interval(IntervalValue::new(months, days, micros)))
            }
            _ => Err(DbError::internal(format!(
                "cannot merge distributed SUM values of type {:?} and {:?}",
                cur.data_type(),
                new_val.data_type()
            ))),
        },
    }
}

pub(in crate::engine) fn deadline_epoch_ms(deadline: Instant) -> u64 {
    let now = Instant::now();
    let remaining = deadline.saturating_duration_since(now);
    let epoch = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    u64::try_from((epoch + remaining).as_millis()).unwrap_or(u64::MAX)
}

/// `EXPLAIN <utility>` should fail with `feature_not_supported` rather
/// than render a plan / proceed to execution. PG's behaviour is to
/// reject "EXPLAIN cannot be used with statements of this kind". We
/// emit the offending tag so callers / tests can match on it. Returns
/// `None` for statements that EXPLAIN supports.
pub(in crate::engine) fn explain_unsupported_inner_tag(statement: &Statement) -> Option<&'static str> {
    match statement {
        Statement::Lock(_) => Some("LOCK"),
        Statement::Discard(_) => Some("DISCARD"),
        Statement::Listen { .. } => Some("LISTEN"),
        Statement::Unlisten { .. } => Some("UNLISTEN"),
        Statement::Notify { .. } => Some("NOTIFY"),
        _ => None,
    }
}

/// Check if an error indicates an unsupported Cypher feature that should
/// trigger fallback to SQL translation.
pub(in crate::engine) fn is_unsupported_cypher_feature(err: &DbError) -> bool {
    err.sqlstate() == SqlState::FeatureNotSupported
}

pub(in crate::engine) fn is_transaction_not_active_in_storage_error(err: &DbError) -> bool {
    err.report()
        .message
        .contains("transaction is not active in storage")
}

pub(in crate::engine) fn insert_values_storage_autocommit_candidate(statement: &Statement) -> bool {
    let Statement::Insert(insert) = statement else {
        return false;
    };
    insert.query.is_none()
        && !insert.rows.is_empty()
        && insert.on_conflict.is_none()
        && insert.returning.is_empty()
}

pub(in crate::engine) fn statement_needs_explicit_txn_participant_enrollment(statement: &Statement) -> bool {
    !matches!(
        statement,
        Statement::Begin { .. }
            | Statement::Commit { .. }
            | Statement::Rollback { .. }
            | Statement::PrepareTransaction { .. }
            | Statement::CommitPrepared { .. }
            | Statement::RollbackPrepared { .. }
            | Statement::SecurityLabel(_)
            | Statement::SetTransaction(_)
            | Statement::SetSessionCharacteristics(_)
            | Statement::SetVariable(_)
            | Statement::ShowVariable(_)
            | Statement::ResetVariable(_)
    )
}

pub(in crate::engine) fn statement_is_read_only_safe(statement: &Statement) -> bool {
    match statement {
        Statement::Begin { .. }
        | Statement::Commit { .. }
        | Statement::Rollback { .. }
        | Statement::SecurityLabel(_)
        | Statement::Savepoint { .. }
        | Statement::RollbackToSavepoint { .. }
        | Statement::ReleaseSavepoint { .. }
        | Statement::SetTransaction(_)
        | Statement::SetSessionCharacteristics(_)
        | Statement::SetVariable(_)
        | Statement::ShowVariable(_)
        | Statement::ResetVariable(_)
        | Statement::SetTenant { .. }
        | Statement::Lock(_)
        | Statement::Select(_)
        | Statement::SetOperation(_) => true,
        Statement::Copy(copy) => matches!(copy.direction, aiondb_parser::CopyDirection::To),
        Statement::Explain {
            analyze,
            statement: inner,
            ..
        } => !*analyze || statement_is_read_only_safe(inner),
        _ => false,
    }
}

pub(in crate::engine) fn update_targets_pg_catalog_virtual_relation(statement: &Statement) -> bool {
    let Statement::Update(update) = statement else {
        return false;
    };
    let parts = &update.table.parts;
    matches!(
        parts.as_slice(),
        [name] if name.eq_ignore_ascii_case("pg_class")
    ) || matches!(
        parts.as_slice(),
        [schema, name]
            if schema.eq_ignore_ascii_case("pg_catalog")
                && name.eq_ignore_ascii_case("pg_class")
    )
}

pub(in crate::engine) fn statement_requires_implicit_transaction_for_ddl(statement: &Statement) -> bool {
    matches!(
        statement,
        Statement::Analyze { .. }
            | Statement::Vacuum { .. }
            | Statement::CreateTable(_)
            | Statement::CreateTableAs(_)
            | Statement::CreateSequence(_)
            | Statement::CreateIndex(_)
            | Statement::TruncateTable(_)
            | Statement::DropTable(_)
            | Statement::DropIndex(_)
            | Statement::DropSequence(_)
            | Statement::AlterTable(_)
            | Statement::CreateView(_)
            | Statement::DropView(_)
            | Statement::CreateSchema(_)
            | Statement::DropSchema(_)
            | Statement::CreateFunction(_)
            | Statement::DropFunction(_)
            | Statement::CreateTrigger(_)
            | Statement::DropTrigger(_)
            | Statement::AlterTriggerRename(_)
            | Statement::CreateExtension(_)
            | Statement::DropExtension(_)
            | Statement::CreateNodeLabel(_)
            | Statement::CreateEdgeLabel(_)
            | Statement::DropNodeLabel(_)
            | Statement::DropEdgeLabel(_)
            | Statement::CreateRole(_)
            | Statement::DropRole(_)
            | Statement::AlterRole(_)
            | Statement::Grant(_)
            | Statement::Revoke(_)
            | Statement::CreateTenant { .. }
            | Statement::DropTenant { .. }
            | Statement::SetTenant { .. }
    )
}

pub(in crate::engine) fn statement_command_tag(statement: &Statement) -> &'static str {
    match statement {
        Statement::AlterTable(_) => "ALTER TABLE",
        Statement::Analyze { .. } => "ANALYZE",
        Statement::Backup { .. } => "BACKUP",
        Statement::Copy(copy) => match copy.direction {
            aiondb_parser::CopyDirection::From => "COPY FROM",
            aiondb_parser::CopyDirection::To => "COPY TO",
        },
        Statement::CreateEdgeLabel(_) => "CREATE EDGE LABEL",
        Statement::CreateExtension(_) => "CREATE EXTENSION",
        Statement::CreateFunction(_) => "CREATE FUNCTION",
        Statement::CreateIndex(_) => "CREATE INDEX",
        Statement::CreateNodeLabel(_) => "CREATE NODE LABEL",
        Statement::CreateRole(_) => "CREATE ROLE",
        Statement::CreateSchema(_) => "CREATE SCHEMA",
        Statement::CreateSequence(_) => "CREATE SEQUENCE",
        Statement::CreateTable(_) => "CREATE TABLE",
        Statement::CreateTableAs(_) => "CREATE TABLE AS",
        Statement::CreateTenant { .. } => "CREATE TENANT",
        Statement::CreateTrigger(_) => "CREATE TRIGGER",
        Statement::CreateView(_) => "CREATE VIEW",
        Statement::Cypher(_) => "CYPHER",
        Statement::Comment(_) => "COMMENT",
        Statement::Delete(_) => "DELETE",
        Statement::DropEdgeLabel(_) => "DROP EDGE LABEL",
        Statement::DropExtension(_) => "DROP EXTENSION",
        Statement::DropFunction(_) => "DROP FUNCTION",
        Statement::DropIndex(_) => "DROP INDEX",
        Statement::DropNodeLabel(_) => "DROP NODE LABEL",
        Statement::DropRole(_) => "DROP ROLE",
        Statement::DropSchema(_) => "DROP SCHEMA",
        Statement::DropSequence(_) => "DROP SEQUENCE",
        Statement::DropTable(_) => "DROP TABLE",
        Statement::DropTenant { .. } => "DROP TENANT",
        Statement::DropTrigger(_) => "DROP TRIGGER",
        Statement::DropView(_) => "DROP VIEW",
        Statement::Grant(_) => "GRANT",
        Statement::Insert(_) => "INSERT",
        Statement::Merge(_) => "MERGE",
        Statement::Restore { .. } => "RESTORE",
        Statement::SecurityLabel(_) => "SECURITY LABEL",
        Statement::Checkpoint { .. } => "CHECKPOINT",
        Statement::PrepareTransaction { .. } => "PREPARE TRANSACTION",
        Statement::PrepareStmt { .. } => "PREPARE",
        Statement::ExecuteStmt { .. } => "EXECUTE",
        Statement::DeallocateStmt { .. } => "DEALLOCATE",
        Statement::DeclareStmt { .. } => "DECLARE CURSOR",
        Statement::FetchStmt { .. } => "FETCH",
        Statement::MoveStmt { .. } => "MOVE",
        Statement::CloseStmt { .. } => "CLOSE CURSOR",
        Statement::CommitPrepared { .. } => "COMMIT PREPARED",
        Statement::RollbackPrepared { .. } => "ROLLBACK PREPARED",
        Statement::Load { .. } => "LOAD",
        Statement::AlterSystem(_) => "ALTER SYSTEM",
        Statement::Discard(_) => "DISCARD",
        Statement::CreateDatabase(_) => "CREATE DATABASE",
        Statement::AlterDatabase(_) => "ALTER DATABASE",
        Statement::DropDatabase(_) => "DROP DATABASE",
        Statement::CreateType(_) => "CREATE TYPE",
        Statement::AlterType(_) => "ALTER TYPE",
        Statement::DropType(_) => "DROP TYPE",
        Statement::CreateDomain(_) => "CREATE DOMAIN",
        Statement::AlterDomain(_) => "ALTER DOMAIN",
        Statement::DropDomain(_) => "DROP DOMAIN",
        Statement::CreateCast(_) => "CREATE CAST",
        Statement::DropCast(_) => "DROP CAST",
        Statement::CreateRule(_) => "CREATE RULE",
        Statement::AlterRule(_) => "ALTER RULE",
        Statement::DropRule(_) => "DROP RULE",
        Statement::CreateOrReplaceCompat(_) => "CREATE OR REPLACE",
        Statement::CreatePolicy(_) => "CREATE POLICY",
        Statement::AlterPolicy(_) => "ALTER POLICY",
        Statement::DropPolicy(_) => "DROP POLICY",
        Statement::CreatePublication(_) => "CREATE PUBLICATION",
        Statement::AlterPublication(_) => "ALTER PUBLICATION",
        Statement::DropPublication(_) => "DROP PUBLICATION",
        Statement::CreateSubscription(_) => "CREATE SUBSCRIPTION",
        Statement::AlterSubscription(_) => "ALTER SUBSCRIPTION",
        Statement::DropSubscription(_) => "DROP SUBSCRIPTION",
        Statement::CreateServer(_) => "CREATE SERVER",
        Statement::AlterServer(_) => "ALTER SERVER",
        Statement::DropServer(_) => "DROP SERVER",
        Statement::CreateUserMapping(_) => "CREATE USER MAPPING",
        Statement::AlterUserMapping(_) => "ALTER USER MAPPING",
        Statement::DropUserMapping(_) => "DROP USER MAPPING",
        Statement::CreateForeignTable(_) => "CREATE FOREIGN TABLE",
        Statement::AlterForeignTable(_) => "ALTER FOREIGN TABLE",
        Statement::DropForeignTable(_) => "DROP FOREIGN TABLE",
        Statement::CreateForeignDataWrapper(_) => "CREATE FOREIGN DATA WRAPPER",
        Statement::AlterForeignDataWrapper(_) => "ALTER FOREIGN DATA WRAPPER",
        Statement::DropForeignDataWrapper(_) => "DROP FOREIGN DATA WRAPPER",
        Statement::CreateCollation(_) => "CREATE COLLATION",
        Statement::AlterCollation(_) => "ALTER COLLATION",
        Statement::DropCollation(_) => "DROP COLLATION",
        Statement::CreateStatistics(_) => "CREATE STATISTICS",
        Statement::CreateTablespace(_) => "CREATE TABLESPACE",
        Statement::DropStatistics(_) => "DROP STATISTICS",
        Statement::AlterStatistics(_) => "ALTER STATISTICS",
        Statement::DropTablespace(_) => "DROP TABLESPACE",
        Statement::AlterTablespace(_) => "ALTER TABLESPACE",
        Statement::CreateAggregate(_) => "CREATE AGGREGATE",
        Statement::DropAggregate(_) => "DROP AGGREGATE",
        Statement::CreateProcedure(_) => "CREATE PROCEDURE",
        Statement::DropProcedure(_) => "DROP PROCEDURE",
        Statement::DropRoutine(_) => "DROP ROUTINE",
        Statement::AlterTriggerCompat(_) => "ALTER TRIGGER",
        Statement::CreateOperator(_) => "CREATE OPERATOR",
        Statement::DropOperator(_) => "DROP OPERATOR",
        Statement::CompatTagged(_) => "COMPAT",
        Statement::CompatTaggedNotice(_) => "COMPAT",
        Statement::PgCompatUtility(s) => {
            let _ = s;
            "PG COMPAT UTILITY"
        }
        Statement::DropOwned(_) => "DROP OWNED",
        Statement::ReassignOwned(_) => "REASSIGN OWNED",
        Statement::Lock(_) => "LOCK TABLE",
        Statement::SetConstraints(_) => "SET CONSTRAINTS",
        Statement::Revoke(_) => "REVOKE",
        Statement::TruncateTable(_) => "TRUNCATE",
        Statement::Update(_) => "UPDATE",
        Statement::Vacuum { .. } => "VACUUM",
        Statement::AlterRole(_) | Statement::AlterRoleRename(_) => "ALTER ROLE",
        Statement::AlterTriggerRename(_) => "ALTER TRIGGER",
        Statement::Explain {
            statement: inner, ..
        } => statement_command_tag(inner),
        _ => "WRITE",
    }
}

pub(in crate::engine) fn statement_is_planner_pg_object_command(statement: &Statement) -> bool {
    matches!(
        statement,
        Statement::CreateType(_)
            | Statement::AlterType(_)
            | Statement::DropType(_)
            | Statement::CreateDomain(_)
            | Statement::AlterDomain(_)
            | Statement::DropDomain(_)
            | Statement::CreateCast(_)
            | Statement::DropCast(_)
            | Statement::CreateRule(_)
            | Statement::AlterRule(_)
            | Statement::DropRule(_)
            | Statement::CreatePolicy(_)
            | Statement::AlterPolicy(_)
            | Statement::DropPolicy(_)
            | Statement::CreatePublication(_)
            | Statement::AlterPublication(_)
            | Statement::DropPublication(_)
            | Statement::CreateSubscription(_)
            | Statement::AlterSubscription(_)
            | Statement::DropSubscription(_)
            | Statement::CreateServer(_)
            | Statement::AlterServer(_)
            | Statement::DropServer(_)
            | Statement::CreateUserMapping(_)
            | Statement::AlterUserMapping(_)
            | Statement::DropUserMapping(_)
            | Statement::CreateForeignTable(_)
            | Statement::AlterForeignTable(_)
            | Statement::DropForeignTable(_)
            | Statement::CreateForeignDataWrapper(_)
            | Statement::AlterForeignDataWrapper(_)
            | Statement::DropForeignDataWrapper(_)
            | Statement::CreateCollation(_)
            | Statement::AlterCollation(_)
            | Statement::DropCollation(_)
            | Statement::CreateStatistics(_)
            | Statement::AlterStatistics(_)
            | Statement::DropStatistics(_)
            | Statement::CreateTablespace(_)
            | Statement::AlterTablespace(_)
            | Statement::DropTablespace(_)
    )
}

/// Map a parsed statement to its required authorization action.
pub(in crate::engine) fn action_for_statement(statement: &Statement) -> Action {
    if let Some(tag) = super::compat::statement_compat_tag(statement) {
        return action_for_compat_command_tag(tag);
    }

    match statement {
        Statement::Select(_) | Statement::SetOperation(_) => Action::Select,
        Statement::Insert(_) => Action::Insert,
        Statement::Update(_) => Action::Update,
        Statement::Delete(_) => Action::Delete,
        Statement::Merge(_) => Action::Execute,
        Statement::Lock(_) => Action::Execute,
        Statement::CreateTable(_)
        | Statement::CreateTableAs(_)
        | Statement::CreateIndex(_)
        | Statement::CreateView(_)
        | Statement::CreateSequence(_)
        | Statement::CreateSchema(_)
        | Statement::CreateRole(_)
        | Statement::CreateNodeLabel(_)
        | Statement::CreateEdgeLabel(_)
        | Statement::CreateFunction(_)
        | Statement::CreateTrigger(_)
        | Statement::CreateExtension(_)
        | Statement::CreateDatabase(_)
        | Statement::CreateType(_)
        | Statement::CreateDomain(_)
        | Statement::CreateCast(_)
        | Statement::CreateRule(_)
        | Statement::CreateOrReplaceCompat(_)
        | Statement::CreatePolicy(_)
        | Statement::CreatePublication(_)
        | Statement::CreateSubscription(_)
        | Statement::CreateServer(_)
        | Statement::CreateUserMapping(_)
        | Statement::CreateForeignTable(_)
        | Statement::CreateForeignDataWrapper(_)
        | Statement::CreateCollation(_)
        | Statement::CreateStatistics(_)
        | Statement::CreateTablespace(_)
        | Statement::CreateAggregate(_)
        | Statement::CreateProcedure(_)
        | Statement::CreateOperator(_) => Action::Create,
        Statement::CompatTagged(s) => action_for_compat_command_tag(&s.tag),
        Statement::CompatTaggedNotice(s) => action_for_compat_command_tag(&s.tag),
        Statement::PgCompatUtility(s) => action_for_compat_command_tag(&s.tag),
        Statement::DropTable(_)
        | Statement::DropIndex(_)
        | Statement::DropView(_)
        | Statement::DropSequence(_)
        | Statement::DropSchema(_)
        | Statement::DropRole(_)
        | Statement::DropNodeLabel(_)
        | Statement::DropEdgeLabel(_)
        | Statement::DropFunction(_)
        | Statement::DropTrigger(_)
        | Statement::DropExtension(_)
        | Statement::DropDatabase(_)
        | Statement::DropType(_)
        | Statement::DropDomain(_)
        | Statement::DropCast(_)
        | Statement::DropRule(_)
        | Statement::DropPolicy(_)
        | Statement::DropPublication(_)
        | Statement::DropSubscription(_)
        | Statement::DropServer(_)
        | Statement::DropUserMapping(_)
        | Statement::DropForeignTable(_)
        | Statement::DropForeignDataWrapper(_)
        | Statement::DropCollation(_)
        | Statement::DropAggregate(_)
        | Statement::DropStatistics(_)
        | Statement::DropTablespace(_)
        | Statement::DropProcedure(_)
        | Statement::DropRoutine(_)
        | Statement::DropOperator(_)
        | Statement::DropOwned(_) => Action::Drop,
        Statement::AlterTable(_)
        | Statement::AlterRole(_)
        | Statement::AlterRoleRename(_)
        | Statement::AlterTriggerRename(_)
        | Statement::AlterDatabase(_)
        | Statement::AlterType(_)
        | Statement::AlterDomain(_)
        | Statement::AlterRule(_)
        | Statement::AlterPolicy(_)
        | Statement::AlterPublication(_)
        | Statement::AlterSubscription(_)
        | Statement::AlterServer(_)
        | Statement::AlterUserMapping(_)
        | Statement::AlterForeignTable(_)
        | Statement::AlterForeignDataWrapper(_)
        | Statement::AlterCollation(_)
        | Statement::AlterStatistics(_)
        | Statement::AlterTablespace(_)
        | Statement::AlterTriggerCompat(_)
        | Statement::AlterSystem(_)
        | Statement::Comment(_)
        | Statement::SecurityLabel(_)
        | Statement::ReassignOwned(_)
        | Statement::SetTransaction(_)
        | Statement::SetSessionCharacteristics(_)
        | Statement::SetConstraints(_)
        | Statement::SetVariable(_)
        | Statement::ResetVariable(_)
        | Statement::SetTenant { .. }
        | Statement::Discard(_) => Action::Alter,
        Statement::Grant(_) | Statement::Revoke(_) => Action::Alter,
        Statement::TruncateTable(_) | Statement::Vacuum { .. } | Statement::Analyze { .. } => {
            Action::Execute
        }
        Statement::Copy(copy) => {
            if let Some(inner) = copy.query.as_ref() {
                action_for_statement(inner)
            } else {
                match copy.direction {
                    CopyDirection::From => Action::Insert,
                    CopyDirection::To => Action::Select,
                }
            }
        }
        Statement::Checkpoint { .. } => Action::Execute,
        Statement::Cypher(_) => Action::Execute,
        Statement::PrepareTransaction { .. }
        | Statement::CommitPrepared { .. }
        | Statement::RollbackPrepared { .. }
        | Statement::DoStmt { .. }
        | Statement::Load { .. } => Action::Execute,
        Statement::Listen { .. } | Statement::Unlisten { .. } | Statement::Notify { .. } => {
            Action::Usage
        }
        Statement::Explain {
            statement: inner, ..
        } => action_for_statement(inner),
        Statement::Backup { .. } | Statement::Restore { .. } => Action::Execute,
        Statement::CreateTenant { .. } | Statement::DropTenant { .. } => Action::Alter,
        // Session management, SET, SHOW, transaction control, etc.
        Statement::Begin { .. }
        | Statement::Commit { .. }
        | Statement::Rollback { .. }
        | Statement::PrepareStmt { .. }
        | Statement::ExecuteStmt { .. }
        | Statement::DeallocateStmt { .. }
        | Statement::DeclareStmt { .. }
        | Statement::FetchStmt { .. }
        | Statement::MoveStmt { .. }
        | Statement::CloseStmt { .. }
        | Statement::Savepoint { .. }
        | Statement::RollbackToSavepoint { .. }
        | Statement::ReleaseSavepoint { .. }
        | Statement::ShowVariable(_) => Action::Usage,
        _ => Action::Usage,
    }
}

pub(in crate::engine) fn action_for_compat_command_tag(tag: &str) -> Action {
    if tag.starts_with("CREATE ") {
        Action::Create
    } else if tag.starts_with("DROP ") {
        Action::Drop
    } else if tag.starts_with("ALTER ") || tag == "GRANT" || tag == "REVOKE" {
        Action::Alter
    } else {
        Action::Usage
    }
}

/// Extract the authorization target from a statement, if applicable.
pub(in crate::engine) fn target_for_statement(_statement: &Statement) -> Option<AccessTarget> {
    // For now, statement-level authorization without specific object targeting.
    // Object-level targeting (specific table/schema) can be added later by
    // inspecting the statement's table references.
    None
}
