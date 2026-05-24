#![allow(
    clippy::cast_lossless,
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    clippy::cast_precision_loss,
    clippy::doc_markdown,
    clippy::float_cmp,
    clippy::items_after_statements,
    clippy::manual_let_else,
    clippy::match_same_arms,
    clippy::map_unwrap_or,
    clippy::missing_errors_doc,
    clippy::must_use_candidate,
    clippy::needless_pass_by_value,
    clippy::redundant_closure_for_method_calls,
    clippy::ref_option,
    clippy::similar_names,
    clippy::single_match_else,
    clippy::too_many_lines,
    clippy::unnecessary_wraps,
    clippy::unused_self,
    clippy::wildcard_imports
)]

pub mod access_path;
pub mod cost;
pub mod distributed;
pub mod graph_optimizer;
mod join_reorder;
mod outer_join_simplify;
pub mod physical_builder;
mod predicate_pushdown;
mod projection_pruning;
pub mod rules;
mod transitive_predicates;

use std::{cell::Cell, sync::Arc};

use aiondb_catalog::IndexKind;
use aiondb_catalog::{
    CatalogReader, IndexDescriptor, QualifiedName, SchemaDescriptor, SequenceDescriptor,
    TableDescriptor, TableStatistics, ViewDescriptor,
};
use aiondb_core::{ColumnId, DataType, DbResult, Value};
use aiondb_core::{IndexId, RelationId, SchemaId, TxnId};
use aiondb_plan::{
    JoinType, LogicalPlan, PhysicalPlan, ProjectionExpr, ResultField, SetOperationType, SortExpr,
    TypedExpr, TypedExprKind,
};

use crate::cost::PlanCost;

use crate::access_path::clear_access_path_meta_cache;

/// Rewrite `COUNT(col)` to `COUNT(*)` whenever `col` is known to be
/// `NOT NULL`. SQL's `COUNT(col)` excludes NULL rows; if the column
/// can never be NULL the two forms produce identical counts. PG's
/// `prepunion.c::reduce_outer_joins` and friends normalise this so
/// the planner's COUNT(*) fast paths (here:
/// `visible_row_count`, `try_count_simple_*`) light up on more
/// queries.
fn rewrite_count_of_notnull_to_count_star(
    aggregates: Vec<ProjectionExpr>,
    table: Option<&TableDescriptor>,
) -> Vec<ProjectionExpr> {
    // Cheap pre-check: do we have any candidate `COUNT(ColumnRef)` to
    // rewrite? Avoid the catalog lookup when nothing applies.
    let has_candidate = aggregates.iter().any(|projection| {
        matches!(
            &projection.expr.kind,
            TypedExprKind::AggCount {
                expr: Some(inner),
                distinct: false,
                filter: None,
            } if matches!(inner.kind, TypedExprKind::ColumnRef { .. })
        )
    });
    if !has_candidate {
        return aggregates;
    }
    let Some(table) = table else {
        return aggregates;
    };
    aggregates
        .into_iter()
        .map(|projection| {
            let TypedExprKind::AggCount {
                expr: Some(inner),
                distinct: false,
                filter: None,
            } = &projection.expr.kind
            else {
                return projection;
            };
            let TypedExprKind::ColumnRef { ordinal, .. } = inner.kind else {
                return projection;
            };
            let Some(column) = table.columns.get(ordinal) else {
                return projection;
            };
            if column.nullable {
                return projection;
            }
            ProjectionExpr {
                field: projection.field.clone(),
                expr: TypedExpr {
                    kind: TypedExprKind::AggCount {
                        expr: None,
                        distinct: false,
                        filter: None,
                    },
                    data_type: projection.expr.data_type.clone(),
                    nullable: projection.expr.nullable,
                },
            }
        })
        .collect()
}
use crate::physical_builder::{
    collect_union_all_append_fragments, exposed_plan_width, remap_projection_expr_for_join_swap,
    remap_sort_expr_for_join_swap, remap_typed_expr_for_join_swap, ExposedJoinSwapPolicy,
    JoinSwapOrdinalRemap, PhysicalBuilder,
};

#[cfg(test)]
pub(crate) use access_path::{
    compare_literal_values, extract_index_access_path, extract_index_lookup_value,
    extract_index_range, RangeConstraint,
};

pub struct OptimizeRequest {
    pub logical_plan: LogicalPlan,
    pub txn_id: TxnId,
}

pub struct Optimizer {
    catalog_reader: Arc<dyn CatalogReader>,
    physical_builder: PhysicalBuilder,
}

#[derive(Clone, Copy, Debug, Default)]
struct OptimizeRuntimeOptions {
    hnsw_ef_search: Option<usize>,
}

struct OptimizedPlan {
    plan: PhysicalPlan,
    exposed_join_swap_remap: Option<JoinSwapOrdinalRemap>,
}

#[inline]
pub(crate) fn usize_to_f64(value: usize) -> f64 {
    value.to_string().parse::<f64>().unwrap_or(f64::MAX)
}

#[inline]
pub(crate) fn u64_to_f64(value: u64) -> f64 {
    value.to_string().parse::<f64>().unwrap_or(f64::MAX)
}

#[inline]
pub(crate) fn i64_to_f64(value: i64) -> f64 {
    value.to_string().parse::<f64>().unwrap_or_else(|_| {
        if value.is_negative() {
            f64::MIN
        } else {
            f64::MAX
        }
    })
}

#[inline]
pub(crate) fn usize_to_i32_saturating(value: usize) -> i32 {
    i32::try_from(value).unwrap_or(i32::MAX)
}

const MAX_OPTIMIZER_FOLD_EXPR_DEPTH: usize = 1024;

thread_local! {
    static OPTIMIZER_FOLD_EXPR_DEPTH: Cell<usize> = const { Cell::new(0) };
}

struct OptimizerFoldExprGuard;

impl OptimizerFoldExprGuard {
    fn enter() -> Option<Self> {
        OPTIMIZER_FOLD_EXPR_DEPTH.with(|depth| {
            let current = depth.get();
            if current >= MAX_OPTIMIZER_FOLD_EXPR_DEPTH {
                None
            } else {
                depth.set(current + 1);
                Some(Self)
            }
        })
    }
}

impl Drop for OptimizerFoldExprGuard {
    fn drop(&mut self) {
        OPTIMIZER_FOLD_EXPR_DEPTH.with(|depth| depth.set(depth.get().saturating_sub(1)));
    }
}

impl OptimizedPlan {
    fn stable(plan: PhysicalPlan) -> Self {
        Self {
            plan,
            exposed_join_swap_remap: None,
        }
    }
}

fn remap_optional_expr_for_join_swap(
    expr: Option<TypedExpr>,
    remap: Option<JoinSwapOrdinalRemap>,
) -> Option<TypedExpr> {
    match remap {
        Some(remap) => expr.map(|expr| remap_typed_expr_for_join_swap(expr, remap)),
        None => expr,
    }
}

fn remap_projection_exprs_for_join_swap(
    projections: Vec<ProjectionExpr>,
    remap: Option<JoinSwapOrdinalRemap>,
) -> Vec<ProjectionExpr> {
    match remap {
        Some(remap) => projections
            .into_iter()
            .map(|projection| remap_projection_expr_for_join_swap(projection, remap))
            .collect(),
        None => projections,
    }
}

fn remap_project_source_distinct_on_exprs(
    exprs: Vec<TypedExpr>,
    outputs: &[ProjectionExpr],
    remap: Option<JoinSwapOrdinalRemap>,
) -> Vec<TypedExpr> {
    exprs
        .into_iter()
        .map(|expr| {
            let remapped = match remap {
                Some(remap) => remap_typed_expr_for_join_swap(expr, remap),
                None => expr,
            };
            rebind_project_source_distinct_on_expr(outputs, &remapped)
                .unwrap_or_else(|| remapped.clone())
        })
        .collect()
}

fn project_table_output_table_ordinals(outputs: &[ProjectionExpr]) -> Option<Vec<usize>> {
    outputs
        .iter()
        .map(|projection| {
            projection
                .expr
                .kind
                .as_column_ref()
                .map(|(_, ordinal)| ordinal)
        })
        .collect()
}

#[derive(Clone)]
struct ParameterizedIndexJoinRightTarget {
    table_id: RelationId,
    filter: Option<TypedExpr>,
    order_by: Vec<SortExpr>,
    limit: Option<TypedExpr>,
    offset: Option<TypedExpr>,
    distinct: bool,
    distinct_on: Vec<TypedExpr>,
    projected_width: Option<usize>,
    table_ordinals: Vec<usize>,
}

fn parameterized_index_join_right_target(
    plan: &PhysicalPlan,
) -> Option<ParameterizedIndexJoinRightTarget> {
    match plan {
        PhysicalPlan::ProjectTable {
            table_id,
            outputs,
            filter,
            order_by,
            limit,
            offset,
            distinct,
            distinct_on,
            ..
        } => Some(ParameterizedIndexJoinRightTarget {
            table_id: *table_id,
            filter: filter.clone(),
            order_by: order_by.clone(),
            limit: limit.clone(),
            offset: offset.clone(),
            distinct: *distinct,
            distinct_on: distinct_on.clone(),
            projected_width: (!outputs.is_empty()).then_some(outputs.len().max(1)),
            table_ordinals: if outputs.is_empty() {
                Vec::new()
            } else {
                project_table_output_table_ordinals(outputs)?
            },
        }),
        PhysicalPlan::SeqScan { table_id } => Some(ParameterizedIndexJoinRightTarget {
            table_id: *table_id,
            filter: None,
            order_by: Vec::new(),
            limit: None,
            offset: None,
            distinct: false,
            distinct_on: Vec::new(),
            projected_width: None,
            table_ordinals: Vec::new(),
        }),
        PhysicalPlan::ProjectSource {
            source,
            outputs,
            filter: None,
            order_by,
            limit: None,
            offset: None,
            distinct: false,
            distinct_on,
        } if order_by.is_empty() && distinct_on.is_empty() => {
            let mut target = parameterized_index_join_right_target(source)?;
            if outputs.is_empty() {
                return Some(target);
            }

            let mut table_ordinals = Vec::with_capacity(outputs.len());
            for output in outputs {
                let (_, source_ordinal) = output.expr.kind.as_column_ref()?;
                if let Some(source_width) = target.projected_width {
                    if source_ordinal >= source_width {
                        return None;
                    }
                }
                let table_ordinal = target
                    .table_ordinals
                    .get(source_ordinal)
                    .copied()
                    .unwrap_or(source_ordinal);
                table_ordinals.push(table_ordinal);
            }
            target.projected_width = Some(outputs.len().max(1));
            target.table_ordinals = table_ordinals;
            Some(target)
        }
        _ => None,
    }
}

fn remap_join_right_projected_ordinals(
    expr: TypedExpr,
    left_width: usize,
    right_table_ordinals: &[usize],
) -> TypedExpr {
    predicate_pushdown::map_ordinals(expr, |ordinal| {
        if ordinal < left_width {
            ordinal
        } else {
            let right_ordinal = ordinal.saturating_sub(left_width);
            left_width
                + right_table_ordinals
                    .get(right_ordinal)
                    .copied()
                    .unwrap_or(right_ordinal)
        }
    })
}

fn remap_join_right_projected_projection(
    projection: ProjectionExpr,
    left_width: usize,
    right_table_ordinals: &[usize],
) -> ProjectionExpr {
    ProjectionExpr {
        field: projection.field,
        expr: remap_join_right_projected_ordinals(
            projection.expr,
            left_width,
            right_table_ordinals,
        ),
    }
}

fn remap_join_right_projected_sort(
    sort: SortExpr,
    left_width: usize,
    right_table_ordinals: &[usize],
) -> SortExpr {
    SortExpr {
        expr: remap_join_right_projected_ordinals(sort.expr, left_width, right_table_ordinals),
        descending: sort.descending,
        nulls_first: sort.nulls_first,
    }
}

fn collect_expr_column_ref_ordinals(
    expr: &TypedExpr,
    ordinals: &mut std::collections::BTreeSet<usize>,
) {
    let mut stack = vec![expr];
    while let Some(expr) = stack.pop() {
        match &expr.kind {
            TypedExprKind::ColumnRef { ordinal, .. } => {
                ordinals.insert(*ordinal);
            }
            _ => {
                predicate_pushdown::for_each_child_expr(expr, &mut |child| {
                    stack.push(child);
                });
            }
        }
    }
}

fn collect_project_table_required_column_ids(
    table: &TableDescriptor,
    outputs: &[ProjectionExpr],
    filter: Option<&TypedExpr>,
    order_by: &[SortExpr],
    distinct_on: &[TypedExpr],
) -> Option<Vec<ColumnId>> {
    let mut ordinals = std::collections::BTreeSet::new();
    for output in outputs {
        collect_expr_column_ref_ordinals(&output.expr, &mut ordinals);
    }
    if let Some(filter) = filter {
        collect_expr_column_ref_ordinals(filter, &mut ordinals);
    }
    for sort in order_by {
        collect_expr_column_ref_ordinals(&sort.expr, &mut ordinals);
    }
    for expr in distinct_on {
        collect_expr_column_ref_ordinals(expr, &mut ordinals);
    }

    ordinals
        .into_iter()
        .map(|ordinal| table.columns.get(ordinal).map(|column| column.column_id))
        .collect()
}

fn collect_aggregate_required_column_ids(
    table: &TableDescriptor,
    group_by: &[TypedExpr],
    aggregates: &[ProjectionExpr],
    having: Option<&TypedExpr>,
    filter: Option<&TypedExpr>,
    order_by: &[SortExpr],
    distinct_on: &[TypedExpr],
) -> Option<Vec<ColumnId>> {
    let mut ordinals = std::collections::BTreeSet::new();
    for expr in group_by {
        collect_expr_column_ref_ordinals(expr, &mut ordinals);
    }
    for aggregate in aggregates {
        collect_expr_column_ref_ordinals(&aggregate.expr, &mut ordinals);
    }
    if let Some(having) = having {
        collect_expr_column_ref_ordinals(having, &mut ordinals);
    }
    if let Some(filter) = filter {
        collect_expr_column_ref_ordinals(filter, &mut ordinals);
    }
    for sort in order_by {
        collect_expr_column_ref_ordinals(&sort.expr, &mut ordinals);
    }
    for expr in distinct_on {
        collect_expr_column_ref_ordinals(expr, &mut ordinals);
    }

    ordinals
        .into_iter()
        .map(|ordinal| table.columns.get(ordinal).map(|column| column.column_id))
        .collect()
}

fn project_source_outputs_are_passthrough(outputs: &[ProjectionExpr]) -> bool {
    outputs.is_empty()
        || outputs
            .iter()
            .all(|projection| matches!(projection.expr.kind, TypedExprKind::ColumnRef { .. }))
}

fn rebind_project_source_distinct_on_expr(
    outputs: &[ProjectionExpr],
    expr: &TypedExpr,
) -> Option<TypedExpr> {
    if let Some((ordinal, output)) = outputs
        .iter()
        .enumerate()
        .find(|(_, output)| output.expr == *expr)
    {
        return Some(TypedExpr::column_ref(
            &output.field.name,
            ordinal,
            output.field.data_type.clone(),
            output.field.nullable,
        ));
    }

    let column_name = match &expr.kind {
        TypedExprKind::ColumnRef { name, .. } | TypedExprKind::OuterColumnRef { name, .. } => name,
        _ => return None,
    };

    let mut matches = outputs
        .iter()
        .enumerate()
        .filter(|(_, output)| projection_matches_distinct_on_name(output, column_name));
    let (ordinal, output) = matches.next()?;
    if matches.next().is_some() {
        return None;
    }

    Some(TypedExpr::column_ref(
        &output.field.name,
        ordinal,
        output.field.data_type.clone(),
        output.field.nullable,
    ))
}

fn projection_matches_distinct_on_name(output: &ProjectionExpr, column_name: &str) -> bool {
    output.field.name.eq_ignore_ascii_case(column_name)
        || matches!(
            &output.expr.kind,
            TypedExprKind::ColumnRef { name, .. } | TypedExprKind::OuterColumnRef { name, .. }
                if name.eq_ignore_ascii_case(column_name)
        )
}

fn remap_sort_exprs_for_join_swap(
    sorts: Vec<SortExpr>,
    remap: Option<JoinSwapOrdinalRemap>,
) -> Vec<SortExpr> {
    match remap {
        Some(remap) => sorts
            .into_iter()
            .map(|sort| remap_sort_expr_for_join_swap(sort, remap))
            .collect(),
        None => sorts,
    }
}

fn remap_exprs_for_join_swap(
    exprs: Vec<TypedExpr>,
    remap: Option<JoinSwapOrdinalRemap>,
) -> Vec<TypedExpr> {
    match remap {
        Some(remap) => exprs
            .into_iter()
            .map(|expr| remap_typed_expr_for_join_swap(expr, remap))
            .collect(),
        None => exprs,
    }
}

fn remap_typed_expr_for_child_join_swaps(
    expr: TypedExpr,
    left_width: usize,
    left_remap: Option<JoinSwapOrdinalRemap>,
    right_width: usize,
    right_remap: Option<JoinSwapOrdinalRemap>,
) -> TypedExpr {
    predicate_pushdown::map_ordinals(expr, |ordinal| {
        if ordinal < left_width {
            left_remap.map_or(ordinal, |remap| remap.remap_ordinal(ordinal))
        } else if ordinal < left_width.saturating_add(right_width) {
            let local_ordinal = ordinal.saturating_sub(left_width);
            let remapped_local =
                right_remap.map_or(local_ordinal, |remap| remap.remap_ordinal(local_ordinal));
            left_width.saturating_add(remapped_local)
        } else {
            ordinal
        }
    })
}

fn remap_optional_expr_for_child_join_swaps(
    expr: Option<TypedExpr>,
    left_width: usize,
    left_remap: Option<JoinSwapOrdinalRemap>,
    right_width: usize,
    right_remap: Option<JoinSwapOrdinalRemap>,
) -> Option<TypedExpr> {
    expr.map(|expr| {
        remap_typed_expr_for_child_join_swaps(
            expr,
            left_width,
            left_remap,
            right_width,
            right_remap,
        )
    })
}

fn remap_projection_exprs_for_child_join_swaps(
    projections: Vec<ProjectionExpr>,
    left_width: usize,
    left_remap: Option<JoinSwapOrdinalRemap>,
    right_width: usize,
    right_remap: Option<JoinSwapOrdinalRemap>,
) -> Vec<ProjectionExpr> {
    projections
        .into_iter()
        .map(|projection| ProjectionExpr {
            field: projection.field,
            expr: remap_typed_expr_for_child_join_swaps(
                projection.expr,
                left_width,
                left_remap,
                right_width,
                right_remap,
            ),
        })
        .collect()
}

fn remap_sort_exprs_for_child_join_swaps(
    sorts: Vec<SortExpr>,
    left_width: usize,
    left_remap: Option<JoinSwapOrdinalRemap>,
    right_width: usize,
    right_remap: Option<JoinSwapOrdinalRemap>,
) -> Vec<SortExpr> {
    sorts
        .into_iter()
        .map(|sort| SortExpr {
            expr: remap_typed_expr_for_child_join_swaps(
                sort.expr,
                left_width,
                left_remap,
                right_width,
                right_remap,
            ),
            descending: sort.descending,
            nulls_first: sort.nulls_first,
        })
        .collect()
}

fn remap_exprs_for_child_join_swaps(
    exprs: Vec<TypedExpr>,
    left_width: usize,
    left_remap: Option<JoinSwapOrdinalRemap>,
    right_width: usize,
    right_remap: Option<JoinSwapOrdinalRemap>,
) -> Vec<TypedExpr> {
    exprs
        .into_iter()
        .map(|expr| {
            remap_typed_expr_for_child_join_swaps(
                expr,
                left_width,
                left_remap,
                right_width,
                right_remap,
            )
        })
        .collect()
}

fn exposed_plan_fields(plan: &PhysicalPlan) -> Vec<ResultField> {
    match plan {
        PhysicalPlan::NestedLoopJoin {
            left,
            right,
            outputs,
            ..
        }
        | PhysicalPlan::HashJoin {
            left,
            right,
            outputs,
            ..
        }
        | PhysicalPlan::MergeJoin {
            left,
            right,
            outputs,
            ..
        } if outputs.is_empty() => {
            let mut fields = exposed_plan_fields(left);
            fields.extend(exposed_plan_fields(right));
            fields
        }
        other => other.output_fields(),
    }
}

fn exposed_plan_field_or_synthetic(fields: &[ResultField], ordinal: usize) -> ResultField {
    fields.get(ordinal).cloned().unwrap_or_else(|| ResultField {
        name: format!("__aiondb_internal_col_{ordinal}"),
        data_type: DataType::Text,
        text_type_modifier: None,
        nullable: true,
    })
}

fn normalized_projection_outputs_for_exposed_child(
    plan: &PhysicalPlan,
    remap: Option<JoinSwapOrdinalRemap>,
    segment_offset: usize,
) -> Vec<ProjectionExpr> {
    let current_fields = exposed_plan_fields(plan);
    let current_width = exposed_plan_width(plan, remap);
    match remap {
        Some(remap) => (0..current_width)
            .map(|original_ordinal| {
                let current_ordinal = remap.remap_ordinal(original_ordinal);
                let field = exposed_plan_field_or_synthetic(&current_fields, current_ordinal);
                ProjectionExpr {
                    expr: TypedExpr::column_ref(
                        &field.name,
                        segment_offset.saturating_add(current_ordinal),
                        field.data_type.clone(),
                        field.nullable,
                    ),
                    field,
                }
            })
            .collect(),
        None => (0..current_width)
            .map(|current_ordinal| {
                let field = exposed_plan_field_or_synthetic(&current_fields, current_ordinal);
                (current_ordinal, field)
            })
            .map(|(current_ordinal, field)| ProjectionExpr {
                expr: TypedExpr::column_ref(
                    &field.name,
                    segment_offset.saturating_add(current_ordinal),
                    field.data_type.clone(),
                    field.nullable,
                ),
                field,
            })
            .collect(),
    }
}

fn identity_projection_outputs_for_join_children(
    left: &PhysicalPlan,
    left_remap: Option<JoinSwapOrdinalRemap>,
    right: &PhysicalPlan,
    right_remap: Option<JoinSwapOrdinalRemap>,
) -> Vec<ProjectionExpr> {
    let left_current_width = exposed_plan_width(left, left_remap);
    let mut outputs = normalized_projection_outputs_for_exposed_child(left, left_remap, 0);
    outputs.extend(normalized_projection_outputs_for_exposed_child(
        right,
        right_remap,
        left_current_width,
    ));
    outputs
}

fn project_source_child_swap_policy(
    parent_policy: ExposedJoinSwapPolicy,
    outputs: &[ProjectionExpr],
) -> ExposedJoinSwapPolicy {
    if outputs.is_empty() {
        match parent_policy {
            ExposedJoinSwapPolicy::AllowMultiRow => ExposedJoinSwapPolicy::SingleRowOnly,
            other => other,
        }
    } else {
        match parent_policy {
            // An explicit projection can absorb a child join remap on its own,
            // even when there is no outer join consuming this ProjectSource.
            ExposedJoinSwapPolicy::Disallow => ExposedJoinSwapPolicy::AllowMultiRow,
            other => other,
        }
    }
}

impl Optimizer {
    pub fn new(catalog_reader: Arc<dyn CatalogReader>) -> Self {
        Self {
            catalog_reader,
            physical_builder: PhysicalBuilder,
        }
    }

    /// Try to create a `NestedLoopIndexJoin` when the right child is a
    /// `ProjectTable` and one of the equi-join key columns has a B-tree
    /// index on the right table.
    fn try_parameterized_index_join(
        &self,
        txn_id: TxnId,
        left: &PhysicalPlan,
        right: &PhysicalPlan,
        join_type: JoinType,
        condition: &Option<TypedExpr>,
        outputs: &[ProjectionExpr],
        filter: &Option<TypedExpr>,
        order_by: &[SortExpr],
        limit: &Option<TypedExpr>,
        offset: &Option<TypedExpr>,
        distinct: bool,
        distinct_on: &[TypedExpr],
        left_width: usize,
    ) -> DbResult<Option<PhysicalPlan>> {
        if !parameterized_index_join_enabled() {
            return Ok(None);
        }

        // Only for Inner, Semi, Anti, Left joins.
        if matches!(join_type, JoinType::Right | JoinType::Full) {
            return Ok(None);
        }

        // Right child must expose a table access directly or through passive
        // projection wrappers. Depending on earlier projection pruning, a
        // full-table child may already have been lowered from ProjectTable to
        // the join leaf SeqScan.
        let Some(right_target) = parameterized_index_join_right_target(right) else {
            return Ok(None);
        };
        let right_table_id = right_target.table_id;

        // Look for a B-tree index on the right table matching a right key.
        let indexes = self.catalog_reader.list_indexes(txn_id, right_table_id)?;
        let table = self
            .catalog_reader
            .get_table_by_id(txn_id, right_table_id)?;
        let Some(table) = table else {
            return Ok(None);
        };
        if !right_target.order_by.is_empty()
            || right_target.limit.is_some()
            || right_target.offset.is_some()
            || right_target.distinct
            || !right_target.distinct_on.is_empty()
        {
            return Ok(None);
        }
        let right_width =
            predicate_pushdown::table_column_count(&self.catalog_reader, txn_id, right_table_id)?
                .unwrap_or_else(|| table.columns.len().max(1));
        let right_table_ordinals = right_target.table_ordinals;
        let right_projected_width = right_target.projected_width.unwrap_or(right_width).max(1);

        let (left_keys, right_keys, residual, output_filter) =
            if let Some((left_keys, right_keys, residual)) =
                physical_builder::extract_equi_join_keys_public(
                    condition.as_ref(),
                    left_width,
                    right_projected_width,
                )
            {
                (left_keys, right_keys, residual, filter.clone())
            } else if let Some((left_keys, right_keys, filter_residual)) =
                physical_builder::extract_equi_join_keys_public(
                    filter.as_ref(),
                    left_width,
                    right_projected_width,
                )
            {
                (left_keys, right_keys, condition.clone(), filter_residual)
            } else {
                return Ok(None);
            };
        if left_keys.is_empty() {
            return Ok(None);
        }

        for (i, &right_key_ordinal) in right_keys.iter().enumerate() {
            // Map right key ordinal to a column_id.
            let right_table_ordinal = right_table_ordinals
                .get(right_key_ordinal)
                .copied()
                .unwrap_or(right_key_ordinal);
            let Some(col) = table.columns.get(right_table_ordinal) else {
                continue;
            };
            let right_col_id = col.column_id;

            // Find a B-tree index whose first key column matches.
            let matching_index = indexes.iter().find(|idx| {
                idx.kind == IndexKind::BTree
                    && !idx.key_columns.is_empty()
                    && idx.key_columns[0].column_id == right_col_id
            });
            let Some(index) = matching_index else {
                continue;
            };

            // Cost comparison: parameterized NLJ vs hash join.
            let left_rows = physical_builder::estimate_plan_rows(left);
            let right_rows = physical_builder::estimate_plan_rows(right);
            let estimated_right_rows_u64 = right_rows.max(1.0).ceil() as u64;
            let estimated_right_bytes = estimated_right_rows_u64.saturating_mul(64);
            let estimated_lookup_selectivity = (1.0 / right_rows.max(1.0)).clamp(1.0e-6, 0.25);
            let param_cost = PlanCost::index_eq_with_selectivity(
                estimated_right_rows_u64,
                estimated_right_bytes,
                estimated_lookup_selectivity,
            );
            let nlij_total = PlanCost(left_rows * param_cost.0);
            let hj_cost = PlanCost::hash_join(left_rows, right_rows);
            let small_outer_index_probe = left_rows <= 256.0;
            let index_join_with_residual_right_filter =
                right_target.filter.is_some() && left_rows > 64.0;

            if index_join_with_residual_right_filter
                || (!small_outer_index_probe && !nlij_total.cheaper_than(hj_cost))
            {
                continue;
            }

            // Build the NestedLoopIndexJoin plan.
            let residual = residual.map(|expr| {
                remap_join_right_projected_ordinals(expr, left_width, &right_table_ordinals)
            });
            let outputs = outputs
                .iter()
                .cloned()
                .map(|projection| {
                    remap_join_right_projected_projection(
                        projection,
                        left_width,
                        &right_table_ordinals,
                    )
                })
                .collect();
            let filter = output_filter.clone().map(|expr| {
                remap_join_right_projected_ordinals(expr, left_width, &right_table_ordinals)
            });
            let order_by = order_by
                .iter()
                .cloned()
                .map(|sort| {
                    remap_join_right_projected_sort(sort, left_width, &right_table_ordinals)
                })
                .collect();
            let limit = limit.clone().map(|expr| {
                remap_join_right_projected_ordinals(expr, left_width, &right_table_ordinals)
            });
            let offset = offset.clone().map(|expr| {
                remap_join_right_projected_ordinals(expr, left_width, &right_table_ordinals)
            });
            let distinct_on = distinct_on
                .iter()
                .cloned()
                .map(|expr| {
                    remap_join_right_projected_ordinals(expr, left_width, &right_table_ordinals)
                })
                .collect();
            return Ok(Some(PhysicalPlan::NestedLoopIndexJoin {
                left: Box::new(left.clone()),
                right_table_id,
                right_index_id: index.index_id,
                right_width,
                outer_key_ordinal: left_keys[i],
                join_type,
                right_filter: right_target.filter.clone(),
                residual,
                outputs,
                filter,
                order_by,
                limit,
                offset,
                distinct,
                distinct_on,
            }));
        }

        Ok(None)
    }

    pub fn optimize(&self, request: OptimizeRequest) -> DbResult<PhysicalPlan> {
        self.optimize_with_runtime_options(request, OptimizeRuntimeOptions::default())
    }

    pub fn optimize_with_hnsw_ef_search(
        &self,
        request: OptimizeRequest,
        hnsw_ef_search: Option<usize>,
    ) -> DbResult<PhysicalPlan> {
        self.optimize_with_runtime_options(request, OptimizeRuntimeOptions { hnsw_ef_search })
    }

    fn optimize_with_runtime_options(
        &self,
        request: OptimizeRequest,
        runtime_options: OptimizeRuntimeOptions,
    ) -> DbResult<PhysicalPlan> {
        clear_access_path_meta_cache();
        let result = self
            .optimize_internal(request, ExposedJoinSwapPolicy::Disallow, runtime_options)?
            .plan;
        clear_access_path_meta_cache();
        Ok(result)
    }

    fn optimize_internal(
        &self,
        request: OptimizeRequest,
        exposed_join_swap_policy: ExposedJoinSwapPolicy,
        runtime_options: OptimizeRuntimeOptions,
    ) -> DbResult<OptimizedPlan> {
        let txn_id = request.txn_id;
        match request.logical_plan {
            LogicalPlan::ProjectTable {
                table_id,
                outputs,
                filter,
                order_by,
                limit,
                offset,
                distinct,
                distinct_on,
            } => {
                if let Some(ref f) = filter {
                    if physical_builder::is_const_expr(f) {
                        let evaluator = aiondb_eval::ExpressionEvaluator;
                        if let Ok(val) = evaluator.evaluate(f) {
                            if matches!(val, Value::Boolean(false) | Value::Null) {
                                return Ok(OptimizedPlan::stable(PhysicalPlan::ProjectOnce {
                                    outputs,
                                    filter: Some(TypedExpr::literal(
                                        Value::Boolean(false),
                                        DataType::Boolean,
                                        false,
                                    )),
                                    order_by,
                                    limit,
                                    offset,
                                    distinct,
                                    distinct_on,
                                }));
                            }
                        }
                    }
                }

                let filter = filter.and_then(simplify_filter);
                if let Some(hnsw_plan) = self.try_hnsw_scan(
                    txn_id,
                    table_id,
                    &outputs,
                    &filter,
                    &order_by,
                    &limit,
                    &offset,
                    distinct,
                    runtime_options.hnsw_ef_search,
                )? {
                    return Ok(OptimizedPlan::stable(hnsw_plan));
                }

                let projected_column_ids = self
                    .catalog_reader
                    .get_table_by_id(txn_id, table_id)?
                    .and_then(|table| {
                        collect_project_table_required_column_ids(
                            &table,
                            &outputs,
                            filter.as_ref(),
                            &order_by,
                            &distinct_on,
                        )
                    });

                Ok(OptimizedPlan::stable(PhysicalPlan::ProjectTable {
                    table_id,
                    outputs,
                    access_path: self.choose_access_path_with_projection(
                        txn_id,
                        table_id,
                        filter.as_ref(),
                        projected_column_ids.as_deref(),
                    )?,
                    filter,
                    order_by,
                    limit,
                    offset,
                    distinct,
                    distinct_on,
                }))
            }
            LogicalPlan::LockingProjectTable {
                table_id,
                outputs,
                filter,
                order_by,
                limit,
                offset,
                distinct,
                distinct_on,
                row_lock,
            } => {
                let filter = filter.and_then(simplify_filter);
                let projected_column_ids = self
                    .catalog_reader
                    .get_table_by_id(txn_id, table_id)?
                    .and_then(|table| {
                        collect_project_table_required_column_ids(
                            &table,
                            &outputs,
                            filter.as_ref(),
                            &order_by,
                            &distinct_on,
                        )
                    });

                Ok(OptimizedPlan::stable(PhysicalPlan::LockingProjectTable {
                    table_id,
                    outputs,
                    access_path: self.choose_access_path_with_projection(
                        txn_id,
                        table_id,
                        filter.as_ref(),
                        projected_column_ids.as_deref(),
                    )?,
                    filter,
                    order_by,
                    limit,
                    offset,
                    distinct,
                    distinct_on,
                    row_lock,
                }))
            }
            LogicalPlan::ProjectSource {
                source,
                outputs,
                filter,
                order_by,
                limit,
                offset,
                distinct,
                distinct_on,
            } => {
                let (source, filter) = if let Some(filter_expr) = filter {
                    if limit.is_none()
                        && offset.is_none()
                        && !distinct
                        && distinct_on.is_empty()
                        && project_source_outputs_are_passthrough(&outputs)
                        && predicate_pushdown::can_push_into_child(&source)
                    {
                        let remapped_filter =
                            predicate_pushdown::rewrite_project_source_refs(filter_expr, &outputs);
                        (
                            Box::new(predicate_pushdown::push_into_child(
                                *source,
                                remapped_filter,
                            )),
                            None,
                        )
                    } else {
                        (source, Some(filter_expr))
                    }
                } else {
                    (source, None)
                };

                // --- Projection pruning ---
                // When the parent has explicit outputs, narrow the
                // child to only the needed columns.
                let source = projection_pruning::try_prune_child_projection(
                    *source,
                    &outputs,
                    &filter,
                    &order_by,
                    &distinct_on,
                );

                // --- Limit pushdown ---
                // When we have LIMIT (and optionally ORDER BY) but no
                // filter/distinct, push them into the child to enable
                // early termination (Top-N instead of full sort).
                let source =
                    if limit.is_some() && filter.is_none() && !distinct && distinct_on.is_empty() {
                        try_push_limit_into_source(source, &order_by, &limit, &offset)
                    } else {
                        source
                    };

                let source_swap_policy =
                    project_source_child_swap_policy(exposed_join_swap_policy, &outputs);
                let optimized_source = self.optimize_internal(
                    OptimizeRequest {
                        logical_plan: source,
                        txn_id,
                    },
                    source_swap_policy,
                    runtime_options,
                )?;
                let source_remap = optimized_source.exposed_join_swap_remap;
                let outputs = remap_projection_exprs_for_join_swap(outputs, source_remap);
                let distinct_on =
                    remap_project_source_distinct_on_exprs(distinct_on, &outputs, source_remap);

                // --- Redundant sort elimination ---
                // If the child already produces output in the required
                // order, skip the sort.
                let order_by = remap_sort_exprs_for_join_swap(order_by, source_remap);
                let order_by = if !order_by.is_empty()
                    && child_satisfies_order(&optimized_source.plan, &order_by)
                {
                    Vec::new()
                } else {
                    order_by
                };

                Ok(OptimizedPlan::stable(PhysicalPlan::ProjectSource {
                    source: Box::new(optimized_source.plan),
                    outputs,
                    filter: remap_optional_expr_for_join_swap(filter, source_remap),
                    order_by,
                    limit: remap_optional_expr_for_join_swap(limit, source_remap),
                    offset: remap_optional_expr_for_join_swap(offset, source_remap),
                    distinct,
                    distinct_on,
                }))
            }
            LogicalPlan::HybridFunctionScan {
                function_name,
                args,
                output_fields,
            } => Ok(OptimizedPlan::stable(PhysicalPlan::HybridFunctionScan {
                function_name,
                args,
                output_fields,
            })),
            LogicalPlan::Aggregate {
                table_id,
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
                let filter = filter.and_then(simplify_filter);
                let having = having.and_then(simplify_filter);

                // MIN/MAX → index scan optimization (planagg.c equivalent).
                // When the query is `SELECT MIN(col1), MAX(col2), ...` with
                // no GROUP BY, no HAVING, no DISTINCT, and every aggregate
                // is a MIN or MAX over a column that has a B-tree index,
                // we rewrite the entire aggregate to a `ProjectOnce` whose
                // outputs are scalar subqueries of the form
                //   SELECT col FROM t ORDER BY col [ASC|DESC] LIMIT 1
                // Each scalar subquery is an O(log N) index probe; the
                // full-scan aggregate is O(N).
                //
                // PG `planagg.c` does the same thing for arbitrary numbers
                // of MIN/MAX aggregates in one query — splitting the
                // aggregate into one ScalarSubquery per aggregate.
                if group_by.is_empty()
                    && grouping_sets.is_empty()
                    && having.is_none()
                    && !distinct
                    && distinct_on.is_empty()
                    && !aggregates.is_empty()
                    && limit.is_none()
                    && offset.is_none()
                    && order_by.is_empty()
                {
                    if let Some(plan) = self.try_minmax_aggregate_index_scan(
                        txn_id,
                        table_id,
                        &aggregates,
                        filter.as_ref(),
                    )? {
                        return Ok(OptimizedPlan::stable(plan));
                    }
                }

                // PG opt: `COUNT(col)` is equivalent to `COUNT(*)` when
                // `col` is declared NOT NULL.  Rewriting in place lets
                // the COUNT(*) fast paths below (visible_row_count,
                // try_count_simple_*) fire on `SELECT COUNT(id)` style
                // queries that PG normalises away in `prepunion.c`.
                let table_for_count_rewrite =
                    self.catalog_reader.get_table_by_id(txn_id, table_id)?;
                let aggregates = rewrite_count_of_notnull_to_count_star(
                    aggregates,
                    table_for_count_rewrite.as_ref(),
                );

                let count_only_filter_scan = group_by.is_empty()
                    && grouping_sets.is_empty()
                    && having.is_none()
                    && !distinct
                    && distinct_on.is_empty()
                    && !aggregates.is_empty()
                    && aggregates.iter().all(|projection| {
                        matches!(
                            &projection.expr.kind,
                            TypedExprKind::AggCount {
                                expr: None,
                                distinct: false,
                                filter: None,
                            }
                        )
                    });
                let count_only_projection: Option<&[ColumnId]> =
                    count_only_filter_scan.then_some(&[]);
                let aggregate_required_column_ids = if count_only_projection.is_some() {
                    None
                } else {
                    self.catalog_reader
                        .get_table_by_id(txn_id, table_id)?
                        .and_then(|table| {
                            collect_aggregate_required_column_ids(
                                &table,
                                &group_by,
                                &aggregates,
                                having.as_ref(),
                                filter.as_ref(),
                                &order_by,
                                &distinct_on,
                            )
                        })
                };
                let projected_column_ids =
                    count_only_projection.or(aggregate_required_column_ids.as_deref());

                // Fall through to normal aggregate plan.
                Ok(OptimizedPlan::stable(PhysicalPlan::Aggregate {
                    table_id,
                    group_by,
                    grouping_sets,
                    aggregates,
                    having,
                    access_path: self.choose_access_path_with_projection(
                        txn_id,
                        table_id,
                        filter.as_ref(),
                        projected_column_ids,
                    )?,
                    filter,
                    order_by,
                    limit,
                    offset,
                    distinct,
                    distinct_on,
                }))
            }
            LogicalPlan::AggregateSource {
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
                let filter = filter.and_then(simplify_filter);
                let having = having.and_then(simplify_filter);
                let (source, filter) = if let Some(filter_expr) = filter {
                    if predicate_pushdown::can_push_into_child(&source) {
                        (
                            Box::new(predicate_pushdown::push_into_child(*source, filter_expr)),
                            None,
                        )
                    } else {
                        (source, Some(filter_expr))
                    }
                } else {
                    (source, None)
                };

                let optimized_source = self.optimize_internal(
                    OptimizeRequest {
                        logical_plan: *source,
                        txn_id,
                    },
                    ExposedJoinSwapPolicy::SingleRowOnly,
                    runtime_options,
                )?;
                let source_remap = optimized_source.exposed_join_swap_remap;

                Ok(OptimizedPlan::stable(PhysicalPlan::AggregateSource {
                    source: Box::new(optimized_source.plan),
                    group_by: remap_exprs_for_join_swap(group_by, source_remap),
                    grouping_sets,
                    aggregates: remap_projection_exprs_for_join_swap(aggregates, source_remap),
                    having: remap_optional_expr_for_join_swap(having, source_remap),
                    filter: remap_optional_expr_for_join_swap(filter, source_remap),
                    order_by: remap_sort_exprs_for_join_swap(order_by, source_remap),
                    limit: remap_optional_expr_for_join_swap(limit, source_remap),
                    offset: remap_optional_expr_for_join_swap(offset, source_remap),
                    distinct,
                    distinct_on: remap_exprs_for_join_swap(distinct_on, source_remap),
                }))
            }
            LogicalPlan::InsertSelect {
                table_id,
                columns,
                assignments,
                source,
                on_conflict,
                returning,
            } => {
                let optimized_source = self.optimize_internal(
                    OptimizeRequest {
                        logical_plan: *source,
                        txn_id,
                    },
                    ExposedJoinSwapPolicy::Disallow,
                    runtime_options,
                )?;

                Ok(OptimizedPlan::stable(PhysicalPlan::InsertSelect {
                    table_id,
                    columns,
                    assignments,
                    source: Box::new(optimized_source.plan),
                    on_conflict,
                    returning,
                }))
            }
            LogicalPlan::SetOperation {
                op,
                all,
                left,
                right,
                output_fields,
                order_by,
                limit,
                offset,
            } => {
                let optimized_left = self.optimize_internal(
                    OptimizeRequest {
                        logical_plan: *left,
                        txn_id,
                    },
                    ExposedJoinSwapPolicy::Disallow,
                    runtime_options,
                )?;
                let optimized_right = self.optimize_internal(
                    OptimizeRequest {
                        logical_plan: *right,
                        txn_id,
                    },
                    ExposedJoinSwapPolicy::Disallow,
                    runtime_options,
                )?;

                let left_plan = optimized_left.plan;
                let right_plan = optimized_right.plan;

                if matches!(op, SetOperationType::Union) && all {
                    // Only attempt flattening when at least one child is
                    // itself a flattenable UNION ALL, avoiding pointless
                    // clone when flattening can't produce > 2 fragments.
                    let child_is_flattenable = |p: &PhysicalPlan| {
                        matches!(
                            p,
                            PhysicalPlan::SetOperation {
                                op: SetOperationType::Union,
                                all: true,
                                order_by,
                                limit,
                                offset,
                                ..
                            } if order_by.is_empty()
                                && limit.is_none()
                                && offset.is_none()
                        )
                    };
                    if child_is_flattenable(&left_plan) || child_is_flattenable(&right_plan) {
                        let mut fragments = Vec::new();
                        collect_union_all_append_fragments(left_plan, &mut fragments);
                        collect_union_all_append_fragments(right_plan, &mut fragments);
                        if fragments.len() > 2 {
                            return Ok(OptimizedPlan::stable(PhysicalPlan::DistributedAppend {
                                fragments,
                                output_fields,
                                order_by,
                                limit,
                                offset,
                            }));
                        }
                        // Flattening didn't produce > 2 fragments;
                        // reconstruct the SetOperation.
                        let mut drain = fragments.into_iter();
                        match (drain.next(), drain.next(), drain.next()) {
                            (Some(left_plan), Some(right_plan), None) => {
                                return Ok(OptimizedPlan::stable(PhysicalPlan::SetOperation {
                                    op,
                                    all,
                                    left: Box::new(left_plan),
                                    right: Box::new(right_plan),
                                    output_fields,
                                    order_by,
                                    limit,
                                    offset,
                                }));
                            }
                            (first, second, third) => {
                                let mut fragments = Vec::new();
                                if let Some(fragment) = first {
                                    fragments.push(fragment);
                                }
                                if let Some(fragment) = second {
                                    fragments.push(fragment);
                                }
                                if let Some(fragment) = third {
                                    fragments.push(fragment);
                                }
                                fragments.extend(drain);
                                return Ok(OptimizedPlan::stable(
                                    PhysicalPlan::DistributedAppend {
                                        fragments,
                                        output_fields,
                                        order_by,
                                        limit,
                                        offset,
                                    },
                                ));
                            }
                        }
                    }
                }

                Ok(OptimizedPlan::stable(PhysicalPlan::SetOperation {
                    op,
                    all,
                    left: Box::new(left_plan),
                    right: Box::new(right_plan),
                    output_fields,
                    order_by,
                    limit,
                    offset,
                }))
            }
            logical_plan @ LogicalPlan::NestedLoopJoin { .. } => {
                // Phase 0: Convert outer joins to inner where WHERE
                // predicates are strict on the nullable side.
                let plan_after_outer_simplify = outer_join_simplify::simplify_outer_joins(
                    logical_plan,
                    &self.catalog_reader,
                    txn_id,
                )?;

                // Phase 0.25: Greedy join reordering for chains of
                // INNER joins (must happen after outer→inner conversion).
                let plan_after_reorder = join_reorder::try_reorder_joins(
                    plan_after_outer_simplify,
                    &self.catalog_reader,
                    txn_id,
                )?;

                // Phase 0.5: Derive transitive predicates from
                // equi-join conditions + single-table equalities.
                let plan_after_transitive =
                    transitive_predicates::enrich_with_transitive_predicates(
                        plan_after_reorder,
                        &self.catalog_reader,
                        txn_id,
                    )?;

                let plan_after_pushdown = predicate_pushdown::push_predicates_into_join(
                    plan_after_transitive,
                    &self.catalog_reader,
                    txn_id,
                )?;

                let plan_after_pushdown = match plan_after_pushdown {
                    LogicalPlan::NestedLoopJoin {
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
                    } => LogicalPlan::NestedLoopJoin {
                        left,
                        right,
                        join_type,
                        condition,
                        outputs,
                        filter: filter.and_then(simplify_filter),
                        order_by,
                        limit,
                        offset,
                        distinct,
                        distinct_on,
                    },
                    other => other,
                };

                if let LogicalPlan::NestedLoopJoin {
                    filter: Some(ref f),
                    ref outputs,
                    ref order_by,
                    ref limit,
                    ref offset,
                    distinct,
                    ref distinct_on,
                    ..
                } = plan_after_pushdown
                {
                    if physical_builder::is_const_expr(f) {
                        let evaluator = aiondb_eval::ExpressionEvaluator;
                        if let Ok(val) = evaluator.evaluate(f) {
                            if matches!(val, Value::Boolean(false) | Value::Null) {
                                return Ok(OptimizedPlan::stable(PhysicalPlan::ProjectOnce {
                                    outputs: outputs.clone(),
                                    filter: Some(TypedExpr::literal(
                                        Value::Boolean(false),
                                        DataType::Boolean,
                                        false,
                                    )),
                                    order_by: order_by.clone(),
                                    limit: limit.clone(),
                                    offset: offset.clone(),
                                    distinct,
                                    distinct_on: distinct_on.clone(),
                                }));
                            }
                        }
                    }
                }

                let plan_to_build = plan_after_pushdown;

                match plan_to_build {
                    LogicalPlan::NestedLoopJoin {
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
                        let logical_input_widths = match (
                            predicate_pushdown::logical_plan_child_width(
                                &left,
                                &self.catalog_reader,
                                txn_id,
                            )?,
                            predicate_pushdown::logical_plan_child_width(
                                &right,
                                &self.catalog_reader,
                                txn_id,
                            )?,
                        ) {
                            (Some(left_width), Some(right_width)) => {
                                Some((left_width, right_width))
                            }
                            _ => None,
                        };
                        let optimized_left = self.optimize_internal(
                            OptimizeRequest {
                                logical_plan: *left,
                                txn_id,
                            },
                            ExposedJoinSwapPolicy::AllowMultiRow,
                            runtime_options,
                        )?;
                        let optimized_right = self.optimize_internal(
                            OptimizeRequest {
                                logical_plan: *right,
                                txn_id,
                            },
                            ExposedJoinSwapPolicy::AllowMultiRow,
                            runtime_options,
                        )?;
                        let left_width = exposed_plan_width(
                            &optimized_left.plan,
                            optimized_left.exposed_join_swap_remap,
                        )
                        .max(logical_input_widths.map_or(0, |(left_width, _)| left_width));
                        let right_width = exposed_plan_width(
                            &optimized_right.plan,
                            optimized_right.exposed_join_swap_remap,
                        )
                        .max(logical_input_widths.map_or(0, |(_, right_width)| right_width));
                        let condition = remap_optional_expr_for_child_join_swaps(
                            condition,
                            left_width,
                            optimized_left.exposed_join_swap_remap,
                            right_width,
                            optimized_right.exposed_join_swap_remap,
                        );
                        let outputs = if outputs.is_empty()
                            && (optimized_left.exposed_join_swap_remap.is_some()
                                || optimized_right.exposed_join_swap_remap.is_some())
                        {
                            identity_projection_outputs_for_join_children(
                                &optimized_left.plan,
                                optimized_left.exposed_join_swap_remap,
                                &optimized_right.plan,
                                optimized_right.exposed_join_swap_remap,
                            )
                        } else {
                            remap_projection_exprs_for_child_join_swaps(
                                outputs,
                                left_width,
                                optimized_left.exposed_join_swap_remap,
                                right_width,
                                optimized_right.exposed_join_swap_remap,
                            )
                        };
                        let filter = remap_optional_expr_for_child_join_swaps(
                            filter,
                            left_width,
                            optimized_left.exposed_join_swap_remap,
                            right_width,
                            optimized_right.exposed_join_swap_remap,
                        );
                        let order_by = remap_sort_exprs_for_child_join_swaps(
                            order_by,
                            left_width,
                            optimized_left.exposed_join_swap_remap,
                            right_width,
                            optimized_right.exposed_join_swap_remap,
                        );
                        let limit = remap_optional_expr_for_child_join_swaps(
                            limit,
                            left_width,
                            optimized_left.exposed_join_swap_remap,
                            right_width,
                            optimized_right.exposed_join_swap_remap,
                        );
                        let offset = remap_optional_expr_for_child_join_swaps(
                            offset,
                            left_width,
                            optimized_left.exposed_join_swap_remap,
                            right_width,
                            optimized_right.exposed_join_swap_remap,
                        );
                        let distinct_on = remap_exprs_for_child_join_swaps(
                            distinct_on,
                            left_width,
                            optimized_left.exposed_join_swap_remap,
                            right_width,
                            optimized_right.exposed_join_swap_remap,
                        );
                        // --- Parameterized NLJ detection ---
                        // When the right child is a ProjectTable and the
                        // equi-join key matches an index on that table,
                        // emit a NestedLoopIndexJoin for O(N * log M).
                        if let Some(param_plan) = self.try_parameterized_index_join(
                            txn_id,
                            &optimized_left.plan,
                            &optimized_right.plan,
                            join_type,
                            &condition,
                            &outputs,
                            &filter,
                            &order_by,
                            &limit,
                            &offset,
                            distinct,
                            &distinct_on,
                            left_width,
                        )? {
                            return Ok(OptimizedPlan {
                                plan: param_plan,
                                exposed_join_swap_remap: None,
                            });
                        }
                        if matches!(join_type, JoinType::Inner)
                            && (!outputs.is_empty()
                                || exposed_join_swap_policy.allows_empty_outputs())
                        {
                            let remap = JoinSwapOrdinalRemap::new(left_width, right_width);
                            let swapped_condition = condition
                                .clone()
                                .map(|expr| remap_typed_expr_for_join_swap(expr, remap));
                            let swapped_outputs = outputs
                                .iter()
                                .cloned()
                                .map(|projection| {
                                    remap_projection_expr_for_join_swap(projection, remap)
                                })
                                .collect::<Vec<_>>();
                            let swapped_filter = filter
                                .clone()
                                .map(|expr| remap_typed_expr_for_join_swap(expr, remap));
                            let swapped_order_by = order_by
                                .iter()
                                .cloned()
                                .map(|sort| remap_sort_expr_for_join_swap(sort, remap))
                                .collect::<Vec<_>>();
                            let swapped_limit = limit
                                .clone()
                                .map(|expr| remap_typed_expr_for_join_swap(expr, remap));
                            let swapped_offset = offset
                                .clone()
                                .map(|expr| remap_typed_expr_for_join_swap(expr, remap));
                            let swapped_distinct_on = distinct_on
                                .iter()
                                .cloned()
                                .map(|expr| remap_typed_expr_for_join_swap(expr, remap))
                                .collect::<Vec<_>>();
                            if let Some(param_plan) = self.try_parameterized_index_join(
                                txn_id,
                                &optimized_right.plan,
                                &optimized_left.plan,
                                join_type,
                                &swapped_condition,
                                &swapped_outputs,
                                &swapped_filter,
                                &swapped_order_by,
                                &swapped_limit,
                                &swapped_offset,
                                distinct,
                                &swapped_distinct_on,
                                right_width,
                            )? {
                                let exposed_join_swap_remap = outputs.is_empty().then_some(remap);
                                return Ok(OptimizedPlan {
                                    plan: param_plan,
                                    exposed_join_swap_remap,
                                });
                            }
                        }

                        let (plan, exposed_join_swap_remap) = self
                            .physical_builder
                            .build_join_from_physical_with_exposed_swap(
                                optimized_left.plan,
                                optimized_right.plan,
                                join_type,
                                condition,
                                outputs,
                                filter,
                                order_by,
                                limit,
                                offset,
                                distinct,
                                distinct_on,
                                exposed_join_swap_policy,
                                logical_input_widths,
                            );

                        Ok(OptimizedPlan {
                            plan,
                            exposed_join_swap_remap,
                        })
                    }
                    other => Ok(OptimizedPlan::stable(self.physical_builder.build(other))),
                }
            }
            LogicalPlan::CypherQuery(plan) => {
                let stats = self.graph_stats_for_cypher_query(txn_id, &plan)?;
                let graph_opt = graph_optimizer::GraphOptimizer::new(stats);
                let optimized = graph_opt.optimize_cypher_query(plan);
                Ok(OptimizedPlan::stable(PhysicalPlan::CypherQuery(Box::new(
                    optimized,
                ))))
            }
            logical_plan => Ok(OptimizedPlan::stable(
                self.physical_builder.build(logical_plan),
            )),
        }
    }

    /// Optimize a Cypher query plan with the provided graph statistics.
    ///
    /// This entry point is for callers that have pre-computed `GraphStats`
    /// (e.g., from adjacency index metadata).
    pub fn optimize_cypher_with_stats(
        &self,
        plan: aiondb_plan::graph::CypherQueryPlan,
        stats: graph_optimizer::GraphStats,
    ) -> PhysicalPlan {
        let graph_opt = graph_optimizer::GraphOptimizer::new(stats);
        let optimized = graph_opt.optimize_cypher_query(plan);
        PhysicalPlan::CypherQuery(Box::new(optimized))
    }

    fn graph_stats_for_cypher_query(
        &self,
        txn_id: TxnId,
        plan: &aiondb_plan::graph::CypherQueryPlan,
    ) -> DbResult<graph_optimizer::GraphStats> {
        let mut stats = graph_optimizer::GraphStats::empty();
        self.collect_graph_stats_for_cypher_query(txn_id, plan, &mut stats)?;
        Ok(stats)
    }

    fn collect_graph_stats_for_cypher_query(
        &self,
        txn_id: TxnId,
        plan: &aiondb_plan::graph::CypherQueryPlan,
        stats: &mut graph_optimizer::GraphStats,
    ) -> DbResult<()> {
        for match_clause in &plan.matches {
            self.collect_graph_stats_for_match(txn_id, match_clause, stats)?;
        }
        for op in &plan.pipeline {
            match op {
                aiondb_plan::graph::CypherPipelineOp::Match(match_clause) => {
                    self.collect_graph_stats_for_match(txn_id, match_clause, stats)?;
                }
                aiondb_plan::graph::CypherPipelineOp::CallSubquery(subquery) => {
                    self.collect_graph_stats_for_cypher_query(txn_id, subquery, stats)?;
                }
                aiondb_plan::graph::CypherPipelineOp::Unwind(_)
                | aiondb_plan::graph::CypherPipelineOp::With(_)
                | aiondb_plan::graph::CypherPipelineOp::ProcedureCall(_)
                | aiondb_plan::graph::CypherPipelineOp::Foreach(_) => {}
            }
        }
        if let Some(union) = &plan.union {
            self.collect_graph_stats_for_cypher_query(txn_id, &union.right, stats)?;
        }
        Ok(())
    }

    fn collect_graph_stats_for_match(
        &self,
        txn_id: TxnId,
        match_clause: &aiondb_plan::graph::CypherMatchClause,
        stats: &mut graph_optimizer::GraphStats,
    ) -> DbResult<()> {
        for pattern in &match_clause.patterns {
            for node in &pattern.nodes {
                self.collect_node_label_stats(txn_id, node, stats)?;
            }
            for rel in &pattern.relationships {
                if let Some(rel_type) = rel.rel_type.as_deref() {
                    self.collect_edge_type_stats(txn_id, rel_type, rel.table_id, stats)?;
                }
                for rel_type in &rel.rel_type_alternatives {
                    self.collect_edge_type_stats(txn_id, rel_type, None, stats)?;
                }
            }
        }
        Ok(())
    }

    /// Project a node label's backing table statistics into graph stats:
    /// label cardinality (row count) plus per-property distinct counts taken
    /// from the table's persisted per-column `ndistinct`. The cost-based
    /// planner uses the latter for real equality selectivity instead of a
    /// generic constant.
    fn collect_node_label_stats(
        &self,
        txn_id: TxnId,
        node: &aiondb_plan::graph::CypherNodePattern,
        stats: &mut graph_optimizer::GraphStats,
    ) -> DbResult<()> {
        let Some(label) = node.label.as_deref() else {
            return Ok(());
        };
        let Some(table_id) = self.node_table_id_for_graph_stats(txn_id, node)? else {
            return Ok(());
        };
        let Some(table_stats) = self.catalog_reader.get_statistics(txn_id, table_id)? else {
            return Ok(());
        };
        stats
            .label_cardinality
            .insert(label.to_owned(), table_stats.row_count);
        if let Some(table) = self.catalog_reader.get_table_by_id(txn_id, table_id)? {
            for column in &table.columns {
                if let Some(ndistinct) = column_ndistinct(&table_stats, column.column_id) {
                    stats
                        .distinct
                        .insert((label.to_owned(), column.name.clone()), ndistinct);
                }
            }
        }
        Ok(())
    }

    /// Project an edge type's backing table statistics into graph stats:
    /// edge cardinality (row count), the declared `(source, target)` endpoint
    /// labels (for the typed-triple estimate), and average in/out degree
    /// derived from the persisted distinct counts of the edge table's
    /// endpoint id columns (`edges / distinct endpoint nodes`).
    fn collect_edge_type_stats(
        &self,
        txn_id: TxnId,
        rel_type: &str,
        table_id_hint: Option<RelationId>,
        stats: &mut graph_optimizer::GraphStats,
    ) -> DbResult<()> {
        let descriptor = self.catalog_reader.get_edge_label(txn_id, rel_type)?;
        if let Some(descriptor) = &descriptor {
            stats.edge_endpoints.insert(
                rel_type.to_owned(),
                (
                    descriptor.source_label.clone(),
                    descriptor.target_label.clone(),
                ),
            );
        }
        let Some(table_id) = table_id_hint.or_else(|| descriptor.as_ref().map(|d| d.table_id))
        else {
            return Ok(());
        };
        let Some(table_stats) = self.catalog_reader.get_statistics(txn_id, table_id)? else {
            return Ok(());
        };
        stats
            .edge_cardinality
            .insert(rel_type.to_owned(), table_stats.row_count);

        let Some(endpoints) = descriptor.as_ref().and_then(|d| d.endpoints.as_ref()) else {
            return Ok(());
        };
        let Some(table) = self.catalog_reader.get_table_by_id(txn_id, table_id)? else {
            return Ok(());
        };
        let row_count = crate::u64_to_f64(table_stats.row_count);
        if let Some(distinct_src) =
            column_ndistinct_by_name(&table, &table_stats, &endpoints.source_id_column)
        {
            stats
                .avg_out_degree
                .insert(rel_type.to_owned(), row_count / distinct_src);
        }
        if let Some(distinct_tgt) =
            column_ndistinct_by_name(&table, &table_stats, &endpoints.target_id_column)
        {
            stats
                .avg_in_degree
                .insert(rel_type.to_owned(), row_count / distinct_tgt);
        }
        Ok(())
    }

    fn node_table_id_for_graph_stats(
        &self,
        txn_id: TxnId,
        node: &aiondb_plan::graph::CypherNodePattern,
    ) -> DbResult<Option<RelationId>> {
        if let Some(table_id) = node.table_id {
            return Ok(Some(table_id));
        }
        let Some(label) = node.label.as_deref() else {
            return Ok(None);
        };
        Ok(self
            .catalog_reader
            .get_node_label(txn_id, label)?
            .map(|descriptor| descriptor.table_id))
    }
}

/// Persisted distinct-value count for a column, or `None` when the column has
/// no usable statistics (`ndistinct == 0` means "unknown", not "zero").
fn column_ndistinct(table_stats: &TableStatistics, column_id: ColumnId) -> Option<f64> {
    let column_stats = table_stats
        .column_stats
        .iter()
        .find(|cs| cs.column_id == column_id)?;
    (column_stats.ndistinct > 0.0).then_some(column_stats.ndistinct)
}

/// Same as [`column_ndistinct`] but resolves the column by name first.
/// Endpoint id columns are matched case-insensitively to mirror SQL
/// identifier folding.
fn column_ndistinct_by_name(
    table: &TableDescriptor,
    table_stats: &TableStatistics,
    name: &str,
) -> Option<f64> {
    let column = table
        .columns
        .iter()
        .find(|c| c.name.eq_ignore_ascii_case(name))?;
    column_ndistinct(table_stats, column.column_id)
}

fn parameterized_index_join_enabled() -> bool {
    std::env::var("AIONDB_DISABLE_PARAMETERIZED_INDEX_JOIN")
        .ok()
        .map_or(true, |value| {
            value == "0"
                || value.eq_ignore_ascii_case("false")
                || value.eq_ignore_ascii_case("no")
                || value.eq_ignore_ascii_case("off")
        })
}

/// Recursively fold integer-arithmetic literal sub-expressions to a
/// single literal. Conservative: only folds Int+Int and BigInt+BigInt
/// for `+`, `-`, `*` (skips division to avoid divide-by-zero foot-guns
/// and integer-truncation surprises) using checked arithmetic so
/// overflow leaves the original expression unchanged.
///
/// Mirrors what Postgres' planner does in `eval_const_expressions`:
/// `WHERE x = 1+2` becomes `WHERE x = 3` once at plan time, instead of
/// re-evaluating the addition for every scanned row.
fn fold_arithmetic_literals(expr: TypedExpr) -> TypedExpr {
    let Some(_guard) = OptimizerFoldExprGuard::enter() else {
        return expr;
    };

    fn fold_int_pair(
        l: &TypedExpr,
        r: &TypedExpr,
        nullable: bool,
        op: fn(i32, i32) -> Option<i32>,
        op64: fn(i64, i64) -> Option<i64>,
        data_type: DataType,
    ) -> Option<TypedExpr> {
        match (&l.kind, &r.kind) {
            (TypedExprKind::Literal(Value::Int(a)), TypedExprKind::Literal(Value::Int(b))) => {
                op(*a, *b).map(|v| TypedExpr::literal(Value::Int(v), data_type, nullable))
            }
            (
                TypedExprKind::Literal(Value::BigInt(a)),
                TypedExprKind::Literal(Value::BigInt(b)),
            ) => op64(*a, *b).map(|v| TypedExpr::literal(Value::BigInt(v), data_type, nullable)),
            _ => None,
        }
    }

    let nullable = expr.nullable;
    let data_type = expr.data_type.clone();
    match expr.kind {
        TypedExprKind::ArithAdd { left, right } => {
            let l = fold_arithmetic_literals(*left);
            let r = fold_arithmetic_literals(*right);
            if let Some(folded) = fold_int_pair(
                &l,
                &r,
                nullable,
                i32::checked_add,
                i64::checked_add,
                data_type.clone(),
            ) {
                folded
            } else {
                TypedExpr {
                    kind: TypedExprKind::ArithAdd {
                        left: Box::new(l),
                        right: Box::new(r),
                    },
                    data_type,
                    nullable,
                }
            }
        }
        TypedExprKind::ArithSub { left, right } => {
            let l = fold_arithmetic_literals(*left);
            let r = fold_arithmetic_literals(*right);
            if let Some(folded) = fold_int_pair(
                &l,
                &r,
                nullable,
                i32::checked_sub,
                i64::checked_sub,
                data_type.clone(),
            ) {
                folded
            } else {
                TypedExpr {
                    kind: TypedExprKind::ArithSub {
                        left: Box::new(l),
                        right: Box::new(r),
                    },
                    data_type,
                    nullable,
                }
            }
        }
        TypedExprKind::ArithMul { left, right } => {
            let l = fold_arithmetic_literals(*left);
            let r = fold_arithmetic_literals(*right);
            if let Some(folded) = fold_int_pair(
                &l,
                &r,
                nullable,
                i32::checked_mul,
                i64::checked_mul,
                data_type.clone(),
            ) {
                folded
            } else {
                TypedExpr {
                    kind: TypedExprKind::ArithMul {
                        left: Box::new(l),
                        right: Box::new(r),
                    },
                    data_type,
                    nullable,
                }
            }
        }
        other => TypedExpr {
            kind: other,
            data_type,
            nullable,
        },
    }
}

/// Simplify obvious tautologies and contradictions in filter expressions.
///
/// Returns `None` only when the input is `None` (no filter). A `Some(expr)`
/// is always returned for non-trivial expressions. Constant TRUE/FALSE
/// leaves are preserved so the caller can detect them.
pub(crate) fn simplify_filter(expr: TypedExpr) -> Option<TypedExpr> {
    let expr = fold_arithmetic_literals(expr);
    match expr.kind {
        TypedExprKind::LogicalAnd { left, right } => {
            let left_simplified = simplify_filter(*left);
            let right_simplified = simplify_filter(*right);
            match (left_simplified, right_simplified) {
                (None, None) => None,
                (Some(l), None) => Some(l),
                (None, Some(r)) => Some(r),
                (Some(l), Some(r)) => {
                    if is_const_true(&l) {
                        return Some(r);
                    }
                    if is_const_true(&r) {
                        return Some(l);
                    }
                    if is_const_false(&l) && is_side_effect_free(&r) {
                        return Some(l);
                    }
                    if is_const_false(&r) && is_side_effect_free(&l) {
                        return Some(r);
                    }
                    if l == r {
                        return Some(l);
                    }
                    Some(TypedExpr::logical_and(l, r))
                }
            }
        }
        TypedExprKind::LogicalOr { left, right } => {
            let left_simplified = simplify_filter(*left);
            let right_simplified = simplify_filter(*right);
            match (left_simplified, right_simplified) {
                (None, None) => None,
                (Some(l), None) => Some(l),
                (None, Some(r)) => Some(r),
                (Some(l), Some(r)) => {
                    if is_const_true(&l) && is_side_effect_free(&r) {
                        return Some(l);
                    }
                    if is_const_true(&r) && is_side_effect_free(&l) {
                        return Some(r);
                    }
                    if is_const_false(&l) {
                        return Some(r);
                    }
                    if is_const_false(&r) {
                        return Some(l);
                    }
                    Some(TypedExpr::logical_or(l, r))
                }
            }
        }
        // NOT NOT x → x (double negation elimination)
        TypedExprKind::LogicalNot { expr: inner }
            if matches!(inner.kind, TypedExprKind::LogicalNot { .. }) =>
        {
            if let TypedExprKind::LogicalNot { expr: inner_inner } = inner.kind {
                simplify_filter(*inner_inner)
            } else {
                unreachable!()
            }
        }
        // De Morgan: NOT (a OR b) → NOT a AND NOT b
        // (beneficial direction: converts OR→AND, enabling conjunct splitting)
        TypedExprKind::LogicalNot { expr: inner }
            if matches!(inner.kind, TypedExprKind::LogicalOr { .. }) =>
        {
            if let TypedExprKind::LogicalOr { left, right } = inner.kind {
                let not_left = TypedExpr::logical_not(*left);
                let not_right = TypedExpr::logical_not(*right);
                simplify_filter(TypedExpr::logical_and(not_left, not_right))
            } else {
                unreachable!()
            }
        }
        // x = x → TRUE when x is not nullable and side-effect free
        TypedExprKind::BinaryEq {
            ref left,
            ref right,
        } if left == right && !left.nullable && is_side_effect_free(left) => Some(
            TypedExpr::literal(Value::Boolean(true), DataType::Boolean, false),
        ),
        // x <> x → FALSE when x is not nullable and side-effect free
        TypedExprKind::BinaryNe {
            ref left,
            ref right,
        } if left == right && !left.nullable && is_side_effect_free(left) => Some(
            TypedExpr::literal(Value::Boolean(false), DataType::Boolean, false),
        ),
        // x IS NULL → FALSE when x is not nullable and side-effect free.
        // Postgres relies on the planner to skip null-checks against
        // NOT NULL columns; without this, every `WHERE x IS NULL` on
        // such a column scans the whole table evaluating a constant
        // FALSE per row.
        TypedExprKind::IsNull {
            ref expr,
            negated: false,
        } if !expr.nullable && is_side_effect_free(expr) => Some(TypedExpr::literal(
            Value::Boolean(false),
            DataType::Boolean,
            false,
        )),
        // x IS NOT NULL → TRUE under the same conditions.
        TypedExprKind::IsNull {
            ref expr,
            negated: true,
        } if !expr.nullable && is_side_effect_free(expr) => Some(TypedExpr::literal(
            Value::Boolean(true),
            DataType::Boolean,
            false,
        )),
        _ => Some(expr),
    }
}

fn is_const_true(expr: &TypedExpr) -> bool {
    matches!(&expr.kind, TypedExprKind::Literal(Value::Boolean(true)))
}

fn is_const_false(expr: &TypedExpr) -> bool {
    matches!(&expr.kind, TypedExprKind::Literal(Value::Boolean(false)))
}

fn is_side_effect_free(expr: &TypedExpr) -> bool {
    match &expr.kind {
        TypedExprKind::Literal(_) => true,
        TypedExprKind::ColumnRef { .. } => true,
        TypedExprKind::LogicalAnd { left, right } | TypedExprKind::LogicalOr { left, right } => {
            is_side_effect_free(left) && is_side_effect_free(right)
        }
        TypedExprKind::LogicalNot { expr: inner } => is_side_effect_free(inner),
        _ => false,
    }
}

// ------------------------------------------------------------------
// Redundant sort elimination
// ------------------------------------------------------------------

/// Check if the child physical plan already produces output in the
/// order required by `required`, making a parent `ORDER BY` redundant.
///
/// Only trusts explicit `ORDER BY` clauses in the child plan (which
/// are exact guarantees), NOT heuristic index-scan ordering from
/// `plan_sorted_prefix`.  Index scans may be ordered on keys that
/// differ from what the parent needs; false matches would drop a
/// required sort and return misordered rows.
fn child_satisfies_order(child: &PhysicalPlan, required: &[SortExpr]) -> bool {
    if required.is_empty() {
        return true;
    }
    // Extract the child's explicit ORDER BY (the only reliable source).
    let Some(child_order_by) = physical_plan_explicit_order_by(child) else {
        return false;
    };
    if child_order_by.len() < required.len() {
        return false;
    }
    required
        .iter()
        .zip(child_order_by.iter())
        .all(|(req, child_sort)| {
            if req.descending != child_sort.descending {
                return false;
            }
            if req.nulls_first != child_sort.nulls_first {
                return false;
            }
            req.expr == child_sort.expr
        })
}

fn physical_plan_explicit_order_by(plan: &PhysicalPlan) -> Option<&[SortExpr]> {
    match plan {
        PhysicalPlan::ProjectOnce { order_by, .. }
        | PhysicalPlan::ProjectTable { order_by, .. }
        | PhysicalPlan::LockingProjectTable { order_by, .. }
        | PhysicalPlan::ProjectSource { order_by, .. }
        | PhysicalPlan::NestedLoopJoin { order_by, .. }
        | PhysicalPlan::NestedLoopIndexJoin { order_by, .. }
        | PhysicalPlan::HashJoin { order_by, .. }
        | PhysicalPlan::MergeJoin { order_by, .. }
        | PhysicalPlan::Aggregate { order_by, .. }
        | PhysicalPlan::AggregateSource { order_by, .. }
        | PhysicalPlan::SetOperation { order_by, .. }
        | PhysicalPlan::DistributedAppend { order_by, .. }
        | PhysicalPlan::ProjectValues { order_by, .. }
        | PhysicalPlan::FinalAggregate { order_by, .. }
            if !order_by.is_empty() =>
        {
            Some(order_by)
        }
        _ => None,
    }
}

// ------------------------------------------------------------------
// Limit pushdown
// ------------------------------------------------------------------

/// Push a parent `ORDER BY` + `LIMIT` (+ optional `OFFSET`) into a
/// child plan node when it is safe to do so.  This enables Top-N
/// execution instead of a full sort at the child level.
///
/// Preconditions (checked by the caller):
/// - Parent has `limit: Some(_)`
/// - Parent has `filter: None`
/// - Parent has `distinct: false` and `distinct_on: []`
fn try_push_limit_into_source(
    source: LogicalPlan,
    parent_order_by: &[SortExpr],
    parent_limit: &Option<TypedExpr>,
    parent_offset: &Option<TypedExpr>,
) -> LogicalPlan {
    match source {
        LogicalPlan::ProjectTable {
            table_id,
            outputs,
            filter,
            order_by: ref child_order_by,
            limit: ref child_limit,
            offset: ref child_offset,
            distinct,
            distinct_on: ref child_distinct_on,
        } if child_limit.is_none()
            && child_order_by.is_empty()
            && child_offset.is_none()
            && !distinct
            && child_distinct_on.is_empty() =>
        {
            let pushed_order_by = match remap_project_source_order_by_for_project_table_pushdown(
                parent_order_by,
                &outputs,
            ) {
                Some(order_by) => order_by,
                None if parent_order_by.is_empty() => Vec::new(),
                None => {
                    return LogicalPlan::ProjectTable {
                        table_id,
                        outputs,
                        filter,
                        order_by: Vec::new(),
                        limit: None,
                        offset: None,
                        distinct,
                        distinct_on: Vec::new(),
                    }
                }
            };
            let effective_limit = compute_effective_limit(parent_limit, parent_offset);
            LogicalPlan::ProjectTable {
                table_id,
                outputs,
                filter,
                order_by: pushed_order_by,
                limit: effective_limit,
                offset: None,
                distinct: false,
                distinct_on: Vec::new(),
            }
        }
        LogicalPlan::ProjectSource {
            source,
            outputs,
            filter: None,
            order_by: ref child_order_by,
            limit: ref child_limit,
            offset: ref child_offset,
            distinct,
            distinct_on: ref child_distinct_on,
        } if child_limit.is_none()
            && child_order_by.is_empty()
            && child_offset.is_none()
            && !distinct
            && child_distinct_on.is_empty() =>
        {
            let pushed_order_by = match remap_project_source_order_by_for_project_table_pushdown(
                parent_order_by,
                &outputs,
            ) {
                Some(order_by) => order_by,
                None if parent_order_by.is_empty() => Vec::new(),
                None => {
                    return LogicalPlan::ProjectSource {
                        source,
                        outputs,
                        filter: None,
                        order_by: Vec::new(),
                        limit: None,
                        offset: None,
                        distinct,
                        distinct_on: Vec::new(),
                    }
                }
            };
            let effective_limit = compute_effective_limit(parent_limit, parent_offset);
            LogicalPlan::ProjectSource {
                source,
                outputs,
                filter: None,
                order_by: pushed_order_by,
                limit: effective_limit,
                offset: None,
                distinct: false,
                distinct_on: Vec::new(),
            }
        }
        other => other,
    }
}

fn remap_project_source_order_by_for_project_table_pushdown(
    parent_order_by: &[SortExpr],
    child_outputs: &[ProjectionExpr],
) -> Option<Vec<SortExpr>> {
    parent_order_by
        .iter()
        .map(|sort| {
            let TypedExprKind::ColumnRef { ordinal, .. } = sort.expr.kind else {
                return None;
            };
            let child_expr = child_outputs.get(ordinal)?.expr.clone();
            Some(SortExpr {
                expr: child_expr,
                descending: sort.descending,
                nulls_first: sort.nulls_first,
            })
        })
        .collect()
}

/// Compute the limit to push into a child: `parent_limit + parent_offset`.
/// The child must produce at least this many rows so the parent can
/// apply the offset and still return `parent_limit` rows.
///
/// Returns `None` (no pushdown) when either value is a non-literal
/// expression - we cannot safely compute the sum at plan time.
fn compute_effective_limit(
    limit: &Option<TypedExpr>,
    offset: &Option<TypedExpr>,
) -> Option<TypedExpr> {
    let limit_val = match limit {
        Some(expr) => match &expr.kind {
            TypedExprKind::Literal(Value::Int(n)) => *n as i64,
            TypedExprKind::Literal(Value::BigInt(n)) => *n,
            // Non-literal limit - cannot safely push down.
            _ => return None,
        },
        None => return None,
    };
    let offset_val = match offset {
        Some(expr) => match &expr.kind {
            TypedExprKind::Literal(Value::Int(n)) => *n as i64,
            TypedExprKind::Literal(Value::BigInt(n)) => *n,
            // Non-literal offset - cannot safely compute effective limit.
            _ => return None,
        },
        None => 0,
    };
    let effective = limit_val.saturating_add(offset_val).max(0);
    Some(TypedExpr::literal(
        Value::BigInt(effective),
        DataType::BigInt,
        false,
    ))
}

impl Default for Optimizer {
    fn default() -> Self {
        Self::new(Arc::new(EmptyCatalog))
    }
}

#[derive(Debug, Default)]
struct EmptyCatalog;

impl CatalogReader for EmptyCatalog {
    fn get_schema(&self, _txn: TxnId, _name: &QualifiedName) -> DbResult<Option<SchemaDescriptor>> {
        Ok(None)
    }

    fn get_table(&self, _txn: TxnId, _name: &QualifiedName) -> DbResult<Option<TableDescriptor>> {
        Ok(None)
    }

    fn get_table_by_id(
        &self,
        _txn: TxnId,
        _table_id: RelationId,
    ) -> DbResult<Option<TableDescriptor>> {
        Ok(None)
    }

    fn list_tables(&self, _txn: TxnId, _schema_id: SchemaId) -> DbResult<Vec<TableDescriptor>> {
        Ok(Vec::new())
    }

    fn list_indexes(&self, _txn: TxnId, _table_id: RelationId) -> DbResult<Vec<IndexDescriptor>> {
        Ok(Vec::new())
    }

    fn get_index(&self, _txn: TxnId, _index_id: IndexId) -> DbResult<Option<IndexDescriptor>> {
        Ok(None)
    }

    fn get_sequence(
        &self,
        _txn: TxnId,
        _name: &QualifiedName,
    ) -> DbResult<Option<SequenceDescriptor>> {
        Ok(None)
    }

    fn get_statistics(
        &self,
        _txn: TxnId,
        _table_id: RelationId,
    ) -> DbResult<Option<TableStatistics>> {
        Ok(None)
    }

    fn get_view(&self, _txn: TxnId, _name: &QualifiedName) -> DbResult<Option<ViewDescriptor>> {
        Ok(None)
    }

    fn list_views(&self, _txn: TxnId, _schema_id: SchemaId) -> DbResult<Vec<ViewDescriptor>> {
        Ok(Vec::new())
    }
}

#[cfg(test)]
mod cypher_stats_tests {
    use super::*;
    use std::collections::HashMap;

    #[derive(Debug)]
    struct CypherStatsCatalog {
        rows: HashMap<u64, u64>,
    }

    impl CypherStatsCatalog {
        fn new(rows: &[(u64, u64)]) -> Self {
            Self {
                rows: rows.iter().copied().collect(),
            }
        }
    }

    impl CatalogReader for CypherStatsCatalog {
        fn get_schema(
            &self,
            _txn: TxnId,
            _name: &QualifiedName,
        ) -> DbResult<Option<SchemaDescriptor>> {
            Ok(None)
        }

        fn get_table(
            &self,
            _txn: TxnId,
            _name: &QualifiedName,
        ) -> DbResult<Option<TableDescriptor>> {
            Ok(None)
        }

        fn get_table_by_id(
            &self,
            _txn: TxnId,
            _table_id: RelationId,
        ) -> DbResult<Option<TableDescriptor>> {
            Ok(None)
        }

        fn list_tables(&self, _txn: TxnId, _schema_id: SchemaId) -> DbResult<Vec<TableDescriptor>> {
            Ok(Vec::new())
        }

        fn list_indexes(
            &self,
            _txn: TxnId,
            _table_id: RelationId,
        ) -> DbResult<Vec<IndexDescriptor>> {
            Ok(Vec::new())
        }

        fn get_index(&self, _txn: TxnId, _index_id: IndexId) -> DbResult<Option<IndexDescriptor>> {
            Ok(None)
        }

        fn get_sequence(
            &self,
            _txn: TxnId,
            _name: &QualifiedName,
        ) -> DbResult<Option<SequenceDescriptor>> {
            Ok(None)
        }

        fn get_statistics(
            &self,
            _txn: TxnId,
            table_id: RelationId,
        ) -> DbResult<Option<TableStatistics>> {
            Ok(self
                .rows
                .get(&table_id.get())
                .map(|row_count| TableStatistics {
                    table_id,
                    row_count: *row_count,
                    total_bytes: row_count.saturating_mul(64),
                    dead_row_count: 0,
                    last_updated_by: None,
                    column_stats: Vec::new(),
                }))
        }

        fn get_view(&self, _txn: TxnId, _name: &QualifiedName) -> DbResult<Option<ViewDescriptor>> {
            Ok(None)
        }

        fn list_views(&self, _txn: TxnId, _schema_id: SchemaId) -> DbResult<Vec<ViewDescriptor>> {
            Ok(Vec::new())
        }
    }

    fn cypher_node(label: &str, table_id: u64) -> aiondb_plan::graph::CypherNodePattern {
        aiondb_plan::graph::CypherNodePattern {
            variable: Some(label.to_ascii_lowercase()),
            label: Some(label.to_owned()),
            table_id: Some(RelationId::new(table_id)),
            properties: Vec::new(),
            index_scan: None,
            range_pushdown: Vec::new(),
        }
    }

    fn empty_cypher_plan() -> aiondb_plan::graph::CypherQueryPlan {
        aiondb_plan::graph::CypherQueryPlan {
            pipeline: Vec::new(),
            matches: Vec::new(),
            creates: Vec::new(),
            merges: Vec::new(),
            sets: Vec::new(),
            deletes: Vec::new(),
            returns: Vec::new(),
            order_by: Vec::new(),
            skip: None,
            limit: None,
            distinct: false,
            union: None,
        }
    }

    #[test]
    fn optimizer_uses_catalog_row_counts_for_cypher_pattern_reorder() {
        let catalog = CypherStatsCatalog::new(&[(1, 10_000), (2, 5)]);
        let optimizer = Optimizer::new(Arc::new(catalog));

        let mut plan = empty_cypher_plan();
        plan.matches.push(aiondb_plan::graph::CypherMatchClause {
            optional: false,
            patterns: vec![
                aiondb_plan::graph::CypherPattern {
                    path_function: None,
                    path_variable: None,
                    nodes: vec![cypher_node("Huge", 1)],
                    relationships: Vec::new(),
                },
                aiondb_plan::graph::CypherPattern {
                    path_function: None,
                    path_variable: None,
                    nodes: vec![cypher_node("Tiny", 2)],
                    relationships: Vec::new(),
                },
            ],
            filter: None,
        });

        let physical = optimizer
            .optimize(OptimizeRequest {
                logical_plan: LogicalPlan::CypherQuery(plan),
                txn_id: TxnId::default(),
            })
            .expect("optimize Cypher query");

        let PhysicalPlan::CypherQuery(plan) = physical else {
            panic!("expected CypherQuery physical plan");
        };
        assert_eq!(
            plan.matches[0].patterns[0].nodes[0].label.as_deref(),
            Some("Tiny")
        );
    }
}

#[cfg(test)]
mod optimizer_flag_tests {
    use super::parameterized_index_join_enabled;
    use std::sync::{Mutex, OnceLock};

    fn with_env_var(key: &'static str, value: Option<&str>, test_fn: impl FnOnce()) {
        static ENV_LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        let _guard = ENV_LOCK
            .get_or_init(|| Mutex::new(()))
            .lock()
            .expect("environment lock must not be poisoned");

        let previous = std::env::var_os(key);
        match value {
            Some(value) => std::env::set_var(key, value),
            None => std::env::remove_var(key),
        }
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(test_fn));
        match previous {
            Some(previous_value) => std::env::set_var(key, previous_value),
            None => std::env::remove_var(key),
        }
        if let Err(panic_payload) = result {
            std::panic::resume_unwind(panic_payload);
        }
    }

    #[test]
    fn parameterized_index_join_is_enabled_by_default() {
        with_env_var("AIONDB_DISABLE_PARAMETERIZED_INDEX_JOIN", None, || {
            assert!(parameterized_index_join_enabled());
        });
    }

    #[test]
    fn parameterized_index_join_disable_flag_turns_it_off() {
        with_env_var("AIONDB_DISABLE_PARAMETERIZED_INDEX_JOIN", Some("1"), || {
            assert!(!parameterized_index_join_enabled());
        });
    }

    #[test]
    fn parameterized_index_join_false_disable_value_leaves_it_on() {
        with_env_var(
            "AIONDB_DISABLE_PARAMETERIZED_INDEX_JOIN",
            Some("false"),
            || {
                assert!(parameterized_index_join_enabled());
            },
        );
    }
}

#[cfg(test)]
mod physical_builder_tests;
#[cfg(test)]
mod tests;
