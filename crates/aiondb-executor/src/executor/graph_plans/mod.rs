//! Executor for Cypher graph queries.
//!
//! Handles MATCH pattern matching, CREATE/MERGE/DELETE graph mutations,
//! SET property updates, and RETURN clause projection.
//! The plan types come from `aiondb_plan::graph` (`CypherQueryPlan`, etc.).
//! Node and edge data is stored in regular SQL tables; the executor scans,
//! inserts, updates and deletes through the same storage API used by DML.

mod graph_match;
mod graph_mutate;
mod graph_procedure;
mod graph_procedure_render;
mod graph_procedure_results;

mod graph_explain;
mod graph_fast_anchored;
mod graph_fast_paths;
mod graph_fast_unanchored;
mod graph_format;
mod graph_pipeline;
mod graph_predicates;

pub(in crate::executor) use graph_mutate::compare_cypher_sort_keys;
use graph_mutate::dedup_rows_by_values;
pub(super) use graph_format::format_cypher_property_value;
use graph_format::{
    format_cypher_bound_edge_literal, format_cypher_path_literal,
    format_cypher_path_value_literal, format_cypher_property_bag, is_cypher_system_column,
};
// Re-exported for `crate::executor::graph_plans::*` path consumers. The only
// callers today live in `#[cfg(test)]` graph tests, so the re-export is unused
// in non-test builds — allow that rather than narrow the crate-internal surface.
#[cfg_attr(not(test), allow(unused_imports))]
pub(in crate::executor) use graph_explain::{
    explain_graph_drift_suggestion_line, explain_graph_pattern_hint_line,
    explain_graph_plan_hint_line, graph_estimate_warning_severity,
};
// Predicate/filter/expr-analysis surface, consumed across the graph_plans
// sibling modules (fast paths, pipeline, …) via their `use super::*`.
pub(in crate::executor) use graph_predicates::*;

use std::cmp::Ordering;
use std::collections::{HashMap, HashSet};
use std::mem::size_of;
use std::sync::Arc;

use aiondb_catalog::{
    ColumnDescriptor, EdgeLabelDescriptor, NodeLabelDescriptor, QualifiedName, SequenceDescriptor,
    TableDescriptor,
};
use aiondb_core::{
    ColumnId, DataType, DbError, DbResult, IndexId, RelationId, Row, SchemaId, SequenceId,
    SqlState, TupleId, TxnId, Value,
};
use aiondb_eval::{build_hash_key, compare_runtime_values, ValueHashKey};
use aiondb_graph::{
    algorithms::procedures::{procedure_info, AlgorithmConfigField},
    HybridGraphPlan, HybridGraphSource,
};
use aiondb_graph_projection::NamedGraphProjectionDescriptor;
use aiondb_plan::graph::{
    CypherCreateClause, CypherDeleteClause, CypherForeachOp, CypherForeachPlan, CypherMatchClause,
    CypherMergeClause, CypherNodePattern, CypherPattern, CypherPipelineOp, CypherPropertyExpr,
    CypherQueryPlan, CypherRelDirection, CypherRelPattern, CypherSetItem,
};
use aiondb_plan::{ProjectionExpr, ScalarFunction, SortExpr, TypedExpr, TypedExprKind};

use tracing::debug;

use super::*;
pub(super) use aiondb_core::convert::usize_to_u32_saturating as usize_to_u32;

pub(super) fn value_to_bfs_key(v: &Value) -> Option<ValueHashKey> {
    graph_mutate::value_to_bfs_key(v)
}

#[inline]
pub(super) fn nonneg_i64_to_usize(value: i64) -> usize {
    if value <= 0 {
        0
    } else {
        usize::try_from(value).unwrap_or(usize::MAX)
    }
}

#[inline]
pub(super) fn len_plus_one_to_u32(len: usize) -> u32 {
    u32::try_from(len.saturating_add(1)).unwrap_or(u32::MAX)
}

#[inline]
pub(super) fn size_of_u64<T>() -> u64 {
    u64::try_from(size_of::<T>()).unwrap_or(u64::MAX)
}

// ---------------------------------------------------------------------------
// Binding model
// ---------------------------------------------------------------------------

pub(super) type SharedBoundValue = Arc<BoundValue>;
pub(super) type SharedRow = Arc<Row>;
pub(super) type SharedStrings = Arc<Vec<String>>;
pub(super) type SharedText = Arc<str>;

#[derive(Default)]
pub(in crate::executor) struct GraphMatchRuntimeCache {
    pub edge_target_cache:
        HashMap<(RelationId, ValueHashKey), Option<(SharedRow, SharedRow, Value, TupleId)>>,
    pub adjacency_neighbor_cache: HashMap<(RelationId, ValueHashKey, bool), Arc<Vec<Value>>>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) enum CypherGraphAccessClauseKind {
    Match,
    PipelineMatch,
}

pub(in crate::executor) fn graph_access_profile_key(
    clause_label: &str,
    clause_index: usize,
    pattern_index: usize,
) -> String {
    format!("{clause_label}:{clause_index}:{pattern_index}")
}

pub(in crate::executor) fn graph_access_clause_profile_input_key(
    clause_label: &str,
    clause_index: usize,
) -> String {
    format!("clause_in:{clause_label}:{clause_index}")
}

pub(in crate::executor) fn graph_access_clause_profile_output_key(
    clause_label: &str,
    clause_index: usize,
) -> String {
    format!("clause_out:{clause_label}:{clause_index}")
}

pub(in crate::executor) fn graph_access_clause_profile_time_key(
    clause_label: &str,
    clause_index: usize,
) -> String {
    format!("clause_time:{clause_label}:{clause_index}")
}

pub(in crate::executor) fn graph_access_clause_runtime_strategy_key(
    clause_label: &str,
    clause_index: usize,
) -> String {
    format!("clause_runtime_strategy:{clause_label}:{clause_index}")
}

pub(in crate::executor) fn graph_access_clause_runtime_reason_key(
    clause_label: &str,
    clause_index: usize,
) -> String {
    format!("clause_runtime_reason:{clause_label}:{clause_index}")
}

pub(in crate::executor) fn graph_access_clause_runtime_blocker_key(
    clause_label: &str,
    clause_index: usize,
) -> String {
    format!("clause_runtime_blocker:{clause_label}:{clause_index}")
}

pub(in crate::executor) fn graph_access_pattern_profile_time_key(
    clause_label: &str,
    clause_index: usize,
    pattern_index: usize,
) -> String {
    format!("pattern_time:{clause_label}:{clause_index}:{pattern_index}")
}

pub(in crate::executor) fn graph_access_pattern_runtime_strategy_key(
    clause_label: &str,
    clause_index: usize,
    pattern_index: usize,
) -> String {
    format!("pattern_runtime_strategy:{clause_label}:{clause_index}:{pattern_index}")
}

pub(in crate::executor) fn graph_access_pattern_runtime_reason_key(
    clause_label: &str,
    clause_index: usize,
    pattern_index: usize,
) -> String {
    format!("pattern_runtime_reason:{clause_label}:{clause_index}:{pattern_index}")
}

pub(in crate::executor) fn graph_access_pattern_pivot_driver_key(
    clause_label: &str,
    clause_index: usize,
    pattern_index: usize,
) -> String {
    format!("pattern_pivot_driver:{clause_label}:{clause_index}:{pattern_index}")
}

pub(in crate::executor) fn graph_access_pattern_pivot_reason_key(
    clause_label: &str,
    clause_index: usize,
    pattern_index: usize,
) -> String {
    format!("pattern_pivot_reason:{clause_label}:{clause_index}:{pattern_index}")
}

pub(in crate::executor) fn graph_access_pattern_pivot_decision_key(
    clause_label: &str,
    clause_index: usize,
    pattern_index: usize,
) -> String {
    format!("pattern_pivot_decision:{clause_label}:{clause_index}:{pattern_index}")
}

#[derive(Clone, Debug, PartialEq)]
pub(super) struct CypherGraphAccessPlanHint {
    pub clause_kind: CypherGraphAccessClauseKind,
    pub clause_index: usize,
    pub pattern_index: usize,
    pub plan: HybridGraphPlan,
}

#[derive(Clone, Debug, PartialEq)]
pub(super) struct CypherProcedureGraphAccessPlanHint {
    pub clause_index: usize,
    pub procedure: String,
    pub weighted: bool,
    pub projection: NamedGraphProjectionDescriptor,
    pub projection_ready: bool,
    pub plan: HybridGraphPlan,
}

#[derive(Clone, Debug)]
pub(in crate::executor) enum GraphBindingReduction {
    GlobalDistinctExpr(TypedExpr),
    TopN {
        order_by: Vec<SortExpr>,
        limit: usize,
    },
}

/// Format a graph node as the Cypher textual literal `(:Label[:Label2] {props})`.
/// `column_names` and `row` come from the node's backing table; columns whose
/// name starts with `__` are treated as system columns and skipped, plus any
/// column whose value is NULL is omitted from the property bag. The
/// synthetic `_default` label used to back anonymous `({prop: ...})` nodes
/// is hidden from the output so the literal round-trips through Cypher.
pub(super) fn format_cypher_node_literal(
    column_names: &[String],
    row: &Row,
    labels: &[String],
) -> String {
    let mut out = String::from("(");
    let visible: Vec<&str> = labels
        .iter()
        .map(String::as_str)
        .filter(|l| *l != "_default")
        .collect();
    if !visible.is_empty() {
        for label in &visible {
            out.push(':');
            out.push_str(label);
        }
    }
    let props = format_cypher_property_bag(column_names, row);
    if !props.is_empty() {
        if !visible.is_empty() {
            out.push(' ');
        }
        out.push_str(&props);
    }
    out.push(')');
    out
}

/// Format a graph edge as the Cypher textual literal `[:TYPE {props}]`.
pub(super) fn format_cypher_edge_literal(
    column_names: &[String],
    row: &Row,
    rel_type: &str,
) -> String {
    let props = format_cypher_property_bag(column_names, row);
    if props.is_empty() {
        format!("[:{rel_type}]")
    } else {
        format!("[:{rel_type} {props}]")
    }
}

pub(super) fn format_cypher_bound_node_literal(
    binding: &BindingRow,
    variable: &str,
) -> Option<String> {
    match binding.get(variable) {
        Some(BoundValue::Node {
            raw_row,
            column_names,
            labels,
            ..
        }) => Some(format_cypher_node_literal(column_names, raw_row, labels)),
        _ => None,
    }
}

pub(in crate::executor) fn graph_bound_node_literal(
    binding: &BindingRow,
    variable: &str,
) -> Option<String> {
    format_cypher_bound_node_literal(binding, variable)
}

pub(in crate::executor) fn graph_bound_edge_literal(
    binding: &BindingRow,
    variable: &str,
) -> Option<String> {
    format_cypher_bound_edge_literal(binding, variable)
}

pub(in crate::executor) fn compact_graph_binding_node_payloads(binding: &mut BindingRow) {
    for (_, value) in &mut binding.entries {
        let compacted = match value.as_ref() {
            BoundValue::Node {
                table_id,
                id_value,
                tuple_id,
                labels,
                column_names,
                raw_row,
                ..
            } => Some(Arc::new(BoundValue::Node {
                table_id: *table_id,
                row: if table_id.get() == 0 && id_value.is_null() {
                    Arc::clone(raw_row)
                } else {
                    Arc::new(Row::new(vec![id_value.clone()]))
                },
                raw_row: Arc::clone(raw_row),
                id_value: id_value.clone(),
                tuple_id: *tuple_id,
                labels: Arc::clone(labels),
                column_names: Arc::clone(column_names),
            })),
            _ => None,
        };
        if let Some(compacted) = compacted {
            *value = compacted;
        }
    }
}

pub(in crate::executor) fn retain_graph_binding_variables(
    binding: &mut BindingRow,
    keep: &std::collections::HashSet<String>,
) {
    if keep.is_empty() {
        binding.entries.clear();
        return;
    }
    binding.entries.retain(|(name, _)| keep.contains(name));
}

pub(in crate::executor) fn compact_node_bound_value(
    table_id: RelationId,
    id_value: Value,
    tuple_id: aiondb_core::TupleId,
    labels: SharedStrings,
    column_names: SharedStrings,
) -> BoundValue {
    // The `row` and `raw_row` fields hold the same single-column marker;
    // share one Arc<Row> instead of allocating two identical Rows and
    // cloning `id_value` an extra time.
    let marker_row = Arc::new(Row::new(vec![id_value.clone()]));
    BoundValue::Node {
        table_id,
        row: Arc::clone(&marker_row),
        raw_row: marker_row,
        id_value,
        tuple_id,
        labels,
        column_names,
    }
}

/// Per-row binding scope produced by Cypher MATCH / WITH / UNWIND.
///
/// Backed by a `Vec` rather than a `HashMap` because the typical
/// pattern bind set is tiny (2-6 entries: nodes, relationships,
/// the optional `__edge_next_node_id__` marker, scalars from
/// UNWIND/WITH). For those sizes a `Vec` clone is materially
/// cheaper than a `HashMap` clone — `BindingRow` is cloned once
/// per matched (a, b, …) row in `match_pattern_pivoted` and
/// adjacent matchers, so the per-binding overhead dominates the
/// 24 µs/traversal floor profiled on `group_neighbor_category`.
///
/// Public surface (`get` / `insert_binding` / `with_binding` / …)
/// is preserved so the refactor stays local to this struct;
/// direct callers iterate via the new `iter()` / `values()` /
/// `remove()` helpers below.
#[derive(Clone, Debug, Default)]
pub(super) struct BindingRow {
    /// Entries stored in insertion order with later inserts
    /// shadowing earlier ones via `last-wins` semantics in
    /// `insert_binding`. `Vec<(String, _)>` was chosen over
    /// `HashMap` for cheap cloning at the small sizes Cypher
    /// patterns produce.
    pub(super) entries: Vec<(String, SharedBoundValue)>,
}

impl BindingRow {
    pub(super) fn new() -> Self {
        Self {
            entries: Vec::new(),
        }
    }

    pub(super) fn with_binding(mut self, name: impl Into<String>, value: BoundValue) -> Self {
        self.insert_binding(name, value);
        self
    }

    pub(super) fn insert_binding(&mut self, name: impl Into<String>, value: BoundValue) {
        self.insert_shared_binding(name, Arc::new(value));
    }

    pub(super) fn insert_shared_binding(
        &mut self,
        name: impl Into<String>,
        value: SharedBoundValue,
    ) {
        let name = name.into();
        if let Some(slot) = self.entries.iter_mut().find(|(k, _)| *k == name) {
            slot.1 = value;
        } else {
            self.entries.push((name, value));
        }
    }

    pub(super) fn push_fresh_shared_binding(
        &mut self,
        name: impl Into<String>,
        value: SharedBoundValue,
    ) {
        self.entries.push((name.into(), value));
    }

    pub(super) fn get(&self, name: &str) -> Option<&BoundValue> {
        self.entries
            .iter()
            .rev()
            .find(|(k, _)| k == name)
            .map(|(_, v)| Arc::as_ref(v))
    }

    pub(super) fn get_shared(&self, name: &str) -> Option<SharedBoundValue> {
        self.entries
            .iter()
            .rev()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.clone())
    }

    /// Remove a binding by name. Returns `true` when an entry
    /// was removed, `false` when the name was not bound.
    pub(super) fn remove(&mut self, name: &str) -> bool {
        if self
            .entries
            .last()
            .is_some_and(|(last_name, _)| last_name == name)
        {
            self.entries.pop();
            return true;
        }
        if let Some(idx) = self.entries.iter().position(|(k, _)| k == name) {
            self.entries.remove(idx);
            true
        } else {
            false
        }
    }

    /// Iterate over `(name, value)` pairs in insertion order.
    pub(super) fn iter(&self) -> std::slice::Iter<'_, (String, SharedBoundValue)> {
        self.entries.iter()
    }

    /// Iterate over the bound values only.
    pub(super) fn values(&self) -> impl Iterator<Item = &SharedBoundValue> {
        self.entries.iter().map(|(_, v)| v)
    }
}

/// A bound value representing a matched graph element.
#[derive(Clone, Debug)]
pub(super) enum BoundValue {
    Node {
        table_id: RelationId,
        /// The compat row (with system columns appended) used for expression evaluation.
        row: SharedRow,
        /// The raw storage row (without system columns) used for updates/deletes.
        raw_row: SharedRow,
        /// The node identity value (first column by convention).
        id_value: Value,
        /// The tuple id for updates/deletes.
        tuple_id: aiondb_core::TupleId,
        /// Node label names for graph introspection (e.g. `["Person"]`).
        labels: SharedStrings,
        /// Column names from the backing table (for building JSONB metadata).
        column_names: SharedStrings,
    },
    Edge {
        table_id: RelationId,
        /// The compat row (with system columns appended) used for expression evaluation.
        row: SharedRow,
        /// The raw storage row (without system columns) used for updates/deletes.
        raw_row: SharedRow,
        /// The tuple id for updates/deletes.
        tuple_id: aiondb_core::TupleId,
        /// The relationship type name for graph introspection (e.g. `"KNOWS"`).
        rel_type: SharedText,
        /// Column names from the backing table (for building JSONB metadata).
        column_names: SharedStrings,
    },
    /// Named path binding from `MATCH p = (...)`.
    #[allow(dead_code)]
    Path {
        /// Node binding variables in query order. Anonymous nodes are
        /// materialised with internal variable names when the path is named.
        nodes: SharedStrings,
        /// Relationship binding variables in query order.
        relationships: SharedStrings,
        /// Relationship directions in query order for rendering `RETURN p`.
        directions: Arc<Vec<CypherRelDirection>>,
    },
    /// Named variable-length path with materialised element literals.
    #[allow(dead_code)]
    PathValues {
        nodes: SharedStrings,
        relationships: SharedStrings,
        directions: Arc<Vec<CypherRelDirection>>,
    },
    /// Null binding from an OPTIONAL MATCH that did not find a match.
    Null {
        /// Number of columns to emit as NULL in the flat row to keep ordinals aligned.
        column_count: usize,
    },
    /// A scalar value from UNWIND or WITH projection.
    Scalar(Value),
}

/// Strategy selector for path search.
///
/// Keeping this as an explicit enum makes it easy to route future algorithms
pub(super) fn ensure_graph_result_row_capacity(
    context: &ExecutionContext,
    current_rows: usize,
) -> DbResult<()> {
    if usize_to_u64(current_rows) >= context.max_result_rows {
        Err(DbError::program_limit(
            "maximum number of result rows reached",
        ))
    } else {
        Ok(())
    }
}

const GRAPH_WORKSET_ENTRIES_PER_RESULT_ROW: u64 = 32;
const GRAPH_MIN_WORKSET_ENTRIES: u64 = 256;
const GRAPH_PREALLOC_CAP: usize = 1024;

pub(super) fn graph_prealloc_capacity(estimated: usize) -> usize {
    estimated.min(GRAPH_PREALLOC_CAP)
}

pub(super) fn graph_workset_entry_cap(context: &ExecutionContext) -> u64 {
    context
        .max_result_rows
        .saturating_mul(GRAPH_WORKSET_ENTRIES_PER_RESULT_ROW)
        .max(GRAPH_MIN_WORKSET_ENTRIES)
}

pub(super) fn ensure_graph_workset_capacity(
    context: &ExecutionContext,
    current_entries: usize,
    component: &str,
) -> DbResult<()> {
    if usize_to_u64(current_entries) >= graph_workset_entry_cap(context) {
        Err(DbError::program_limit(format!(
            "maximum graph traversal workset reached while expanding {component}"
        )))
    } else {
        Ok(())
    }
}

pub(super) fn push_graph_binding(
    context: &ExecutionContext,
    output: &mut Vec<BindingRow>,
    binding: BindingRow,
) -> DbResult<()> {
    ensure_graph_result_row_capacity(context, output.len())?;
    context.track_memory(estimate_binding_row_bytes(&binding))?;
    output.push(binding);
    Ok(())
}

pub(super) fn estimate_bound_value_bytes(value: &BoundValue) -> u64 {
    match value {
        BoundValue::Node { id_value, .. } => 64u64.saturating_add(estimate_value_bytes(id_value)),
        BoundValue::Edge { .. } => 64,
        BoundValue::Path {
            nodes,
            relationships,
            directions,
        }
        | BoundValue::PathValues {
            nodes,
            relationships,
            directions,
        } => {
            let node_bytes = nodes
                .iter()
                .map(|name| usize_to_u64(name.len()))
                .sum::<u64>();
            let rel_bytes = relationships
                .iter()
                .map(|name| usize_to_u64(name.len()))
                .sum::<u64>();
            48u64
                .saturating_add(node_bytes)
                .saturating_add(rel_bytes)
                .saturating_add(
                    usize_to_u64(directions.len())
                        .saturating_mul(size_of_u64::<CypherRelDirection>()),
                )
        }
        BoundValue::Null { column_count } => {
            16u64.saturating_add(usize_to_u64(*column_count).saturating_mul(size_of_u64::<Value>()))
        }
        BoundValue::Scalar(v) => 24u64.saturating_add(estimate_value_bytes(v)),
    }
}

pub(super) fn estimate_binding_row_bytes(binding: &BindingRow) -> u64 {
    let entries = binding.iter().map(|(name, value)| {
        usize_to_u64(name.len())
            .saturating_add(size_of_u64::<String>())
            .saturating_add(size_of_u64::<Arc<BoundValue>>())
            .saturating_add(estimate_bound_value_bytes(value.as_ref()))
    });
    entries.fold(64, u64::saturating_add)
}

pub(super) fn estimate_bfs_path_bytes(path_len: usize) -> u64 {
    24u64.saturating_add(usize_to_u64(path_len).saturating_mul(size_of_u64::<usize>()))
}

pub(super) fn estimate_bfs_path_set_bytes(path_set_len: usize) -> u64 {
    // Approximate HashSet node overhead (bucket + key + allocator metadata).
    usize_to_u64(path_set_len).saturating_mul(size_of_u64::<usize>().saturating_mul(3))
}

pub(super) fn estimate_shortest_path_queue_entry_bytes(
    node_id: &Value,
    path_len: usize,
    path_set_len: usize,
) -> u64 {
    64u64
        .saturating_add(estimate_value_bytes(node_id))
        .saturating_add(estimate_bfs_path_bytes(path_len))
        .saturating_add(estimate_bfs_path_set_bytes(path_set_len))
}

pub(super) fn estimate_variable_frontier_entry_bytes(
    node_id: &Value,
    binding: &BindingRow,
    traversed_edges: usize,
) -> u64 {
    64u64
        .saturating_add(estimate_value_bytes(node_id))
        .saturating_add(estimate_binding_row_bytes(binding))
        .saturating_add(
            usize_to_u64(traversed_edges)
                .saturating_mul(size_of_u64::<aiondb_core::TupleId>().saturating_mul(3)),
        )
}
