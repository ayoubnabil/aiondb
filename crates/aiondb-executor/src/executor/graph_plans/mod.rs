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

use graph_mutate::dedup_rows_by_values;

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
    SqlState, TxnId, Value,
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

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) enum CypherGraphAccessClauseKind {
    Match,
    PipelineMatch,
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
            row,
            column_names,
            labels,
            ..
        }) => Some(format_cypher_node_literal(column_names, row, labels)),
        _ => None,
    }
}

fn format_cypher_bound_edge_literal(binding: &BindingRow, variable: &str) -> Option<String> {
    match binding.get(variable) {
        Some(BoundValue::Edge {
            row,
            column_names,
            rel_type,
            ..
        }) => Some(format_cypher_edge_literal(column_names, row, rel_type)),
        _ => None,
    }
}

fn format_cypher_path_literal(
    binding: &BindingRow,
    node_vars: &[String],
    relationship_vars: &[String],
    directions: &[CypherRelDirection],
) -> String {
    let nodes = node_vars
        .iter()
        .map(|var| {
            format_cypher_bound_node_literal(binding, var).unwrap_or_else(|| "()".to_owned())
        })
        .collect::<Vec<_>>();
    let relationships = relationship_vars
        .iter()
        .map(|var| {
            format_cypher_bound_edge_literal(binding, var).unwrap_or_else(|| "[]".to_owned())
        })
        .collect::<Vec<_>>();
    format_cypher_path_value_literal(&nodes, &relationships, directions)
}

fn format_cypher_path_value_literal(
    node_literals: &[String],
    relationship_literals: &[String],
    directions: &[CypherRelDirection],
) -> String {
    if node_literals.is_empty() {
        return String::from("()");
    }

    let mut out = String::new();
    for (idx, node_literal) in node_literals.iter().enumerate() {
        if idx > 0 {
            let rel_idx = idx - 1;
            let rel_literal = relationship_literals
                .get(rel_idx)
                .map(String::as_str)
                .unwrap_or("[]");
            match directions
                .get(rel_idx)
                .copied()
                .unwrap_or(CypherRelDirection::Outgoing)
            {
                CypherRelDirection::Outgoing => {
                    out.push('-');
                    out.push_str(rel_literal);
                    out.push_str("->");
                }
                CypherRelDirection::Incoming => {
                    out.push_str("<-");
                    out.push_str(rel_literal);
                    out.push('-');
                }
                CypherRelDirection::Both => {
                    out.push('-');
                    out.push_str(rel_literal);
                    out.push('-');
                }
            }
        }
        out.push_str(node_literal);
    }
    out
}

/// Render the user-visible properties of a graph element as Cypher's
/// `{key: value, ...}` map literal. System columns (prefixed with `__` or
/// the conventional `id`/`tid`/`source`/`target` identity columns) and
/// NULL-valued columns are skipped.
fn format_cypher_property_bag(column_names: &[String], row: &Row) -> String {
    use std::fmt::Write as _;

    // Walk once to count visible properties; if there are none we don't need
    // to emit braces (matches the empty-entries -> empty string behaviour)
    // and we avoid even allocating the output buffer.
    let mut visible = 0usize;
    for (idx, name) in column_names.iter().enumerate() {
        if is_cypher_system_column(name) {
            continue;
        }
        if row.values.get(idx).is_some_and(|value| !value.is_null()) {
            visible += 1;
        }
    }
    if visible == 0 {
        return String::new();
    }

    // Reserve a rough capacity (`{name: value, ` ~ 20 chars per entry) so
    // the buffer typically doesn't need to grow during writes. Replaces
    // the previous Vec<String>-of-entries-then-join pattern, which paid
    // one alloc per visible column plus one for the joined output.
    let mut out = String::with_capacity(2 + visible * 20);
    out.push('{');
    let mut first = true;
    for (idx, name) in column_names.iter().enumerate() {
        if is_cypher_system_column(name) {
            continue;
        }
        let Some(value) = row.values.get(idx) else {
            continue;
        };
        if value.is_null() {
            continue;
        }
        if !first {
            out.push_str(", ");
        }
        first = false;
        let _ = write!(out, "{name}: {}", format_cypher_property_value(value));
    }
    out.push('}');
    out
}

fn is_cypher_system_column(name: &str) -> bool {
    if name.starts_with("__") {
        return true;
    }
    matches!(
        name,
        "id" | "tid"
            | "source"
            | "target"
            | "source_id"
            | "target_id"
            | "__id"
            | "__tid"
            | "__source"
            | "__target"
    )
}

pub(super) fn format_cypher_property_value(value: &Value) -> String {
    match value {
        Value::Null => "null".to_owned(),
        Value::Boolean(b) => b.to_string(),
        Value::Int(n) => n.to_string(),
        Value::BigInt(n) => n.to_string(),
        Value::Real(f) => f.to_string(),
        Value::Double(f) => f.to_string(),
        Value::Numeric(v) => v.to_string(),
        Value::Text(s) => format!("'{}'", s.replace('\'', "\\'")),
        // Cypher renders temporal values in single quotes inside node
        // property bags (e.g. `{date: '1910-05-06'}`). Cypher format differs
        // from PG: timestamps use `T` separator, time/datetime drop seconds
        // when zero with no sub-second, and tz offsets always include minutes.
        Value::Date(_) | Value::Interval(_) => format!("'{value}'"),
        Value::Time(t) => format!("'{}'", format_cypher_time(t)),
        Value::TimeTz(t, off) => {
            format!("'{}{}'", format_cypher_time(t), format_cypher_offset(off))
        }
        Value::Timestamp(ts) => format!("'{}'", format_cypher_primitive_datetime(ts)),
        Value::TimestampTz(odt) => {
            let pdt = time::PrimitiveDateTime::new(odt.date(), odt.time());
            format!(
                "'{}{}'",
                format_cypher_primitive_datetime(&pdt),
                format_cypher_offset(&odt.offset())
            )
        }
        Value::Array(elems) => {
            // Cypher renders arrays as `[1, 2, 3]`, not PG's `{1,2,3}`.
            // Build the output in one buffer with a capacity hint
            // instead of paying N+2 heap allocations
            // (Vec<String> + per-element String + join String).
            let mut out = String::with_capacity(2 + elems.len() * 8);
            out.push('[');
            for (i, elem) in elems.iter().enumerate() {
                if i > 0 {
                    out.push_str(", ");
                }
                out.push_str(&format_cypher_property_value(elem));
            }
            out.push(']');
            out
        }
        Value::Jsonb(json) => format_cypher_jsonb_value(json),
        other => format!("{other}"),
    }
}

fn format_cypher_time(t: &time::Time) -> String {
    use std::fmt::Write as _;
    let nano = t.nanosecond();
    let sec = t.second();
    // Reserve enough room for `HH:MM:SS.fffffffff` so the buffer never grows.
    let mut out = String::with_capacity(18);
    let _ = write!(out, "{:02}:{:02}", t.hour(), t.minute());
    if sec != 0 || nano != 0 {
        let _ = write!(out, ":{sec:02}");
    }
    push_trimmed_nanos(&mut out, nano);
    out
}

fn format_cypher_offset(offset: &time::UtcOffset) -> String {
    let (oh, om, _) = offset.as_hms();
    let abs_om = om.unsigned_abs();
    format!("{oh:+03}:{abs_om:02}")
}

fn format_cypher_primitive_datetime(dt: &time::PrimitiveDateTime) -> String {
    use std::fmt::Write as _;
    let nano = dt.nanosecond();
    let sec = dt.second();
    let mut out = String::with_capacity(32);
    let _ = write!(
        out,
        "{:04}-{:02}-{:02}T{:02}:{:02}",
        dt.year(),
        u8::from(dt.month()),
        dt.day(),
        dt.hour(),
        dt.minute()
    );
    if sec != 0 || nano != 0 {
        let _ = write!(out, ":{sec:02}");
    }
    push_trimmed_nanos(&mut out, nano);
    out
}

/// Append `.<digits>` to `out` for a nanosecond fraction, trimming trailing
/// zeros. Skips emission entirely when `nano == 0` or when the trimmed digit
/// string is empty. Replaces a `format!`-then-`push_str` pattern with a
/// stack-buffer write - no intermediate `String` allocation.
fn push_trimmed_nanos(out: &mut String, nano: u32) {
    if nano == 0 {
        return;
    }
    // 9 ASCII digits cover 0..=999_999_999.
    let mut buf = [0u8; 9];
    let mut n = nano;
    for slot in buf.iter_mut().rev() {
        *slot = b'0' + (n % 10) as u8;
        n /= 10;
    }
    let mut end = buf.len();
    while end > 0 && buf[end - 1] == b'0' {
        end -= 1;
    }
    if end == 0 {
        return;
    }
    out.push('.');
    out.push_str(std::str::from_utf8(&buf[..end]).unwrap_or(""));
}

fn format_cypher_jsonb_value(v: &serde_json::Value) -> String {
    match v {
        serde_json::Value::Null => "null".to_owned(),
        serde_json::Value::Bool(b) => b.to_string(),
        serde_json::Value::Number(n) => n.to_string(),
        serde_json::Value::String(s) => format!("'{}'", s.replace('\'', "\\'")),
        // Build the array / object layout into a single output String
        // instead of paying N+2 heap allocations per recursion level
        // (Vec<String> of formatted parts, the join() output, and the
        // outer `format!("[{}]", ...)`). The recursive call still
        // allocates one String per element, but the per-level
        // surface allocations collapse to one.
        serde_json::Value::Array(items) => {
            let mut out = String::with_capacity(2 + items.len() * 8);
            out.push('[');
            for (i, item) in items.iter().enumerate() {
                if i > 0 {
                    out.push_str(", ");
                }
                out.push_str(&format_cypher_jsonb_value(item));
            }
            out.push(']');
            out
        }
        serde_json::Value::Object(map) => {
            let mut out = String::with_capacity(2 + map.len() * 16);
            out.push('{');
            let mut first = true;
            for (k, v) in map {
                if !first {
                    out.push_str(", ");
                }
                first = false;
                out.push_str(k);
                out.push_str(": ");
                out.push_str(&format_cypher_jsonb_value(v));
            }
            out.push('}');
            out
        }
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

impl Executor {
    fn cypher_procedure_uses_weighted_projection(
        call: &aiondb_plan::graph::CypherProcedureCall,
    ) -> bool {
        procedure_info(&call.procedure).is_some_and(|info| {
            call.args
                .iter()
                .zip(info.args.iter())
                .any(|(_, arg_info)| arg_info.config_field == AlgorithmConfigField::WeightColumn)
        })
    }

    pub(super) fn describe_cypher_procedure_graph_plan(
        &self,
        txn_id: TxnId,
        call: &aiondb_plan::graph::CypherProcedureCall,
        clause_index: usize,
    ) -> CypherProcedureGraphAccessPlanHint {
        let weighted = Self::cypher_procedure_uses_weighted_projection(call);
        let projection_kind = if weighted { "weighted CSR" } else { "CSR" };
        let discovered = self.describe_current_cypher_projection_or_placeholder(txn_id, weighted);
        let projection = discovered.descriptor;
        let projection_ready = discovered.ready;
        CypherProcedureGraphAccessPlanHint {
            clause_index,
            procedure: call.procedure.clone(),
            weighted,
            projection: projection.clone(),
            projection_ready,
            plan: HybridGraphPlan {
                source: Some(HybridGraphSource::ProjectionStore),
                fallback_source: Some(HybridGraphSource::RowStore),
                estimated_rows: projection_ready.then_some(projection.stats.edge_count),
                projection_name: Some(projection.name),
                reason: Some(format!(
                    "native Cypher graph procedure executes against an executor-managed {projection_kind} projection snapshot"
                )),
            },
        }
    }

    pub(super) fn describe_cypher_query_graph_procedure_plans(
        &self,
        txn_id: TxnId,
        query: &CypherQueryPlan,
    ) -> Vec<CypherProcedureGraphAccessPlanHint> {
        query
            .pipeline
            .iter()
            .enumerate()
            .filter_map(|(clause_index, op)| match op {
                CypherPipelineOp::ProcedureCall(call) => {
                    Some(self.describe_cypher_procedure_graph_plan(txn_id, call, clause_index))
                }
                _ => None,
            })
            .collect()
    }

    fn describe_cypher_pattern_graph_plan_for_txn(
        &self,
        txn_id: TxnId,
        pattern: &CypherPattern,
    ) -> HybridGraphPlan {
        if pattern.relationships.is_empty() {
            return HybridGraphPlan {
                source: Some(HybridGraphSource::RowStore),
                fallback_source: None,
                estimated_rows: None,
                projection_name: None,
                reason: Some("node-only Cypher pattern uses row-store scans".to_owned()),
            };
        }

        let mut available_edges = 0usize;
        let mut missing_edges = 0usize;
        let mut estimated_rows = 0u64;

        for rel in &pattern.relationships {
            let Some(table_id) = rel.table_id else {
                missing_edges = missing_edges.saturating_add(1);
                continue;
            };
            if self.storage_dml.adjacency_index_available(txn_id, table_id) {
                available_edges = available_edges.saturating_add(1);
                if let Some(stats) = self.storage_dml.adjacency_index_stats(txn_id, table_id) {
                    estimated_rows = estimated_rows.saturating_add(stats.edge_count);
                }
            } else {
                missing_edges = missing_edges.saturating_add(1);
            }
        }

        let estimated_rows = (estimated_rows > 0).then_some(estimated_rows);
        if available_edges == 0 {
            return HybridGraphPlan {
                source: Some(HybridGraphSource::RowStore),
                fallback_source: None,
                estimated_rows,
                projection_name: None,
                reason: Some(
                    "relationship pattern has no native traversal store; row-store fallback only"
                        .to_owned(),
                ),
            };
        }
        if missing_edges == 0 {
            return HybridGraphPlan {
                source: Some(HybridGraphSource::TraversalStore),
                fallback_source: Some(HybridGraphSource::RowStore),
                estimated_rows,
                projection_name: None,
                reason: Some(
                    "all relationship tables expose native adjacency traversal".to_owned(),
                ),
            };
        }
        HybridGraphPlan {
            source: Some(HybridGraphSource::Hybrid),
            fallback_source: Some(HybridGraphSource::RowStore),
            estimated_rows,
            projection_name: None,
            reason: Some(
                "some relationship tables expose native adjacency traversal and others fall back to row-store scans"
                    .to_owned(),
            ),
        }
    }

    pub(super) fn describe_cypher_match_graph_plans(
        &self,
        context: &ExecutionContext,
        clause: &CypherMatchClause,
        clause_kind: CypherGraphAccessClauseKind,
        clause_index: usize,
    ) -> Vec<CypherGraphAccessPlanHint> {
        clause
            .patterns
            .iter()
            .enumerate()
            .map(|(pattern_index, pattern)| CypherGraphAccessPlanHint {
                clause_kind: clause_kind.clone(),
                clause_index,
                pattern_index,
                plan: self.describe_cypher_pattern_graph_plan_for_txn(context.txn_id, pattern),
            })
            .collect()
    }

    pub(super) fn describe_cypher_query_graph_plans(
        &self,
        context: &ExecutionContext,
        query: &CypherQueryPlan,
    ) -> Vec<CypherGraphAccessPlanHint> {
        let mut hints = Vec::new();
        for (clause_index, op) in query.pipeline.iter().enumerate() {
            if let CypherPipelineOp::Match(clause) = op {
                hints.extend(self.describe_cypher_match_graph_plans(
                    context,
                    clause,
                    CypherGraphAccessClauseKind::PipelineMatch,
                    clause_index,
                ));
            }
        }
        for (clause_index, clause) in query.matches.iter().enumerate() {
            hints.extend(self.describe_cypher_match_graph_plans(
                context,
                clause,
                CypherGraphAccessClauseKind::Match,
                clause_index,
            ));
        }
        hints
    }

    pub(super) fn describe_cypher_pattern_graph_plan(
        &self,
        context: &ExecutionContext,
        pattern: &CypherPattern,
    ) -> HybridGraphPlan {
        self.describe_cypher_pattern_graph_plan_for_txn(context.txn_id, pattern)
    }

    pub fn explain_cypher_query_graph_access_lines(
        &self,
        txn_id: TxnId,
        query: &CypherQueryPlan,
    ) -> Vec<String> {
        let mut lines = Vec::new();
        for hint in self.describe_cypher_query_graph_procedure_plans(txn_id, query) {
            lines.push(format!(
                "Graph Projection [ProcedureCall {}]: procedure={}, source={:?}, fallback={:?}, projection={}, snapshot_generation={}, refresh_policy={:?}, refreshed_at_epoch_millis={}, weighted={}, estimated_rows={}, node_count={}, edge_count={}, reason={}",
                hint.clause_index,
                hint.procedure,
                hint.plan.source,
                hint.plan.fallback_source,
                hint.plan.projection_name.as_deref().unwrap_or("unknown"),
                hint.projection.snapshot.generation,
                hint.projection.snapshot.refresh_policy,
                hint.projection
                    .snapshot
                    .refreshed_at_epoch_millis
                    .map_or_else(|| "unknown".to_owned(), |ts| ts.to_string()),
                hint.weighted,
                hint.plan
                    .estimated_rows
                    .map_or_else(|| "unknown".to_owned(), |rows| rows.to_string()),
                (hint.projection_ready)
                    .then_some(hint.projection.stats)
                    .and_then(|stats| stats.node_count)
                    .map_or_else(|| "unknown".to_owned(), |count| count.to_string()),
                (hint.projection_ready)
                    .then_some(hint.projection.stats)
                    .map_or_else(|| "unknown".to_owned(), |stats| stats.edge_count.to_string()),
                hint.plan.reason.unwrap_or_default()
            ));
        }
        for (clause_index, op) in query.pipeline.iter().enumerate() {
            if let CypherPipelineOp::Match(clause) = op {
                for (pattern_index, pattern) in clause.patterns.iter().enumerate() {
                    let plan = self.describe_cypher_pattern_graph_plan_for_txn(txn_id, pattern);
                    lines.push(format!(
                        "Graph Access [{} {} pattern {}]: source={:?}, fallback={:?}, estimated_rows={}, reason={}",
                        "PipelineMatch",
                        clause_index,
                        pattern_index,
                        plan.source,
                        plan.fallback_source,
                        plan.estimated_rows
                            .map_or_else(|| "unknown".to_owned(), |rows| rows.to_string()),
                        plan.reason.unwrap_or_default()
                    ));
                }
            }
        }
        for (clause_index, clause) in query.matches.iter().enumerate() {
            for (pattern_index, pattern) in clause.patterns.iter().enumerate() {
                let plan = self.describe_cypher_pattern_graph_plan_for_txn(txn_id, pattern);
                lines.push(format!(
                    "Graph Access [{} {} pattern {}]: source={:?}, fallback={:?}, estimated_rows={}, reason={}",
                    "Match",
                    clause_index,
                    pattern_index,
                    plan.source,
                    plan.fallback_source,
                    plan.estimated_rows
                        .map_or_else(|| "unknown".to_owned(), |rows| rows.to_string()),
                    plan.reason.unwrap_or_default()
                ));
            }
        }
        lines
    }

    pub fn explain_physical_plan_graph_access_lines(
        &self,
        txn_id: TxnId,
        plan: &aiondb_plan::PhysicalPlan,
    ) -> Vec<String> {
        fn collect(
            executor: &Executor,
            txn_id: TxnId,
            plan: &aiondb_plan::PhysicalPlan,
            lines: &mut Vec<String>,
        ) {
            match plan {
                aiondb_plan::PhysicalPlan::CypherQuery(query) => {
                    lines.extend(
                        executor.explain_cypher_query_graph_access_lines(txn_id, query.as_ref()),
                    );
                }
                aiondb_plan::PhysicalPlan::ProjectSource { source, .. }
                | aiondb_plan::PhysicalPlan::AggregateSource { source, .. }
                | aiondb_plan::PhysicalPlan::PartialAggregate { source, .. }
                | aiondb_plan::PhysicalPlan::CreateTableAs { source, .. }
                | aiondb_plan::PhysicalPlan::InsertSelect { source, .. } => {
                    collect(executor, txn_id, source, lines);
                }
                aiondb_plan::PhysicalPlan::NestedLoopJoin { left, right, .. }
                | aiondb_plan::PhysicalPlan::HashJoin { left, right, .. }
                | aiondb_plan::PhysicalPlan::MergeJoin { left, right, .. }
                | aiondb_plan::PhysicalPlan::SetOperation { left, right, .. }
                | aiondb_plan::PhysicalPlan::BroadcastHashJoin {
                    broadcast: left,
                    local: right,
                    ..
                } => {
                    collect(executor, txn_id, left, lines);
                    collect(executor, txn_id, right, lines);
                }
                aiondb_plan::PhysicalPlan::NestedLoopIndexJoin { left, .. } => {
                    collect(executor, txn_id, left, lines);
                }
                aiondb_plan::PhysicalPlan::DistributedAppend { fragments, .. } => {
                    for fragment in fragments {
                        collect(executor, txn_id, fragment, lines);
                    }
                }
                aiondb_plan::PhysicalPlan::RecursiveCte {
                    base, recursive, ..
                } => {
                    collect(executor, txn_id, base, lines);
                    collect(executor, txn_id, recursive, lines);
                }
                aiondb_plan::PhysicalPlan::FinalAggregate { partials, .. } => {
                    for partial in partials {
                        collect(executor, txn_id, partial, lines);
                    }
                }
                _ => {}
            }
        }

        let mut lines = Vec::new();
        collect(self, txn_id, plan, &mut lines);
        lines
    }
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

fn literal_value(expr: &TypedExpr) -> Option<Value> {
    match &expr.kind {
        TypedExprKind::Literal(value) => Some(value.clone()),
        _ => None,
    }
}

fn extract_start_id_literal(
    start: &CypherNodePattern,
    filter: Option<&TypedExpr>,
    start_variable: &str,
) -> Option<Value> {
    let inline_id = match start.properties.as_slice() {
        [] => None,
        [property] if property.key.eq_ignore_ascii_case("id") => literal_value(&property.value),
        _ => return None,
    };
    let filter_id = match filter {
        Some(expr) => Some(extract_exact_id_equality(expr, start_variable)?),
        None => None,
    };
    match (inline_id, filter_id) {
        (Some(inline), Some(filter)) => {
            let mut left = inline;
            let mut right = filter;
            normalize_int_key(&mut left);
            normalize_int_key(&mut right);
            (left == right).then_some(left)
        }
        (Some(inline), None) => Some(inline),
        (None, Some(filter)) => Some(filter),
        (None, None) => None,
    }
}

fn extract_exact_id_equality(expr: &TypedExpr, variable: &str) -> Option<Value> {
    let TypedExprKind::BinaryEq { left, right } = &expr.kind else {
        return None;
    };
    match (&left.kind, &right.kind) {
        (TypedExprKind::ColumnRef { name, .. }, TypedExprKind::Literal(value))
            if is_graph_id_ref(name, variable) =>
        {
            Some(value.clone())
        }
        (TypedExprKind::Literal(value), TypedExprKind::ColumnRef { name, .. })
            if is_graph_id_ref(name, variable) =>
        {
            Some(value.clone())
        }
        _ => None,
    }
}

fn is_graph_id_ref(name: &str, variable: &str) -> bool {
    name.eq_ignore_ascii_case(&format!("{variable}.id"))
}

struct HybridGraphVectorFilter {
    start_tenant: Value,
    target_tenant: Value,
    /// L2 query vector stored as `Vec<f32>` so the per-target distance
    /// loop can run through the SIMD `l2_squared_f64` kernel (which takes
    /// two `&[f32]` slices and accumulates in `f64`). Vector embeddings are
    /// f32 in storage, so converting once at filter-extraction time avoids
    /// a scalar zip/map/sum per target row.
    query_vector: Vec<f32>,
    distance_threshold: f64,
}

struct HybridDeepGraphVectorFilter {
    start_id: Value,
    /// See [`HybridGraphVectorFilter::query_vector`].
    query_vector: Vec<f32>,
    distance_threshold: f64,
    popularity_threshold: Value,
}

/// Score a node pattern by inferred selectivity:
///   0 = literal-equality on indexed column (`index_scan` set)
///   1 = at least one literal property OR range pushdown
///        (storage can apply the predicate inline)
///   2 = label-only or no constraint (full SeqScan)
fn pivot_node_score(node: &CypherNodePattern) -> u8 {
    if node.index_scan.is_some() {
        return 0;
    }
    if !node.properties.is_empty() || !node.range_pushdown.is_empty() {
        return 1;
    }
    2
}

/// Pick the most-selective node in `pattern` to drive the match.
/// Returns `Some(idx)` only when the chosen pivot is BETTER than
/// the leftmost node — same-or-worse pivots leave the original
/// left-to-right walk in place to avoid pointless reordering.
/// Patterns of length 1 always return `None`.
pub(super) fn pick_match_pivot_index(pattern: &CypherPattern) -> Option<usize> {
    if pattern.nodes.len() <= 1 {
        return None;
    }
    // Pivoting requires single-hop relationships everywhere along
    // the chain — variable-length expansion needs the original
    // walk so it can chain through `match_variable_length_relationship`.
    if pattern
        .relationships
        .iter()
        .any(|r| r.min_hops.is_some() || r.max_hops.is_some())
    {
        return None;
    }
    let leftmost_node = pattern.nodes.first()?;
    let leftmost_score = pivot_node_score(leftmost_node);
    let (best_idx, best_score) = pattern
        .nodes
        .iter()
        .enumerate()
        .map(|(i, n)| (i, pivot_node_score(n)))
        .min_by_key(|(_, score)| *score)?;
    if best_idx == 0 || best_score >= leftmost_score {
        return None;
    }
    Some(best_idx)
}

/// Reverse a relationship's direction so the matcher walks the
/// adjacency list backwards. `Both` stays `Both` — undirected
/// relationships are symmetric so reversing is a no-op.
pub(super) fn flip_relationship_direction(rel: &CypherRelPattern) -> CypherRelPattern {
    let mut flipped = rel.clone();
    flipped.direction = match rel.direction {
        aiondb_plan::graph::CypherRelDirection::Outgoing => {
            aiondb_plan::graph::CypherRelDirection::Incoming
        }
        aiondb_plan::graph::CypherRelDirection::Incoming => {
            aiondb_plan::graph::CypherRelDirection::Outgoing
        }
        aiondb_plan::graph::CypherRelDirection::Both => {
            aiondb_plan::graph::CypherRelDirection::Both
        }
    };
    flipped
}

fn extract_hybrid_graph_vector_filter(
    filter: &TypedExpr,
    start_variable: &str,
    target_variable: &str,
) -> Option<HybridGraphVectorFilter> {
    let mut conjuncts = Vec::new();
    collect_graph_filter_conjuncts(filter, &mut conjuncts);

    let mut start_tenant = None;
    let mut target_tenant = None;
    let mut query_vector = None;
    let mut distance_threshold = None;

    for conjunct in conjuncts {
        if let Some((name, value)) = exact_column_literal_equality(conjunct) {
            if name.eq_ignore_ascii_case(&format!("{start_variable}.tenant_id")) {
                start_tenant = Some(value);
                continue;
            }
            if name.eq_ignore_ascii_case(&format!("{target_variable}.tenant_id")) {
                target_tenant = Some(value);
                continue;
            }
        }

        if let Some((vector, threshold)) = extract_l2_distance_threshold(conjunct, target_variable)
        {
            query_vector = Some(vector);
            distance_threshold = Some(threshold);
            continue;
        }

        return None;
    }

    Some(HybridGraphVectorFilter {
        start_tenant: start_tenant?,
        target_tenant: target_tenant?,
        // Storage vectors are f32. Converting once here removes the
        // per-target `f64::from(*left)` cast inside the SIMD-replaceable
        // hot loop and lets that loop reach the `l2_squared_f64` kernel.
        query_vector: query_vector?.into_iter().map(|v| v as f32).collect(),
        distance_threshold: distance_threshold?,
    })
}

pub(super) fn collect_graph_filter_conjuncts<'a>(
    expr: &'a TypedExpr,
    out: &mut Vec<&'a TypedExpr>,
) {
    let mut stack = vec![expr];
    while let Some(expr) = stack.pop() {
        if let TypedExprKind::LogicalAnd { left, right } = &expr.kind {
            stack.push(right);
            stack.push(left);
        } else {
            out.push(expr);
        }
    }
}

pub(super) struct GraphFilterConjunct<'a> {
    pub(super) expr: &'a TypedExpr,
    pub(super) referenced_vars: Option<HashSet<String>>,
}

impl<'a> GraphFilterConjunct<'a> {
    fn new(expr: &'a TypedExpr) -> Self {
        Self {
            expr,
            referenced_vars: referenced_graph_variables(expr),
        }
    }

    pub(super) fn is_ready(&self, binding: &BindingRow) -> bool {
        let Some(vars) = self.referenced_vars.as_ref() else {
            return false;
        };
        vars.iter()
            .all(|variable| binding.get(variable.as_str()).is_some())
    }
}

pub(super) fn build_graph_filter_conjuncts(filter: &TypedExpr) -> Vec<GraphFilterConjunct<'_>> {
    let mut conjuncts = Vec::new();
    collect_graph_filter_conjuncts(filter, &mut conjuncts);
    conjuncts
        .into_iter()
        .map(GraphFilterConjunct::new)
        .collect()
}

fn referenced_graph_variables(expr: &TypedExpr) -> Option<HashSet<String>> {
    let mut vars = HashSet::new();
    if collect_referenced_graph_variables(expr, &mut vars) {
        Some(vars)
    } else {
        None
    }
}

fn collect_referenced_graph_variables(expr: &TypedExpr, vars: &mut HashSet<String>) -> bool {
    match &expr.kind {
        TypedExprKind::Literal(_) | TypedExprKind::NextValue { .. } => true,
        TypedExprKind::ColumnRef { name, .. } | TypedExprKind::OuterColumnRef { name, .. } => {
            let variable = name.split_once('.').map_or(name.as_str(), |(head, _)| head);
            vars.insert(variable.to_owned());
            true
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
        | TypedExprKind::Nullif { left, right } => {
            collect_referenced_graph_variables(left, vars)
                && collect_referenced_graph_variables(right, vars)
        }
        TypedExprKind::LogicalNot { expr }
        | TypedExprKind::Negate { expr }
        | TypedExprKind::IsNull { expr, .. }
        | TypedExprKind::Cast { expr, .. } => collect_referenced_graph_variables(expr, vars),
        TypedExprKind::IsDistinctFrom { left, right, .. } => {
            collect_referenced_graph_variables(left, vars)
                && collect_referenced_graph_variables(right, vars)
        }
        TypedExprKind::Like { expr, pattern, .. } => {
            collect_referenced_graph_variables(expr, vars)
                && collect_referenced_graph_variables(pattern, vars)
        }
        TypedExprKind::InList { expr, list, .. } => {
            collect_referenced_graph_variables(expr, vars)
                && list
                    .iter()
                    .all(|item| collect_referenced_graph_variables(item, vars))
        }
        TypedExprKind::Between {
            expr, low, high, ..
        } => {
            collect_referenced_graph_variables(expr, vars)
                && collect_referenced_graph_variables(low, vars)
                && collect_referenced_graph_variables(high, vars)
        }
        TypedExprKind::CaseWhen {
            conditions,
            results,
            else_result,
        } => {
            conditions
                .iter()
                .chain(results.iter())
                .all(|item| collect_referenced_graph_variables(item, vars))
                && else_result
                    .as_deref()
                    .map_or(true, |item| collect_referenced_graph_variables(item, vars))
        }
        TypedExprKind::Coalesce { args }
        | TypedExprKind::ScalarFunction { args, .. }
        | TypedExprKind::ArrayConstruct { elements: args }
        | TypedExprKind::UserFunction { args, .. } => args
            .iter()
            .all(|arg| collect_referenced_graph_variables(arg, vars)),
        _ => false,
    }
}

pub(super) fn exact_column_literal_equality(expr: &TypedExpr) -> Option<(&str, Value)> {
    let TypedExprKind::BinaryEq { left, right } = &expr.kind else {
        return None;
    };
    match (&left.kind, &right.kind) {
        (TypedExprKind::ColumnRef { name, .. }, TypedExprKind::Literal(value)) => {
            Some((name.as_str(), value.clone()))
        }
        (TypedExprKind::Literal(value), TypedExprKind::ColumnRef { name, .. }) => {
            Some((name.as_str(), value.clone()))
        }
        _ => None,
    }
}

/// Detect a `column_ref CMP literal` predicate and return it as a
/// `(column_name, lower_bound, upper_bound)` triple. Only matches
/// when the operator is one of `<`, `<=`, `>`, `>=`, or
/// `BETWEEN lo AND hi` over a single `ColumnRef` and one or two
/// literals. Used by `apply_match_filter_index_hints` to drive the
/// per-node range pushdown that `scan_node_candidates` then sends
/// through `scan_table_multi_range_filter`.
pub(super) fn extract_column_literal_range(
    expr: &TypedExpr,
) -> Option<(&str, std::ops::Bound<Value>, std::ops::Bound<Value>)> {
    use std::ops::Bound;
    fn lit(expr: &TypedExpr) -> Option<&Value> {
        match &expr.kind {
            TypedExprKind::Literal(v) => Some(v),
            _ => None,
        }
    }
    fn col(expr: &TypedExpr) -> Option<&str> {
        match &expr.kind {
            TypedExprKind::ColumnRef { name, .. } => Some(name),
            _ => None,
        }
    }
    match &expr.kind {
        TypedExprKind::BinaryLt { left, right } => {
            if let (Some(c), Some(v)) = (col(left), lit(right)) {
                return Some((c, Bound::Unbounded, Bound::Excluded(v.clone())));
            }
            if let (Some(v), Some(c)) = (lit(left), col(right)) {
                return Some((c, Bound::Excluded(v.clone()), Bound::Unbounded));
            }
            None
        }
        TypedExprKind::BinaryLe { left, right } => {
            if let (Some(c), Some(v)) = (col(left), lit(right)) {
                return Some((c, Bound::Unbounded, Bound::Included(v.clone())));
            }
            if let (Some(v), Some(c)) = (lit(left), col(right)) {
                return Some((c, Bound::Included(v.clone()), Bound::Unbounded));
            }
            None
        }
        TypedExprKind::BinaryGt { left, right } => {
            if let (Some(c), Some(v)) = (col(left), lit(right)) {
                return Some((c, Bound::Excluded(v.clone()), Bound::Unbounded));
            }
            if let (Some(v), Some(c)) = (lit(left), col(right)) {
                return Some((c, Bound::Unbounded, Bound::Excluded(v.clone())));
            }
            None
        }
        TypedExprKind::BinaryGe { left, right } => {
            if let (Some(c), Some(v)) = (col(left), lit(right)) {
                return Some((c, Bound::Included(v.clone()), Bound::Unbounded));
            }
            if let (Some(v), Some(c)) = (lit(left), col(right)) {
                return Some((c, Bound::Unbounded, Bound::Included(v.clone())));
            }
            None
        }
        TypedExprKind::Between {
            expr, low, high, ..
        } => {
            let c = col(expr)?;
            let lo = lit(low)?;
            let hi = lit(high)?;
            Some((c, Bound::Included(lo.clone()), Bound::Included(hi.clone())))
        }
        _ => None,
    }
}

fn exact_named_column_literal_equality(expr: &TypedExpr, expected_name: &str) -> Option<Value> {
    let (name, value) = exact_column_literal_equality(expr)?;
    name.eq_ignore_ascii_case(expected_name).then_some(value)
}

fn exact_named_column_literal_gt(expr: &TypedExpr, expected_name: &str) -> Option<Value> {
    let TypedExprKind::BinaryGt { left, right } = &expr.kind else {
        return None;
    };
    let TypedExprKind::ColumnRef { name, .. } = &left.kind else {
        return None;
    };
    if !name.eq_ignore_ascii_case(expected_name) {
        return None;
    }
    let TypedExprKind::Literal(value) = &right.kind else {
        return None;
    };
    Some(value.clone())
}

fn extract_hybrid_deep_graph_vector_filter(
    filter: &TypedExpr,
    start_variable: &str,
    friend_variable: &str,
    target_variable: &str,
) -> Option<HybridDeepGraphVectorFilter> {
    let mut conjuncts = Vec::new();
    collect_graph_filter_conjuncts(filter, &mut conjuncts);

    let mut start_id = None;
    let mut query_vector = None;
    let mut distance_threshold = None;
    let mut popularity_threshold = None;

    for conjunct in conjuncts {
        if let Some(value) =
            exact_named_column_literal_equality(conjunct, &format!("{start_variable}.id"))
        {
            start_id = Some(value);
            continue;
        }

        if let Some(value) =
            exact_named_column_literal_gt(conjunct, &format!("{target_variable}.popularity"))
        {
            popularity_threshold = Some(value);
            continue;
        }

        if let Some((vector, threshold)) = extract_l2_distance_threshold(conjunct, target_variable)
        {
            query_vector = Some(vector);
            distance_threshold = Some(threshold);
            continue;
        }

        if is_column_column_equality(
            conjunct,
            &format!("{target_variable}.tenant_id"),
            &format!("{start_variable}.tenant_id"),
        ) || is_column_column_equality(
            conjunct,
            &format!("{friend_variable}.tenant_id"),
            &format!("{start_variable}.tenant_id"),
        ) {
            continue;
        }

        return None;
    }

    Some(HybridDeepGraphVectorFilter {
        start_id: start_id?,
        // See [`HybridGraphVectorFilter::query_vector`].
        query_vector: query_vector?.into_iter().map(|v| v as f32).collect(),
        distance_threshold: distance_threshold?,
        popularity_threshold: popularity_threshold?,
    })
}

fn is_column_column_equality(expr: &TypedExpr, left_name: &str, right_name: &str) -> bool {
    let TypedExprKind::BinaryEq { left, right } = &expr.kind else {
        return false;
    };
    let (TypedExprKind::ColumnRef { name: left, .. }, TypedExprKind::ColumnRef { name: right, .. }) =
        (&left.kind, &right.kind)
    else {
        return false;
    };
    (left.eq_ignore_ascii_case(left_name) && right.eq_ignore_ascii_case(right_name))
        || (left.eq_ignore_ascii_case(right_name) && right.eq_ignore_ascii_case(left_name))
}

fn extract_l2_distance_threshold(
    expr: &TypedExpr,
    target_variable: &str,
) -> Option<(Vec<f64>, f64)> {
    let TypedExprKind::BinaryLt { left, right } = &expr.kind else {
        return None;
    };
    let threshold = literal_f64(right)?;
    let TypedExprKind::ScalarFunction { func, args } = &left.kind else {
        return None;
    };
    if !matches!(func, ScalarFunction::L2Distance)
        && !matches!(func, ScalarFunction::Generic(name) if name.eq_ignore_ascii_case("l2_distance"))
    {
        return None;
    }
    let [left_arg, right_arg] = args.as_slice() else {
        return None;
    };
    let TypedExprKind::ColumnRef { name, .. } = &left_arg.kind else {
        return None;
    };
    if !name.eq_ignore_ascii_case(&format!("{target_variable}.embedding")) {
        return None;
    }
    let TypedExprKind::Literal(value) = &right_arg.kind else {
        return None;
    };
    Some((literal_vector_f64(value)?, threshold))
}

fn is_l2_distance_expr_or_alias(expr: &TypedExpr, target_variable: &str, alias: &str) -> bool {
    column_ref_name(expr).is_some_and(|name| name.eq_ignore_ascii_case(alias))
        || is_l2_distance_expr_for_variable(expr, target_variable)
}

fn is_l2_distance_expr_for_variable(expr: &TypedExpr, target_variable: &str) -> bool {
    let TypedExprKind::ScalarFunction { func, args } = &expr.kind else {
        return false;
    };
    if !matches!(func, ScalarFunction::L2Distance)
        && !matches!(func, ScalarFunction::Generic(name) if name.eq_ignore_ascii_case("l2_distance"))
    {
        return false;
    }
    let [left_arg, right_arg] = args.as_slice() else {
        return false;
    };
    let TypedExprKind::ColumnRef { name, .. } = &left_arg.kind else {
        return false;
    };
    if !name.eq_ignore_ascii_case(&format!("{target_variable}.embedding")) {
        return false;
    }
    matches!(right_arg.kind, TypedExprKind::Literal(_))
}

fn literal_vector_f64(value: &Value) -> Option<Vec<f64>> {
    let vector = match value {
        Value::Vector(vector) => vector
            .values
            .iter()
            .map(|value| f64::from(*value))
            .collect(),
        Value::Text(text) => parse_vector_text_literal(text)?,
        Value::Array(values) => {
            let mut parsed = Vec::with_capacity(values.len());
            for value in values {
                parsed.push(value_to_f64(value)?);
            }
            parsed
        }
        _ => return None,
    };
    Some(vector)
}

fn parse_vector_text_literal(text: &str) -> Option<Vec<f64>> {
    let trimmed = text.trim();
    let inner = trimmed.strip_prefix('[')?.strip_suffix(']')?;
    if inner.trim().is_empty() {
        return Some(Vec::new());
    }
    inner
        .split(',')
        .map(|part| part.trim().parse::<f64>().ok())
        .collect()
}

fn literal_f64(expr: &TypedExpr) -> Option<f64> {
    match &expr.kind {
        TypedExprKind::Literal(value) => value_to_f64(value),
        _ => None,
    }
}

fn value_to_f64(value: &Value) -> Option<f64> {
    match value {
        Value::Int(value) => Some(f64::from(*value)),
        Value::BigInt(value) => Some(i64_to_f64(*value)),
        Value::Real(value) => Some(f64::from(*value)),
        Value::Double(value) => Some(*value),
        _ => None,
    }
}

fn literal_i64(expr: &TypedExpr) -> Option<i64> {
    match &expr.kind {
        TypedExprKind::Literal(Value::Int(value)) => Some(i64::from(*value)),
        TypedExprKind::Literal(Value::BigInt(value)) => Some(*value),
        _ => None,
    }
}

fn normalize_int_key(value: &mut Value) {
    if let Value::BigInt(raw) = value {
        if let Ok(int_value) = i32::try_from(*raw) {
            *value = Value::Int(int_value);
        }
    }
}

fn column_ref_name(expr: &TypedExpr) -> Option<&str> {
    match &expr.kind {
        TypedExprKind::ColumnRef { name, .. } => Some(name.as_str()),
        _ => None,
    }
}

fn count_return_variable(expr: &TypedExpr) -> Option<&str> {
    let TypedExprKind::AggCount {
        expr: Some(expr),
        distinct: false,
        filter: None,
    } = &expr.kind
    else {
        return None;
    };
    column_ref_name(expr)
}

fn count_distinct_id_return_variable(expr: &TypedExpr) -> Option<&str> {
    let TypedExprKind::AggCount {
        expr: Some(expr),
        distinct: true,
        filter: None,
    } = &expr.kind
    else {
        return None;
    };
    let name = column_ref_name(expr)?;
    let (variable, property) = name.rsplit_once('.')?;
    property.eq_ignore_ascii_case("id").then_some(variable)
}

// ---------------------------------------------------------------------------
// Entry point
// ---------------------------------------------------------------------------

impl Executor {
    fn fast_graph_adjacency_neighbors_cached(
        &self,
        context: &ExecutionContext,
        edge_table_id: RelationId,
        node_id: &Value,
        outgoing: bool,
    ) -> DbResult<Vec<Value>> {
        let generation = self.storage_dml.cache_generation();
        let cache_key = generation
            .and_then(|_| build_hash_key(node_id).ok())
            .map(|node_key| GraphAdjacencyNeighborsCacheKey {
                edge_table_id,
                node_key,
                outgoing,
            });

        if let (Some(cache_key), Some(generation)) = (&cache_key, generation) {
            let cached = self
                .graph_adjacency_neighbors_cache
                .read()
                .map_err(|error| {
                    DbError::internal(format!("graph adjacency cache poisoned: {error}"))
                })?
                .get(cache_key)
                .cloned();
            if let Some((cached_generation, values)) = cached {
                if cached_generation == generation {
                    return Ok(values);
                }
            }
        }

        let mut cursor = self.storage_dml.adjacency_neighbor_cursor(
            context.txn_id,
            &context.snapshot,
            edge_table_id,
            node_id,
            outgoing,
        )?;
        let mut values = Vec::with_capacity(graph_prealloc_capacity(cursor.remaining_hint()));
        while let Some(value) = cursor.next_neighbor() {
            context.check_deadline()?;
            ensure_graph_workset_capacity(context, values.len(), "adjacency neighbor cache")?;
            context.track_memory(estimate_value_bytes(&value).saturating_add(32))?;
            values.push(value);
        }

        if let (Some(cache_key), Some(generation)) = (cache_key, generation) {
            let mut cache = self
                .graph_adjacency_neighbors_cache
                .write()
                .map_err(|error| {
                    DbError::internal(format!("graph adjacency cache poisoned: {error}"))
                })?;
            if cache.len() >= 4096 {
                cache.clear();
            }
            cache.insert(cache_key, (generation, values.clone()));
        }

        Ok(values)
    }

    fn fast_graph_push_adjacency_neighbor_ids(
        &self,
        context: &ExecutionContext,
        edge_table_id: RelationId,
        node_id: &Value,
        outgoing: bool,
        remaining: Option<usize>,
        output: &mut Vec<Value>,
    ) -> DbResult<()> {
        let Some(max_new) = remaining else {
            let values = self.fast_graph_adjacency_neighbors_cached(
                context,
                edge_table_id,
                node_id,
                outgoing,
            )?;
            output.extend(values.into_iter().filter(|value| !value.is_null()));
            return Ok(());
        };
        if max_new == 0 {
            return Ok(());
        }

        let start_len = output.len();
        let generation = self.storage_dml.cache_generation();
        let cache_key = generation
            .and_then(|_| build_hash_key(node_id).ok())
            .map(|node_key| GraphAdjacencyNeighborsCacheKey {
                edge_table_id,
                node_key,
                outgoing,
            });

        if let (Some(cache_key), Some(generation)) = (&cache_key, generation) {
            let cache = self
                .graph_adjacency_neighbors_cache
                .read()
                .map_err(|error| {
                    DbError::internal(format!("graph adjacency cache poisoned: {error}"))
                })?;
            if let Some((cached_generation, values)) = cache.get(cache_key) {
                if *cached_generation == generation {
                    for value in values {
                        if value.is_null() {
                            continue;
                        }
                        output.push(value.clone());
                        if output.len() - start_len >= max_new {
                            break;
                        }
                    }
                    return Ok(());
                }
            }
        }

        let mut cursor = self.storage_dml.adjacency_neighbor_cursor(
            context.txn_id,
            &context.snapshot,
            edge_table_id,
            node_id,
            outgoing,
        )?;
        while let Some(value) = cursor.next_neighbor() {
            context.check_deadline()?;
            if value.is_null() {
                continue;
            }
            ensure_graph_workset_capacity(context, output.len(), "adjacency neighbor traversal")?;
            context.track_memory(estimate_value_bytes(&value).saturating_add(32))?;
            output.push(value);
            if output.len() - start_len >= max_new {
                break;
            }
        }
        Ok(())
    }

    fn fast_graph_adjacency_neighbor_count(
        &self,
        context: &ExecutionContext,
        edge_table_id: RelationId,
        node_id: &Value,
        outgoing: bool,
    ) -> DbResult<u64> {
        let cursor = self.storage_dml.adjacency_neighbor_cursor(
            context.txn_id,
            &context.snapshot,
            edge_table_id,
            node_id,
            outgoing,
        )?;
        Ok(usize_to_u64(cursor.remaining_hint()))
    }

    fn fast_graph_add_count_frontier_node(
        context: &ExecutionContext,
        frontier: &mut HashMap<ValueHashKey, (Value, u64)>,
        mut node_id: Value,
        multiplicity: u64,
    ) -> DbResult<()> {
        if node_id.is_null() || multiplicity == 0 {
            return Ok(());
        }
        normalize_int_key(&mut node_id);
        let key = build_hash_key(&node_id)?;
        match frontier.entry(key) {
            std::collections::hash_map::Entry::Occupied(mut entry) => {
                let count = &mut entry.get_mut().1;
                *count = count.saturating_add(multiplicity);
            }
            std::collections::hash_map::Entry::Vacant(entry) => {
                context.track_memory(estimate_value_bytes(&node_id).saturating_add(64))?;
                entry.insert((node_id, multiplicity));
            }
        }
        Ok(())
    }

    fn fast_graph_count_fixed_outgoing_paths(
        &self,
        context: &ExecutionContext,
        edge_table_id: RelationId,
        start_id: &Value,
        hops: usize,
    ) -> DbResult<u64> {
        if hops == 0 {
            return Ok(0);
        }
        if hops == 1 {
            return self.fast_graph_adjacency_neighbor_count(
                context,
                edge_table_id,
                start_id,
                true,
            );
        }
        if hops == 2 {
            let middle_ids =
                self.fast_graph_adjacency_neighbors_cached(context, edge_table_id, start_id, true)?;
            let mut count = 0u64;
            for mut middle_id in middle_ids {
                context.check_deadline()?;
                if middle_id.is_null() {
                    continue;
                }
                normalize_int_key(&mut middle_id);
                let degree = self.fast_graph_adjacency_neighbor_count(
                    context,
                    edge_table_id,
                    &middle_id,
                    true,
                )?;
                count = count.saturating_add(degree);
            }
            return Ok(count);
        }
        if hops == 3 {
            let first_ids =
                self.fast_graph_adjacency_neighbors_cached(context, edge_table_id, start_id, true)?;
            let mut count = 0u64;
            for mut first_id in first_ids {
                context.check_deadline()?;
                if first_id.is_null() {
                    continue;
                }
                normalize_int_key(&mut first_id);
                let second_ids = self.fast_graph_adjacency_neighbors_cached(
                    context,
                    edge_table_id,
                    &first_id,
                    true,
                )?;
                for mut second_id in second_ids {
                    context.check_deadline()?;
                    if second_id.is_null() {
                        continue;
                    }
                    normalize_int_key(&mut second_id);
                    let degree = self.fast_graph_adjacency_neighbor_count(
                        context,
                        edge_table_id,
                        &second_id,
                        true,
                    )?;
                    count = count.saturating_add(degree);
                }
            }
            return Ok(count);
        }

        let mut frontier = HashMap::new();
        Self::fast_graph_add_count_frontier_node(context, &mut frontier, start_id.clone(), 1)?;
        for _ in 1..hops {
            let mut next = HashMap::new();
            for (node_id, multiplicity) in frontier.into_values() {
                context.check_deadline()?;
                let neighbors = self.fast_graph_adjacency_neighbors_cached(
                    context,
                    edge_table_id,
                    &node_id,
                    true,
                )?;
                for neighbor_id in neighbors {
                    Self::fast_graph_add_count_frontier_node(
                        context,
                        &mut next,
                        neighbor_id,
                        multiplicity,
                    )?;
                }
            }
            if next.is_empty() {
                return Ok(0);
            }
            frontier = next;
        }

        let mut count = 0u64;
        for (node_id, multiplicity) in frontier.into_values() {
            context.check_deadline()?;
            let degree =
                self.fast_graph_adjacency_neighbor_count(context, edge_table_id, &node_id, true)?;
            count = count.saturating_add(multiplicity.saturating_mul(degree));
        }
        Ok(count)
    }

    fn fast_graph_count_distinct_fixed_outgoing_end_ids(
        &self,
        context: &ExecutionContext,
        edge_table_id: RelationId,
        start_id: &Value,
        hops: usize,
    ) -> DbResult<u64> {
        if hops == 0 {
            return Ok(0);
        }

        let mut frontier = HashMap::new();
        Self::fast_graph_add_count_frontier_node(context, &mut frontier, start_id.clone(), 1)?;
        for _ in 0..hops {
            let mut next = HashMap::new();
            for (node_id, _) in frontier.into_values() {
                context.check_deadline()?;
                let neighbors = self.fast_graph_adjacency_neighbors_cached(
                    context,
                    edge_table_id,
                    &node_id,
                    true,
                )?;
                for neighbor_id in neighbors {
                    Self::fast_graph_add_count_frontier_node(context, &mut next, neighbor_id, 1)?;
                }
            }
            if next.is_empty() {
                return Ok(0);
            }
            frontier = next;
        }

        Ok(usize_to_u64(frontier.len()))
    }

    fn fast_graph_id_lookup_cache_get(
        &self,
        edge_table_id: RelationId,
        start_id: &Value,
        hops: u8,
        ordered: bool,
        limit: Option<usize>,
    ) -> DbResult<Option<Vec<Row>>> {
        let Some(generation) = self.storage_dml.cache_generation() else {
            return Ok(None);
        };
        let Ok(start_key) = build_hash_key(start_id) else {
            return Ok(None);
        };
        let cache_key = GraphIdLookupResultCacheKey {
            edge_table_id,
            start_key,
            hops,
            ordered,
            limit,
        };
        let cached = self
            .graph_id_lookup_result_cache
            .read()
            .map_err(|error| DbError::internal(format!("graph id lookup cache poisoned: {error}")))?
            .get(&cache_key)
            .cloned();
        Ok(cached.and_then(|(cached_generation, rows)| {
            (cached_generation == generation).then_some(rows)
        }))
    }

    fn fast_graph_id_lookup_cache_put(
        &self,
        edge_table_id: RelationId,
        start_id: &Value,
        hops: u8,
        ordered: bool,
        limit: Option<usize>,
        rows: &[Row],
    ) -> DbResult<()> {
        let Some(generation) = self.storage_dml.cache_generation() else {
            return Ok(());
        };
        let Ok(start_key) = build_hash_key(start_id) else {
            return Ok(());
        };
        let cache_key = GraphIdLookupResultCacheKey {
            edge_table_id,
            start_key,
            hops,
            ordered,
            limit,
        };
        let mut cache = self.graph_id_lookup_result_cache.write().map_err(|error| {
            DbError::internal(format!("graph id lookup cache poisoned: {error}"))
        })?;
        if cache.len() >= 4096 {
            cache.clear();
        }
        cache.insert(cache_key, (generation, rows.to_vec()));
        Ok(())
    }

    fn fast_graph_collect_target_ids_gt_filter(
        &self,
        context: &ExecutionContext,
        target_table_id: RelationId,
        filter_column_name: &str,
        filter_value: &Value,
    ) -> DbResult<Option<Arc<HashSet<ValueHashKey, join_plans::JoinFxBuildHasher>>>> {
        let Some(target_table) = self
            .catalog_reader
            .get_table_by_id(context.txn_id, target_table_id)?
        else {
            return Ok(None);
        };
        let id_ordinal = self
            .find_column_index(&target_table.columns, "id")
            .unwrap_or(0);
        let Some(filter_ordinal) =
            self.find_column_index(&target_table.columns, filter_column_name)
        else {
            return Ok(None);
        };
        let mut required_ordinals = vec![id_ordinal];
        if filter_ordinal != id_ordinal {
            required_ordinals.push(filter_ordinal);
        }
        let filter_projected_ordinal = required_ordinals
            .iter()
            .position(|ordinal| *ordinal == filter_ordinal)
            .ok_or_else(|| DbError::internal("failed to map graph filter ordinal"))?;
        let Some(projected_columns) =
            self.table_column_ids_for_ordinals(context, target_table_id, &required_ordinals)?
        else {
            return Ok(None);
        };
        let cache_key = self
            .storage_dml
            .cache_generation()
            .and_then(|_| build_hash_key(filter_value).ok())
            .map(|filter_value| GraphTargetFilterIdsCacheKey {
                target_table_id,
                id_ordinal,
                filter_ordinal,
                filter_value,
            });
        if let (Some(cache_key), Some(generation)) =
            (&cache_key, self.storage_dml.cache_generation())
        {
            let cached = self
                .graph_target_filter_ids_cache
                .read()
                .map_err(|error| {
                    DbError::internal(format!("graph target filter cache poisoned: {error}"))
                })?
                .get(cache_key)
                .cloned();
            if let Some((cached_generation, allowed)) = cached {
                if cached_generation == generation {
                    return Ok(Some(allowed));
                }
            }
        }
        let mut stream = self.resolve_scan_stream(
            context,
            target_table_id,
            &ScanAccessPath::SeqScan,
            Some(projected_columns),
        )?;
        let mut allowed = HashSet::<ValueHashKey, join_plans::JoinFxBuildHasher>::with_hasher(
            join_plans::JoinFxBuildHasher::default(),
        );
        while let Some(record) = stream.next()? {
            context.check_deadline()?;
            let number = record
                .row
                .values
                .get(filter_projected_ordinal)
                .unwrap_or(&Value::Null);
            let Some(ordering) = compare_runtime_values(number, filter_value)? else {
                continue;
            };
            if ordering != Ordering::Greater {
                continue;
            }
            let Some(id_value) = record.row.values.first() else {
                continue;
            };
            if id_value.is_null() {
                continue;
            }
            let mut normalized_id = id_value.clone();
            normalize_int_key(&mut normalized_id);
            let id_key = build_hash_key(&normalized_id)?;
            if allowed.insert(id_key) {
                context.track_memory(estimate_value_bytes(&normalized_id).saturating_add(32))?;
            }
        }
        let allowed = Arc::new(allowed);
        if let (Some(cache_key), Some(generation)) =
            (cache_key, self.storage_dml.cache_generation())
        {
            let mut cache = self
                .graph_target_filter_ids_cache
                .write()
                .map_err(|error| {
                    DbError::internal(format!("graph target filter cache poisoned: {error}"))
                })?;
            if cache.len() >= 256 {
                cache.clear();
            }
            cache.insert(cache_key, (generation, Arc::clone(&allowed)));
        }
        Ok(Some(allowed))
    }

    fn hybrid_deep_graph_vector_meta_cached(
        &self,
        context: &ExecutionContext,
        start_table_id: RelationId,
        friend_table_id: RelationId,
        source_table_id: RelationId,
        target_table_id: RelationId,
    ) -> DbResult<Option<HybridDeepGraphVectorMeta>> {
        let cache_key = HybridDeepGraphVectorMetaCacheKey {
            start_table_id,
            friend_table_id,
            source_table_id,
            target_table_id,
        };
        if let Some(generation) = self.storage_dml.cache_generation() {
            if let Some((cached_generation, meta)) = self
                .hybrid_deep_graph_vector_meta_cache
                .read()
                .map_err(|error| {
                    DbError::internal(format!("hybrid graph-vector meta cache poisoned: {error}"))
                })?
                .get(&cache_key)
                .cloned()
            {
                if cached_generation == generation {
                    return Ok(Some(meta));
                }
            }
        }

        let Some(start_columns) = self
            .catalog_reader
            .get_table_by_id(context.txn_id, start_table_id)?
            .map(|table| table.columns)
        else {
            return Ok(None);
        };
        let Some(friend_columns) = self
            .catalog_reader
            .get_table_by_id(context.txn_id, friend_table_id)?
            .map(|table| table.columns)
        else {
            return Ok(None);
        };
        let Some(source_columns) = self
            .catalog_reader
            .get_table_by_id(context.txn_id, source_table_id)?
            .map(|table| table.columns)
        else {
            return Ok(None);
        };
        let Some(target_columns) = self
            .catalog_reader
            .get_table_by_id(context.txn_id, target_table_id)?
            .map(|table| table.columns)
        else {
            return Ok(None);
        };

        let start_id_idx = self.find_column_index(&start_columns, "id").unwrap_or(0);
        let Some(start_tenant_idx) = self.find_column_index(&start_columns, "tenant_id") else {
            return Ok(None);
        };
        let friend_id_idx = self.find_column_index(&friend_columns, "id").unwrap_or(0);
        let Some(friend_tenant_idx) = self.find_column_index(&friend_columns, "tenant_id") else {
            return Ok(None);
        };
        let source_id_idx = self.find_column_index(&source_columns, "id").unwrap_or(0);
        let Some(source_title_idx) = self.find_column_index(&source_columns, "title") else {
            return Ok(None);
        };
        let Some(target_title_idx) = self.find_column_index(&target_columns, "title") else {
            return Ok(None);
        };
        let Some(target_tenant_idx) = self.find_column_index(&target_columns, "tenant_id") else {
            return Ok(None);
        };
        let Some(target_popularity_idx) = self.find_column_index(&target_columns, "popularity")
        else {
            return Ok(None);
        };
        let Some(target_embedding_idx) = self.find_column_index(&target_columns, "embedding")
        else {
            return Ok(None);
        };
        let Some(start_id_index) =
            self.find_first_column_btree_index_for_fast_graph(context, start_table_id)?
        else {
            return Ok(None);
        };
        let Some(friend_id_index) =
            self.find_first_column_btree_index_for_fast_graph(context, friend_table_id)?
        else {
            return Ok(None);
        };
        let Some(source_id_index) =
            self.find_first_column_btree_index_for_fast_graph(context, source_table_id)?
        else {
            return Ok(None);
        };
        let Some(target_id_index) =
            self.find_first_column_btree_index_for_fast_graph(context, target_table_id)?
        else {
            return Ok(None);
        };
        let meta = HybridDeepGraphVectorMeta {
            start_id_idx,
            start_tenant_idx,
            friend_id_idx,
            friend_tenant_idx,
            source_id_idx,
            source_title_idx,
            target_title_idx,
            target_tenant_idx,
            target_popularity_idx,
            target_embedding_idx,
            start_id_index,
            friend_id_index,
            source_id_index,
            target_id_index,
        };
        if let Some(generation) = self.storage_dml.cache_generation() {
            let mut cache = self
                .hybrid_deep_graph_vector_meta_cache
                .write()
                .map_err(|error| {
                    DbError::internal(format!("hybrid graph-vector meta cache poisoned: {error}"))
                })?;
            if cache.len() >= 256 {
                cache.clear();
            }
            cache.insert(cache_key, (generation, meta.clone()));
        }
        Ok(Some(meta))
    }

    /// Execute a Cypher query plan.
    pub(super) fn execute_cypher_query(
        &self,
        plan: &CypherQueryPlan,
        context: &ExecutionContext,
    ) -> DbResult<ExecutionResult> {
        context.check_deadline()?;

        for hint in self.describe_cypher_query_graph_plans(context, plan) {
            debug!(
                clause_kind = ?hint.clause_kind,
                clause_index = hint.clause_index,
                pattern_index = hint.pattern_index,
                source = ?hint.plan.source,
                fallback_source = ?hint.plan.fallback_source,
                estimated_rows = hint.plan.estimated_rows,
                reason = hint.plan.reason.as_deref().unwrap_or(""),
                "cypher graph access plan"
            );
        }
        for hint in self.describe_cypher_query_graph_procedure_plans(context.txn_id, plan) {
            debug!(
                clause_index = hint.clause_index,
                procedure = %hint.procedure,
                source = ?hint.plan.source,
                fallback_source = ?hint.plan.fallback_source,
                projection = hint.plan.projection_name.as_deref().unwrap_or("unknown"),
                snapshot_generation = hint.projection.snapshot.generation,
                refresh_policy = ?hint.projection.snapshot.refresh_policy,
                refreshed_at_epoch_millis = hint.projection.snapshot.refreshed_at_epoch_millis,
                weighted = hint.weighted,
                estimated_rows = hint.plan.estimated_rows,
                projection_ready = hint.projection_ready,
                projection_state = ?hint.projection.state,
                build_mode = ?hint.projection.build_mode,
                node_count = hint.projection_ready.then_some(hint.projection.stats).and_then(|stats| stats.node_count),
                edge_count = hint.projection_ready.then_some(hint.projection.stats).map(|stats| stats.edge_count),
                reason = hint.plan.reason.as_deref().unwrap_or(""),
                "cypher graph procedure plan"
            );
        }

        if let Some(result) = self.try_execute_fast_one_hop_id_lookup(plan, context)? {
            return Ok(result);
        }
        if let Some(result) = self.try_execute_fast_one_hop_endpoint_id_lookup(plan, context)? {
            return Ok(result);
        }
        if let Some(result) = self.try_execute_fast_anchored_path_count(plan, context)? {
            return Ok(result);
        }
        if let Some(result) = self.try_execute_fast_two_hop_id_lookup(plan, context)? {
            return Ok(result);
        }
        if let Some(result) = self.try_execute_fast_three_hop_id_lookup(plan, context)? {
            return Ok(result);
        }
        if let Some(result) = self.try_execute_fast_unanchored_one_hop_count(plan, context)? {
            return Ok(result);
        }
        if let Some(result) = self.try_execute_fast_unanchored_one_hop_group_count(plan, context)? {
            return Ok(result);
        }
        if let Some(result) = self.try_execute_fast_unanchored_target_filter_limit(plan, context)? {
            return Ok(result);
        }
        if let Some(result) = self.try_execute_fast_unanchored_edge_filter_limit(plan, context)? {
            return Ok(result);
        }
        if let Some(result) =
            self.try_execute_fast_unanchored_edge_eq_filter_limit(plan, context)?
        {
            return Ok(result);
        }
        if let Some(result) = self.try_execute_fast_multi_out_limit(plan, context)? {
            return Ok(result);
        }
        if let Some(result) = self.try_execute_fast_unanchored_one_hop_limit(plan, context)? {
            return Ok(result);
        }
        if let Some(result) = self.try_execute_fast_unanchored_two_hop_limit(plan, context)? {
            return Ok(result);
        }
        if let Some(result) = self.try_execute_fast_hybrid_deep_graph_vector_rel(plan, context)? {
            return Ok(result);
        }
        if let Some(result) = self.try_execute_fast_hybrid_graph_vector_rel(plan, context)? {
            return Ok(result);
        }

        // 0. Execute pipeline operations (UNWIND, WITH) to produce initial bindings.
        let mut bindings = vec![BindingRow::new()];
        for op in &plan.pipeline {
            context.check_deadline()?;
            match op {
                CypherPipelineOp::Unwind(u) => {
                    bindings = self.execute_cypher_unwind(context, u, bindings)?;
                }
                CypherPipelineOp::With(ref w) => {
                    bindings = self.execute_cypher_with(context, w, bindings)?;
                }
                CypherPipelineOp::Match(m) => {
                    bindings = self.execute_cypher_match(context, m, bindings)?;
                }
                CypherPipelineOp::ProcedureCall(call) => {
                    bindings = self.execute_cypher_procedure_call(context, call, bindings)?;
                }
                CypherPipelineOp::CallSubquery(subquery) => {
                    bindings = self.execute_cypher_call_subquery(context, subquery, bindings)?;
                }
                CypherPipelineOp::Foreach(foreach) => {
                    bindings = self.execute_cypher_foreach(context, foreach, bindings)?;
                }
            }
        }

        // 1. Execute MATCH / OPTIONAL MATCH clauses -> produce binding rows.
        for match_clause in &plan.matches {
            context.check_deadline()?;
            bindings = self.execute_cypher_match(context, match_clause, bindings)?;
        }

        // 2. Execute CREATE clauses -> insert nodes/edges.
        let mut created_count = 0u64;
        for create_clause in &plan.creates {
            context.check_deadline()?;
            let (new_bindings, count) =
                self.execute_cypher_create(context, create_clause, bindings)?;
            bindings = new_bindings;
            created_count += count;
        }

        // 3. Execute MERGE clauses -> match-or-create.
        for merge_clause in &plan.merges {
            context.check_deadline()?;
            bindings = self.execute_cypher_merge(context, merge_clause, bindings)?;
        }

        // 4. Execute SET clauses -> update properties.
        for set_item in &plan.sets {
            context.check_deadline()?;
            self.execute_cypher_set(context, set_item, &mut bindings)?;
        }

        // 5. Execute DELETE clauses -> delete rows.
        let mut delete_count = 0u64;
        for delete_clause in &plan.deletes {
            context.check_deadline()?;
            delete_count += self.execute_cypher_delete(context, delete_clause, &bindings)?;
        }

        // 6. Build RETURN result, or fall back to a Command tag.
        let left_result = if plan.returns.is_empty() {
            let (tag, rows_affected) = if !plan.deletes.is_empty() {
                ("DELETE", delete_count)
            } else if !plan.creates.is_empty() {
                ("CREATE", created_count)
            } else if !plan.merges.is_empty() {
                ("MERGE", usize_to_u64(bindings.len()))
            } else if !plan.sets.is_empty() {
                ("SET", usize_to_u64(bindings.len()))
            } else {
                ("CYPHER", usize_to_u64(bindings.len()))
            };
            ExecutionResult::Command {
                tag: tag.to_owned(),
                rows_affected,
            }
        } else {
            let rows = self.project_cypher_return(
                context,
                &plan.returns,
                plan.distinct,
                &plan.order_by,
                plan.skip.as_ref(),
                plan.limit.as_ref(),
                bindings,
            )?;
            let columns = plan.returns.iter().map(|r| r.field.clone()).collect();
            ExecutionResult::Query { columns, rows }
        };

        // 7. Handle UNION [ALL] if present.
        if let Some(ref union_plan) = plan.union {
            context.check_deadline()?;
            let right_result = self.execute_cypher_query(&union_plan.right, context)?;

            // Combine the results from left and right sides.
            match (left_result, right_result) {
                (
                    ExecutionResult::Query {
                        columns,
                        rows: mut left_rows,
                    },
                    ExecutionResult::Query {
                        rows: right_rows, ..
                    },
                ) => {
                    left_rows.extend(right_rows);

                    if !union_plan.all {
                        // UNION (distinct): deduplicate rows using value-based hashing.
                        left_rows = dedup_rows_by_values(left_rows)?;
                    }

                    Ok(ExecutionResult::Query {
                        columns,
                        rows: left_rows,
                    })
                }
                (left, _) => Ok(left),
            }
        } else {
            Ok(left_result)
        }
    }

    fn try_execute_fast_one_hop_id_lookup(
        &self,
        plan: &CypherQueryPlan,
        context: &ExecutionContext,
    ) -> DbResult<Option<ExecutionResult>> {
        if plan.pipeline.len() + plan.matches.len() != 1
            || !plan.creates.is_empty()
            || !plan.merges.is_empty()
            || !plan.sets.is_empty()
            || !plan.deletes.is_empty()
            || plan.distinct
            || plan.skip.is_some()
            || plan.union.is_some()
            || plan.returns.len() != 1
        {
            return Ok(None);
        }

        let match_clause = match plan.pipeline.as_slice() {
            [CypherPipelineOp::Match(match_clause)] => match_clause,
            [] => &plan.matches[0],
            _ => return Ok(None),
        };
        if match_clause.optional || match_clause.patterns.len() != 1 {
            return Ok(None);
        }
        let pattern = &match_clause.patterns[0];
        if pattern.path_function.is_some()
            || pattern.nodes.len() != 2
            || pattern.relationships.len() != 1
        {
            return Ok(None);
        }

        let start = &pattern.nodes[0];
        let end = &pattern.nodes[1];
        let rel = &pattern.relationships[0];
        let Some(start_variable) = start.variable.as_deref() else {
            return Ok(None);
        };
        let Some(end_variable) = end.variable.as_deref() else {
            return Ok(None);
        };
        let expected_return = format!("{end_variable}.id");
        if start.table_id.is_none()
            || end.table_id.is_none()
            || !end.properties.is_empty()
            || rel.table_id.is_none()
            || rel.direction != CypherRelDirection::Outgoing
            || rel.variable.is_some()
            || rel.min_hops.is_some()
            || rel.max_hops.is_some()
            || !rel.properties.is_empty()
        {
            return Ok(None);
        }

        if column_ref_name(&plan.returns[0].expr) != Some(expected_return.as_str()) {
            return Ok(None);
        }
        if plan
            .order_by
            .iter()
            .any(|sort| column_ref_name(&sort.expr) != Some(expected_return.as_str()))
        {
            return Ok(None);
        }

        let Some(edge_table_id) = rel.table_id else {
            return Ok(None);
        };
        let Some(mut start_id) =
            extract_start_id_literal(start, match_clause.filter.as_ref(), start_variable)
        else {
            return Ok(None);
        };
        normalize_int_key(&mut start_id);
        let ordered = !plan.order_by.is_empty();
        let limit = plan
            .limit
            .as_ref()
            .and_then(literal_i64)
            .and_then(|value| usize::try_from(value.max(0)).ok());
        if let Some(rows) =
            self.fast_graph_id_lookup_cache_get(edge_table_id, &start_id, 1, ordered, limit)?
        {
            let columns = plan.returns.iter().map(|r| r.field.clone()).collect();
            return Ok(Some(ExecutionResult::Query { columns, rows }));
        }

        let mut ids = Vec::with_capacity(limit.unwrap_or(0).min(1024));
        let remaining = if ordered { None } else { limit };
        if self
            .fast_graph_push_adjacency_neighbor_ids(
                context,
                edge_table_id,
                &start_id,
                true,
                remaining,
                &mut ids,
            )
            .is_err()
        {
            return Ok(None);
        }

        if !plan.order_by.is_empty() {
            ids.sort_by(|left, right| {
                compare_runtime_values(left, right)
                    .ok()
                    .flatten()
                    .unwrap_or(std::cmp::Ordering::Equal)
            });
        }

        if let Some(limit) = limit {
            ids.truncate(limit);
        }

        let rows: Vec<Row> = ids.into_iter().map(|id| Row::new(vec![id])).collect();
        self.fast_graph_id_lookup_cache_put(edge_table_id, &start_id, 1, ordered, limit, &rows)?;
        let columns = plan.returns.iter().map(|r| r.field.clone()).collect();
        Ok(Some(ExecutionResult::Query { columns, rows }))
    }

    fn try_execute_fast_one_hop_endpoint_id_lookup(
        &self,
        plan: &CypherQueryPlan,
        context: &ExecutionContext,
    ) -> DbResult<Option<ExecutionResult>> {
        if plan.pipeline.len() + plan.matches.len() != 1
            || !plan.creates.is_empty()
            || !plan.merges.is_empty()
            || !plan.sets.is_empty()
            || !plan.deletes.is_empty()
            || plan.distinct
            || plan.skip.is_some()
            || plan.union.is_some()
            || plan.returns.len() != 1
        {
            return Ok(None);
        }

        let match_clause = match plan.pipeline.as_slice() {
            [CypherPipelineOp::Match(match_clause)] => match_clause,
            [] => &plan.matches[0],
            _ => return Ok(None),
        };
        if match_clause.optional || match_clause.patterns.len() != 1 {
            return Ok(None);
        }
        let pattern = &match_clause.patterns[0];
        if pattern.path_function.is_some()
            || pattern.nodes.len() != 2
            || pattern.relationships.len() != 1
        {
            return Ok(None);
        }

        let left = &pattern.nodes[0];
        let right = &pattern.nodes[1];
        let rel = &pattern.relationships[0];
        let Some(left_variable) = left.variable.as_deref() else {
            return Ok(None);
        };
        let Some(right_variable) = right.variable.as_deref() else {
            return Ok(None);
        };
        if left.table_id.is_none()
            || right.table_id.is_none()
            || rel.table_id.is_none()
            || rel.variable.is_some()
            || rel.min_hops.is_some()
            || rel.max_hops.is_some()
            || !rel.properties.is_empty()
        {
            return Ok(None);
        }

        let return_name = column_ref_name(&plan.returns[0].expr);
        let returns_left = return_name.is_some_and(|name| is_graph_id_ref(name, left_variable));
        let returns_right = return_name.is_some_and(|name| is_graph_id_ref(name, right_variable));
        if !returns_left && !returns_right {
            return Ok(None);
        }
        if plan
            .order_by
            .iter()
            .any(|sort| column_ref_name(&sort.expr) != return_name)
        {
            return Ok(None);
        }

        let left_id = extract_start_id_literal(left, match_clause.filter.as_ref(), left_variable);
        let right_id =
            extract_start_id_literal(right, match_clause.filter.as_ref(), right_variable);
        let (mut anchor_id, lookup_outgoing): (Value, Vec<bool>) =
            match (left_id, right_id, returns_left, returns_right) {
                (Some(anchor_id), None, false, true) if right.properties.is_empty() => {
                    let directions = match rel.direction {
                        CypherRelDirection::Outgoing => vec![true],
                        CypherRelDirection::Incoming => vec![false],
                        CypherRelDirection::Both => vec![true, false],
                    };
                    (anchor_id, directions)
                }
                (None, Some(anchor_id), true, false) if left.properties.is_empty() => {
                    let directions = match rel.direction {
                        CypherRelDirection::Outgoing => vec![false],
                        CypherRelDirection::Incoming => vec![true],
                        CypherRelDirection::Both => vec![true, false],
                    };
                    (anchor_id, directions)
                }
                _ => return Ok(None),
            };
        normalize_int_key(&mut anchor_id);

        let Some(edge_table_id) = rel.table_id else {
            return Ok(None);
        };
        let ordered = !plan.order_by.is_empty();
        let limit = plan
            .limit
            .as_ref()
            .and_then(literal_i64)
            .and_then(|value| usize::try_from(value.max(0)).ok());

        let mut ids = Vec::new();
        for outgoing in lookup_outgoing {
            let remaining = if ordered {
                None
            } else {
                limit.map(|limit| limit.saturating_sub(ids.len()))
            };
            if self
                .fast_graph_push_adjacency_neighbor_ids(
                    context,
                    edge_table_id,
                    &anchor_id,
                    outgoing,
                    remaining,
                    &mut ids,
                )
                .is_err()
            {
                return Ok(None);
            }
            if !ordered && limit.is_some_and(|limit| ids.len() >= limit) {
                break;
            }
        }

        if ordered {
            ids.sort_by(|left, right| {
                compare_runtime_values(left, right)
                    .ok()
                    .flatten()
                    .unwrap_or(std::cmp::Ordering::Equal)
            });
        }

        if let Some(limit) = limit {
            ids.truncate(limit);
        }

        let mut rows = Vec::with_capacity(ids.len());
        let mut result_bytes = 0u64;
        for id in ids {
            let row = Row::new(vec![id]);
            result_bytes =
                ensure_result_bytes_fit_and_track_query_row(context, &row, result_bytes)?;
            rows.push(row);
        }
        let columns = plan.returns.iter().map(|r| r.field.clone()).collect();
        Ok(Some(ExecutionResult::Query { columns, rows }))
    }

    fn try_execute_fast_anchored_path_count(
        &self,
        plan: &CypherQueryPlan,
        context: &ExecutionContext,
    ) -> DbResult<Option<ExecutionResult>> {
        if plan.pipeline.len() + plan.matches.len() != 1
            || !plan.creates.is_empty()
            || !plan.merges.is_empty()
            || !plan.sets.is_empty()
            || !plan.deletes.is_empty()
            || plan.distinct
            || plan.skip.is_some()
            || plan.limit.is_some()
            || plan.union.is_some()
            || !plan.order_by.is_empty()
            || plan.returns.len() != 1
        {
            return Ok(None);
        }

        let match_clause = match plan.pipeline.as_slice() {
            [CypherPipelineOp::Match(match_clause)] => match_clause,
            [] => &plan.matches[0],
            _ => return Ok(None),
        };
        if match_clause.optional || match_clause.patterns.len() != 1 {
            return Ok(None);
        }
        let pattern = &match_clause.patterns[0];
        let hops = pattern.relationships.len();
        if pattern.path_function.is_some() || hops == 0 || pattern.nodes.len() != hops + 1 {
            return Ok(None);
        }

        let Some(start) = pattern.nodes.first() else {
            return Ok(None);
        };
        let Some(end) = pattern.nodes.last() else {
            return Ok(None);
        };
        let Some(start_variable) = start.variable.as_deref() else {
            return Ok(None);
        };
        let Some(end_variable) = end.variable.as_deref() else {
            return Ok(None);
        };
        let count_all_end = count_return_variable(&plan.returns[0].expr)
            .is_some_and(|name| name.eq_ignore_ascii_case(end_variable));
        let count_distinct_end_id = count_distinct_id_return_variable(&plan.returns[0].expr)
            .is_some_and(|name| name.eq_ignore_ascii_case(end_variable));
        if !count_all_end && !count_distinct_end_id {
            return Ok(None);
        }
        if start.table_id.is_none()
            || pattern
                .nodes
                .iter()
                .skip(1)
                .any(|node| node.table_id.is_none() || !node.properties.is_empty())
        {
            return Ok(None);
        }

        let Some(edge_table_id) = pattern.relationships.first().and_then(|rel| rel.table_id) else {
            return Ok(None);
        };
        if pattern.relationships.iter().any(|rel| {
            rel.table_id != Some(edge_table_id)
                || rel.direction != CypherRelDirection::Outgoing
                || rel.variable.is_some()
                || rel.min_hops.is_some()
                || rel.max_hops.is_some()
                || !rel.properties.is_empty()
        }) {
            return Ok(None);
        }

        let Some(mut start_id) =
            extract_start_id_literal(start, match_clause.filter.as_ref(), start_variable)
        else {
            return Ok(None);
        };
        normalize_int_key(&mut start_id);

        let count_result = if count_distinct_end_id {
            self.fast_graph_count_distinct_fixed_outgoing_end_ids(
                context,
                edge_table_id,
                &start_id,
                hops,
            )
        } else {
            self.fast_graph_count_fixed_outgoing_paths(context, edge_table_id, &start_id, hops)
        };
        let count = match count_result {
            Ok(count) => count,
            Err(_) => return Ok(None),
        };

        let row = Row::new(vec![Value::BigInt(
            i64::try_from(count).unwrap_or(i64::MAX),
        )]);
        ensure_result_bytes_fit_and_track_query_row(context, &row, 0)?;
        Ok(Some(ExecutionResult::Query {
            columns: plan.returns.iter().map(|r| r.field.clone()).collect(),
            rows: vec![row],
        }))
    }

    fn try_execute_fast_two_hop_id_lookup(
        &self,
        plan: &CypherQueryPlan,
        context: &ExecutionContext,
    ) -> DbResult<Option<ExecutionResult>> {
        if plan.pipeline.len() + plan.matches.len() != 1
            || !plan.creates.is_empty()
            || !plan.merges.is_empty()
            || !plan.sets.is_empty()
            || !plan.deletes.is_empty()
            || plan.distinct
            || plan.skip.is_some()
            || plan.union.is_some()
            || plan.returns.len() != 1
        {
            return Ok(None);
        }

        let match_clause = match plan.pipeline.as_slice() {
            [CypherPipelineOp::Match(match_clause)] => match_clause,
            [] => &plan.matches[0],
            _ => return Ok(None),
        };
        if match_clause.optional || match_clause.patterns.len() != 1 {
            return Ok(None);
        }
        let pattern = &match_clause.patterns[0];
        if pattern.path_function.is_some()
            || pattern.nodes.len() != 3
            || pattern.relationships.len() != 2
        {
            return Ok(None);
        }

        let start = &pattern.nodes[0];
        let middle = &pattern.nodes[1];
        let end = &pattern.nodes[2];
        let first_rel = &pattern.relationships[0];
        let second_rel = &pattern.relationships[1];
        let Some(start_variable) = start.variable.as_deref() else {
            return Ok(None);
        };
        let Some(end_variable) = end.variable.as_deref() else {
            return Ok(None);
        };
        let expected_return = format!("{end_variable}.id");
        if start.table_id.is_none()
            || middle.table_id.is_none()
            || end.table_id.is_none()
            || !middle.properties.is_empty()
            || !end.properties.is_empty()
            || first_rel.table_id.is_none()
            || second_rel.table_id.is_none()
            || first_rel.table_id != second_rel.table_id
            || first_rel.direction != CypherRelDirection::Outgoing
            || second_rel.direction != CypherRelDirection::Outgoing
            || first_rel.variable.is_some()
            || second_rel.variable.is_some()
            || first_rel.min_hops.is_some()
            || first_rel.max_hops.is_some()
            || second_rel.min_hops.is_some()
            || second_rel.max_hops.is_some()
            || !first_rel.properties.is_empty()
            || !second_rel.properties.is_empty()
        {
            return Ok(None);
        }

        if column_ref_name(&plan.returns[0].expr) != Some(expected_return.as_str()) {
            return Ok(None);
        }
        if plan
            .order_by
            .iter()
            .any(|sort| column_ref_name(&sort.expr) != Some(expected_return.as_str()))
        {
            return Ok(None);
        }

        let Some(edge_table_id) = first_rel.table_id else {
            return Ok(None);
        };
        let Some(mut start_id) =
            extract_start_id_literal(start, match_clause.filter.as_ref(), start_variable)
        else {
            return Ok(None);
        };
        normalize_int_key(&mut start_id);
        let ordered = !plan.order_by.is_empty();
        let limit = plan
            .limit
            .as_ref()
            .and_then(literal_i64)
            .and_then(|value| usize::try_from(value.max(0)).ok());
        if let Some(rows) =
            self.fast_graph_id_lookup_cache_get(edge_table_id, &start_id, 2, ordered, limit)?
        {
            let columns = plan.returns.iter().map(|r| r.field.clone()).collect();
            return Ok(Some(ExecutionResult::Query { columns, rows }));
        }

        let middle_ids = match self.fast_graph_adjacency_neighbors_cached(
            context,
            edge_table_id,
            &start_id,
            true,
        ) {
            Ok(ids) => ids,
            Err(_) => return Ok(None),
        };

        let mut ids = Vec::with_capacity(limit.unwrap_or(0).min(1024));
        for mut middle_id in middle_ids {
            if middle_id.is_null() {
                continue;
            }
            normalize_int_key(&mut middle_id);
            let remaining = if ordered {
                None
            } else {
                limit.map(|limit| limit.saturating_sub(ids.len()))
            };
            if self
                .fast_graph_push_adjacency_neighbor_ids(
                    context,
                    edge_table_id,
                    &middle_id,
                    true,
                    remaining,
                    &mut ids,
                )
                .is_err()
            {
                return Ok(None);
            }
            if !ordered && limit.is_some_and(|limit| ids.len() >= limit) {
                break;
            }
        }

        if !plan.order_by.is_empty() {
            ids.sort_by(|left, right| {
                compare_runtime_values(left, right)
                    .ok()
                    .flatten()
                    .unwrap_or(std::cmp::Ordering::Equal)
            });
        }

        if let Some(limit) = limit {
            ids.truncate(limit);
        }

        let rows: Vec<Row> = ids.into_iter().map(|id| Row::new(vec![id])).collect();
        self.fast_graph_id_lookup_cache_put(edge_table_id, &start_id, 2, ordered, limit, &rows)?;
        let columns = plan.returns.iter().map(|r| r.field.clone()).collect();
        Ok(Some(ExecutionResult::Query { columns, rows }))
    }

    fn try_execute_fast_three_hop_id_lookup(
        &self,
        plan: &CypherQueryPlan,
        context: &ExecutionContext,
    ) -> DbResult<Option<ExecutionResult>> {
        if plan.pipeline.len() + plan.matches.len() != 1
            || !plan.creates.is_empty()
            || !plan.merges.is_empty()
            || !plan.sets.is_empty()
            || !plan.deletes.is_empty()
            || plan.distinct
            || plan.skip.is_some()
            || plan.union.is_some()
            || plan.returns.len() != 1
        {
            return Ok(None);
        }

        let match_clause = match plan.pipeline.as_slice() {
            [CypherPipelineOp::Match(match_clause)] => match_clause,
            [] => &plan.matches[0],
            _ => return Ok(None),
        };
        if match_clause.optional || match_clause.patterns.len() != 1 {
            return Ok(None);
        }
        let pattern = &match_clause.patterns[0];
        if pattern.path_function.is_some()
            || pattern.nodes.len() != 4
            || pattern.relationships.len() != 3
        {
            return Ok(None);
        }

        let start = &pattern.nodes[0];
        let first_mid = &pattern.nodes[1];
        let second_mid = &pattern.nodes[2];
        let end = &pattern.nodes[3];
        let Some(start_variable) = start.variable.as_deref() else {
            return Ok(None);
        };
        let Some(end_variable) = end.variable.as_deref() else {
            return Ok(None);
        };
        let expected_return = format!("{end_variable}.id");
        if start.table_id.is_none()
            || first_mid.table_id.is_none()
            || second_mid.table_id.is_none()
            || end.table_id.is_none()
            || !first_mid.properties.is_empty()
            || !second_mid.properties.is_empty()
            || !end.properties.is_empty()
        {
            return Ok(None);
        }

        let Some(first_rel_table_id) = pattern.relationships[0].table_id else {
            return Ok(None);
        };
        if pattern.relationships.iter().any(|rel| {
            rel.table_id != Some(first_rel_table_id)
                || rel.direction != CypherRelDirection::Outgoing
                || rel.variable.is_some()
                || rel.min_hops.is_some()
                || rel.max_hops.is_some()
                || !rel.properties.is_empty()
        }) {
            return Ok(None);
        }

        if column_ref_name(&plan.returns[0].expr) != Some(expected_return.as_str()) {
            return Ok(None);
        }
        if plan
            .order_by
            .iter()
            .any(|sort| column_ref_name(&sort.expr) != Some(expected_return.as_str()))
        {
            return Ok(None);
        }

        let Some(mut start_id) =
            extract_start_id_literal(start, match_clause.filter.as_ref(), start_variable)
        else {
            return Ok(None);
        };
        normalize_int_key(&mut start_id);
        let ordered = !plan.order_by.is_empty();
        let limit = plan
            .limit
            .as_ref()
            .and_then(literal_i64)
            .and_then(|value| usize::try_from(value.max(0)).ok());
        if let Some(rows) =
            self.fast_graph_id_lookup_cache_get(first_rel_table_id, &start_id, 3, ordered, limit)?
        {
            let columns = plan.returns.iter().map(|r| r.field.clone()).collect();
            return Ok(Some(ExecutionResult::Query { columns, rows }));
        }

        let first_ids = match self.fast_graph_adjacency_neighbors_cached(
            context,
            first_rel_table_id,
            &start_id,
            true,
        ) {
            Ok(ids) => ids,
            Err(_) => return Ok(None),
        };

        let mut ids = Vec::with_capacity(limit.unwrap_or(0).min(1024));
        'outer: for mut first_id in first_ids {
            if first_id.is_null() {
                continue;
            }
            normalize_int_key(&mut first_id);
            let second_ids = match self.fast_graph_adjacency_neighbors_cached(
                context,
                first_rel_table_id,
                &first_id,
                true,
            ) {
                Ok(ids) => ids,
                Err(_) => return Ok(None),
            };
            for mut second_id in second_ids {
                if second_id.is_null() {
                    continue;
                }
                normalize_int_key(&mut second_id);
                let remaining = if ordered {
                    None
                } else {
                    limit.map(|limit| limit.saturating_sub(ids.len()))
                };
                if self
                    .fast_graph_push_adjacency_neighbor_ids(
                        context,
                        first_rel_table_id,
                        &second_id,
                        true,
                        remaining,
                        &mut ids,
                    )
                    .is_err()
                {
                    return Ok(None);
                }
                if !ordered && limit.is_some_and(|limit| ids.len() >= limit) {
                    break 'outer;
                }
            }
        }

        if !plan.order_by.is_empty() {
            ids.sort_by(|left, right| {
                compare_runtime_values(left, right)
                    .ok()
                    .flatten()
                    .unwrap_or(std::cmp::Ordering::Equal)
            });
        }

        if let Some(limit) = limit {
            ids.truncate(limit);
        }

        let rows: Vec<Row> = ids.into_iter().map(|id| Row::new(vec![id])).collect();
        self.fast_graph_id_lookup_cache_put(
            first_rel_table_id,
            &start_id,
            3,
            ordered,
            limit,
            &rows,
        )?;
        let columns = plan.returns.iter().map(|r| r.field.clone()).collect();
        Ok(Some(ExecutionResult::Query { columns, rows }))
    }

    fn try_execute_fast_unanchored_edge_filter_limit(
        &self,
        plan: &CypherQueryPlan,
        context: &ExecutionContext,
    ) -> DbResult<Option<ExecutionResult>> {
        if plan.pipeline.len() + plan.matches.len() != 1
            || !plan.creates.is_empty()
            || !plan.merges.is_empty()
            || !plan.sets.is_empty()
            || !plan.deletes.is_empty()
            || plan.distinct
            || plan.skip.is_some()
            || plan.union.is_some()
            || !plan.order_by.is_empty()
            || plan.returns.len() != 1
        {
            return Ok(None);
        }
        let Some(limit) = plan
            .limit
            .as_ref()
            .and_then(literal_i64)
            .and_then(|value| usize::try_from(value.max(0)).ok())
        else {
            return Ok(None);
        };
        if limit == 0 {
            return Ok(Some(ExecutionResult::Query {
                columns: plan.returns.iter().map(|r| r.field.clone()).collect(),
                rows: Vec::new(),
            }));
        }

        let match_clause = match plan.pipeline.as_slice() {
            [CypherPipelineOp::Match(match_clause)] => match_clause,
            [] => &plan.matches[0],
            _ => return Ok(None),
        };
        if match_clause.optional || match_clause.patterns.len() != 1 {
            return Ok(None);
        }
        let pattern = &match_clause.patterns[0];
        if pattern.path_function.is_some()
            || pattern.nodes.len() != 2
            || pattern.relationships.len() != 1
        {
            return Ok(None);
        }

        let start = &pattern.nodes[0];
        let end = &pattern.nodes[1];
        let rel = &pattern.relationships[0];
        let Some(end_variable) = end.variable.as_deref() else {
            return Ok(None);
        };
        let Some(rel_variable) = rel.variable.as_deref() else {
            return Ok(None);
        };
        let expected_return = format!("{end_variable}.id");
        if column_ref_name(&plan.returns[0].expr) != Some(expected_return.as_str()) {
            return Ok(None);
        }
        if start.table_id.is_none()
            || end.table_id.is_none()
            || !start.properties.is_empty()
            || !end.properties.is_empty()
            || rel.table_id.is_none()
            || rel.direction != CypherRelDirection::Outgoing
            || rel.min_hops.is_some()
            || rel.max_hops.is_some()
            || !rel.properties.is_empty()
        {
            return Ok(None);
        }

        let Some(filter_value) = match_clause.filter.as_ref().and_then(|filter| {
            exact_named_column_literal_gt(filter, &format!("{rel_variable}.weight"))
        }) else {
            return Ok(None);
        };
        let Some(edge_table_id) = rel.table_id else {
            return Ok(None);
        };
        let ((_, tgt_col_idx), _) = self.resolve_edge_endpoint_columns_for_rel(
            context,
            edge_table_id,
            rel.rel_type.as_deref(),
        )?;
        let Some(edge_table) = self
            .catalog_reader
            .get_table_by_id(context.txn_id, edge_table_id)?
        else {
            return Ok(None);
        };
        let Some(weight_col_idx) = self.find_column_index(&edge_table.columns, "weight") else {
            return Ok(None);
        };
        let Some(projected_columns) = self.table_column_ids_for_ordinals(
            context,
            edge_table_id,
            &[tgt_col_idx, weight_col_idx],
        )?
        else {
            return Ok(None);
        };
        let cache_key = self
            .storage_dml
            .cache_generation()
            .and_then(|_| build_hash_key(&filter_value).ok())
            .map(|filter_value| GraphEdgeFilterLimitRowsCacheKey {
                edge_table_id,
                target_col_idx: tgt_col_idx,
                weight_col_idx,
                filter_value,
                limit,
            });
        if let (Some(cache_key), Some(generation)) =
            (&cache_key, self.storage_dml.cache_generation())
        {
            if let Some((cached_generation, cached_rows)) = self
                .graph_edge_filter_limit_rows_cache
                .read()
                .map_err(|error| {
                    DbError::internal(format!("graph edge filter cache poisoned: {error}"))
                })?
                .get(cache_key)
                .cloned()
            {
                if cached_generation == generation {
                    let mut result_bytes = 0u64;
                    for row in &cached_rows {
                        result_bytes = ensure_result_bytes_fit_and_track_query_row(
                            context,
                            row,
                            result_bytes,
                        )?;
                    }
                    return Ok(Some(ExecutionResult::Query {
                        columns: plan.returns.iter().map(|r| r.field.clone()).collect(),
                        rows: cached_rows,
                    }));
                }
            }
        }
        let mut stream = self.resolve_scan_stream(
            context,
            edge_table_id,
            &ScanAccessPath::SeqScan,
            Some(projected_columns),
        )?;
        let mut rows = Vec::with_capacity(limit.min(1024));
        let mut result_bytes = 0u64;
        while let Some(record) = stream.next()? {
            context.check_deadline()?;
            let target_id = record.row.values.first().unwrap_or(&Value::Null);
            let weight = record.row.values.get(1).unwrap_or(&Value::Null);
            if target_id.is_null() || weight.is_null() {
                continue;
            }
            let Some(ordering) = compare_runtime_values(weight, &filter_value)? else {
                continue;
            };
            if ordering != Ordering::Greater {
                continue;
            }
            let row = Row::new(vec![target_id.clone()]);
            result_bytes =
                ensure_result_bytes_fit_and_track_query_row(context, &row, result_bytes)?;
            rows.push(row);
            if rows.len() >= limit {
                break;
            }
        }
        if let (Some(cache_key), Some(generation)) =
            (cache_key, self.storage_dml.cache_generation())
        {
            let mut cache = self
                .graph_edge_filter_limit_rows_cache
                .write()
                .map_err(|error| {
                    DbError::internal(format!("graph edge filter cache poisoned: {error}"))
                })?;
            if cache.len() >= 256 {
                cache.clear();
            }
            cache.insert(cache_key, (generation, rows.clone()));
        }
        Ok(Some(ExecutionResult::Query {
            columns: plan.returns.iter().map(|r| r.field.clone()).collect(),
            rows,
        }))
    }

    /// Fast-path for an unanchored single-hop pattern carrying an inline edge
    /// property equality filter, e.g.
    /// `MATCH (a:L)-[:T {weight: 10}]->(b:L) RETURN b.id LIMIT n`.
    /// The `WHERE r.weight > x` shape is handled by
    /// `try_execute_fast_unanchored_edge_filter_limit`; this covers the inline
    /// `{prop: literal}` equality shape, which otherwise falls back to a full
    /// per-node adjacency traversal that fetches every edge row.
    fn try_execute_fast_unanchored_edge_eq_filter_limit(
        &self,
        plan: &CypherQueryPlan,
        context: &ExecutionContext,
    ) -> DbResult<Option<ExecutionResult>> {
        if plan.pipeline.len() + plan.matches.len() != 1
            || !plan.creates.is_empty()
            || !plan.merges.is_empty()
            || !plan.sets.is_empty()
            || !plan.deletes.is_empty()
            || plan.distinct
            || plan.skip.is_some()
            || plan.union.is_some()
            || !plan.order_by.is_empty()
            || plan.returns.len() != 1
        {
            return Ok(None);
        }
        let Some(limit) = plan
            .limit
            .as_ref()
            .and_then(literal_i64)
            .and_then(|value| usize::try_from(value.max(0)).ok())
        else {
            return Ok(None);
        };
        if limit == 0 {
            return Ok(Some(ExecutionResult::Query {
                columns: plan.returns.iter().map(|r| r.field.clone()).collect(),
                rows: Vec::new(),
            }));
        }

        let match_clause = match plan.pipeline.as_slice() {
            [CypherPipelineOp::Match(match_clause)] => match_clause,
            [] => &plan.matches[0],
            _ => return Ok(None),
        };
        if match_clause.optional
            || match_clause.patterns.len() != 1
            || match_clause.filter.is_some()
        {
            return Ok(None);
        }
        let pattern = &match_clause.patterns[0];
        if pattern.path_function.is_some()
            || pattern.nodes.len() != 2
            || pattern.relationships.len() != 1
        {
            return Ok(None);
        }

        let rel = &pattern.relationships[0];
        // Orient to the physical edge (source -> target); the returned node is
        // the physical target (the other endpoint is unconstrained).
        let (phys_src, phys_tgt) = match rel.direction {
            CypherRelDirection::Outgoing => (&pattern.nodes[0], &pattern.nodes[1]),
            CypherRelDirection::Incoming => (&pattern.nodes[1], &pattern.nodes[0]),
            CypherRelDirection::Both => return Ok(None),
        };
        if rel.table_id.is_none()
            || rel.min_hops.is_some()
            || rel.max_hops.is_some()
            || phys_src.table_id.is_none()
            || phys_tgt.table_id.is_none()
            || !phys_src.properties.is_empty()
            || !phys_tgt.properties.is_empty()
            || rel.properties.len() != 1
        {
            return Ok(None);
        }
        let Some(end_var) = phys_tgt.variable.as_deref() else {
            return Ok(None);
        };
        let expected_return = format!("{end_var}.id");
        if column_ref_name(&plan.returns[0].expr) != Some(expected_return.as_str()) {
            return Ok(None);
        }

        let prop = &rel.properties[0];
        let TypedExprKind::Literal(filter_value) = &prop.value.kind else {
            return Ok(None);
        };
        let filter_value = filter_value.clone();

        let Some(edge_table_id) = rel.table_id else {
            return Ok(None);
        };
        let ((_, tgt_col_idx), _) = self.resolve_edge_endpoint_columns_for_rel(
            context,
            edge_table_id,
            rel.rel_type.as_deref(),
        )?;
        let Some(edge_table) = self
            .catalog_reader
            .get_table_by_id(context.txn_id, edge_table_id)?
        else {
            return Ok(None);
        };
        let Some(prop_col_idx) = self.find_column_index(&edge_table.columns, &prop.key) else {
            return Ok(None);
        };
        let Some(projected_columns) = self.table_column_ids_for_ordinals(
            context,
            edge_table_id,
            &[tgt_col_idx, prop_col_idx],
        )?
        else {
            return Ok(None);
        };

        let mut stream = self.resolve_scan_stream(
            context,
            edge_table_id,
            &ScanAccessPath::SeqScan,
            Some(projected_columns),
        )?;
        let mut rows = Vec::with_capacity(limit.min(1024));
        let mut result_bytes = 0u64;
        while let Some(record) = stream.next()? {
            context.check_deadline()?;
            let target_id = record.row.values.first().unwrap_or(&Value::Null);
            let prop_value = record.row.values.get(1).unwrap_or(&Value::Null);
            if target_id.is_null() {
                continue;
            }
            let Some(ordering) = compare_runtime_values(prop_value, &filter_value)? else {
                continue;
            };
            if ordering != Ordering::Equal {
                continue;
            }
            let row = Row::new(vec![target_id.clone()]);
            result_bytes =
                ensure_result_bytes_fit_and_track_query_row(context, &row, result_bytes)?;
            rows.push(row);
            if rows.len() >= limit {
                break;
            }
        }
        Ok(Some(ExecutionResult::Query {
            columns: plan.returns.iter().map(|r| r.field.clone()).collect(),
            rows,
        }))
    }

    fn try_execute_fast_multi_out_limit(
        &self,
        plan: &CypherQueryPlan,
        context: &ExecutionContext,
    ) -> DbResult<Option<ExecutionResult>> {
        if plan.pipeline.len() + plan.matches.len() != 1
            || !plan.creates.is_empty()
            || !plan.merges.is_empty()
            || !plan.sets.is_empty()
            || !plan.deletes.is_empty()
            || plan.distinct
            || plan.skip.is_some()
            || plan.union.is_some()
            || !plan.order_by.is_empty()
            || plan.returns.len() != 2
        {
            return Ok(None);
        }
        let Some(limit) = plan
            .limit
            .as_ref()
            .and_then(literal_i64)
            .and_then(|value| usize::try_from(value.max(0)).ok())
        else {
            return Ok(None);
        };
        if limit == 0 {
            return Ok(Some(ExecutionResult::Query {
                columns: plan.returns.iter().map(|r| r.field.clone()).collect(),
                rows: Vec::new(),
            }));
        }

        let match_clause = match plan.pipeline.as_slice() {
            [CypherPipelineOp::Match(match_clause)] => match_clause,
            [] => &plan.matches[0],
            _ => return Ok(None),
        };
        if match_clause.optional || match_clause.patterns.len() != 2 {
            return Ok(None);
        }
        let first = &match_clause.patterns[0];
        let second = &match_clause.patterns[1];
        if first.path_function.is_some()
            || second.path_function.is_some()
            || first.nodes.len() != 2
            || second.nodes.len() != 2
            || first.relationships.len() != 1
            || second.relationships.len() != 1
        {
            return Ok(None);
        }

        let first_rel = &first.relationships[0];
        let second_rel = &second.relationships[0];
        // Normalize each pattern to the physical edge (source -> target)
        // following the relationship direction. The binder may reverse a
        // pattern (e.g. anchoring on a filtered/returned node), turning
        // `(a)-[:k]->(b)` into `(b)<-[:k]-(a)`; both orientations describe the
        // same edge so the same scan/group/cartesian plan applies.
        let (first_src, first_tgt) = match first_rel.direction {
            CypherRelDirection::Outgoing => (&first.nodes[0], &first.nodes[1]),
            CypherRelDirection::Incoming => (&first.nodes[1], &first.nodes[0]),
            CypherRelDirection::Both => return Ok(None),
        };
        let (second_src, second_tgt) = match second_rel.direction {
            CypherRelDirection::Outgoing => (&second.nodes[0], &second.nodes[1]),
            CypherRelDirection::Incoming => (&second.nodes[1], &second.nodes[0]),
            CypherRelDirection::Both => return Ok(None),
        };
        let (Some(src_var), Some(first_tgt_var), Some(second_src_var), Some(second_tgt_var)) = (
            first_src.variable.as_deref(),
            first_tgt.variable.as_deref(),
            second_src.variable.as_deref(),
            second_tgt.variable.as_deref(),
        ) else {
            return Ok(None);
        };
        let expected_first_return = format!("{first_tgt_var}.id");
        let expected_second_return = format!("{second_tgt_var}.id");
        if src_var != second_src_var
            || column_ref_name(&plan.returns[0].expr) != Some(expected_first_return.as_str())
            || column_ref_name(&plan.returns[1].expr) != Some(expected_second_return.as_str())
        {
            return Ok(None);
        }
        if second_src
            .table_id
            .is_some_and(|table_id| Some(table_id) != first_src.table_id)
        {
            return Ok(None);
        }
        if first_src.table_id.is_none()
            || first_tgt.table_id.is_none()
            || second_tgt.table_id.is_none()
            || !first_src.properties.is_empty()
            || !first_tgt.properties.is_empty()
            || !second_src.properties.is_empty()
            || !second_tgt.properties.is_empty()
            || first_rel.table_id.is_none()
            || second_rel.table_id.is_none()
            || first_rel.table_id != second_rel.table_id
            || first_rel.variable.is_some()
            || second_rel.variable.is_some()
            || first_rel.min_hops.is_some()
            || first_rel.max_hops.is_some()
            || second_rel.min_hops.is_some()
            || second_rel.max_hops.is_some()
            || !first_rel.properties.is_empty()
            || !second_rel.properties.is_empty()
        {
            return Ok(None);
        }

        let filter_value = match match_clause.filter.as_ref() {
            Some(filter) => {
                let Some(value) =
                    exact_named_column_literal_gt(filter, &format!("{first_tgt_var}.number"))
                else {
                    return Ok(None);
                };
                Some(value)
            }
            None => None,
        };

        let Some(edge_table_id) = first_rel.table_id else {
            return Ok(None);
        };
        let Some(first_target_table_id) = first_tgt.table_id else {
            return Ok(None);
        };
        let ((src_col_idx, _), _) = self.resolve_edge_endpoint_columns_for_rel(
            context,
            edge_table_id,
            first_rel.rel_type.as_deref(),
        )?;
        let Some(projected_columns) =
            self.table_column_ids_for_ordinals(context, edge_table_id, &[src_col_idx])?
        else {
            return Ok(None);
        };

        let allowed_left_target_ids = match filter_value.as_ref() {
            Some(filter_value) => Some(self.fast_graph_collect_target_ids_gt_filter(
                context,
                first_target_table_id,
                "number",
                filter_value,
            )?),
            None => None,
        };

        let mut stream = self.resolve_scan_stream(
            context,
            edge_table_id,
            &ScanAccessPath::SeqScan,
            Some(projected_columns),
        )?;
        let mut seen_sources = HashSet::<ValueHashKey>::new();
        let mut rows = Vec::with_capacity(limit.min(1024));
        let mut result_bytes = 0u64;

        while let Some(record) = stream.next()? {
            context.check_deadline()?;
            let mut source_id = record.row.values.first().cloned().unwrap_or(Value::Null);
            if source_id.is_null() {
                continue;
            }
            normalize_int_key(&mut source_id);
            let source_key = build_hash_key(&source_id)?;
            if !seen_sources.insert(source_key) {
                continue;
            }
            context.track_memory(estimate_value_bytes(&source_id).saturating_add(32))?;

            let neighbor_ids = match self.fast_graph_adjacency_neighbors_cached(
                context,
                edge_table_id,
                &source_id,
                true,
            ) {
                Ok(ids) => ids,
                Err(_) => return Ok(None),
            };
            if neighbor_ids.is_empty() {
                continue;
            }
            for left in neighbor_ids.iter().filter(|id| !id.is_null()) {
                let first_target_allowed =
                    if let Some(Some(allowed_ids)) = allowed_left_target_ids.as_ref() {
                        let mut normalized_target_id = left.clone();
                        normalize_int_key(&mut normalized_target_id);
                        allowed_ids.contains(&build_hash_key(&normalized_target_id)?)
                    } else {
                        true
                    };
                if !first_target_allowed {
                    continue;
                }
                for right in neighbor_ids.iter().filter(|id| !id.is_null()) {
                    let row = Row::new(vec![left.clone(), right.clone()]);
                    result_bytes =
                        ensure_result_bytes_fit_and_track_query_row(context, &row, result_bytes)?;
                    rows.push(row);
                    if rows.len() >= limit {
                        return Ok(Some(ExecutionResult::Query {
                            columns: plan.returns.iter().map(|r| r.field.clone()).collect(),
                            rows,
                        }));
                    }
                }
            }
        }

        Ok(Some(ExecutionResult::Query {
            columns: plan.returns.iter().map(|r| r.field.clone()).collect(),
            rows,
        }))
    }

    fn try_execute_fast_unanchored_one_hop_limit(
        &self,
        plan: &CypherQueryPlan,
        context: &ExecutionContext,
    ) -> DbResult<Option<ExecutionResult>> {
        if plan.pipeline.len() + plan.matches.len() != 1
            || !plan.creates.is_empty()
            || !plan.merges.is_empty()
            || !plan.sets.is_empty()
            || !plan.deletes.is_empty()
            || plan.distinct
            || plan.skip.is_some()
            || plan.union.is_some()
            || !plan.order_by.is_empty()
            || plan.returns.len() != 1
        {
            return Ok(None);
        }
        let Some(limit) = plan
            .limit
            .as_ref()
            .and_then(literal_i64)
            .and_then(|value| usize::try_from(value.max(0)).ok())
        else {
            return Ok(None);
        };
        if limit == 0 {
            return Ok(Some(ExecutionResult::Query {
                columns: plan.returns.iter().map(|r| r.field.clone()).collect(),
                rows: Vec::new(),
            }));
        }

        let match_clause = match plan.pipeline.as_slice() {
            [CypherPipelineOp::Match(match_clause)] => match_clause,
            [] => &plan.matches[0],
            _ => return Ok(None),
        };
        if match_clause.optional || match_clause.patterns.len() != 1 {
            return Ok(None);
        }
        let pattern = &match_clause.patterns[0];
        if pattern.path_function.is_some()
            || pattern.nodes.len() != 2
            || pattern.relationships.len() != 1
        {
            return Ok(None);
        }

        let start = &pattern.nodes[0];
        let end = &pattern.nodes[1];
        let rel = &pattern.relationships[0];
        let Some(end_variable) = end.variable.as_deref() else {
            return Ok(None);
        };
        let Some(return_name) = column_ref_name(&plan.returns[0].expr) else {
            return Ok(None);
        };
        let Some(return_property) = return_name
            .strip_prefix(end_variable)
            .and_then(|tail| tail.strip_prefix('.'))
        else {
            return Ok(None);
        };
        if start.table_id.is_none()
            || end.table_id.is_none()
            || !start.properties.is_empty()
            || !end.properties.is_empty()
            || rel.table_id.is_none()
            || rel.direction != CypherRelDirection::Outgoing
            || rel.variable.is_some()
            || rel.min_hops.is_some()
            || rel.max_hops.is_some()
            || !rel.properties.is_empty()
        {
            return Ok(None);
        }

        let Some(edge_table_id) = rel.table_id else {
            return Ok(None);
        };
        let Some(target_table_id) = end.table_id else {
            return Ok(None);
        };
        let ((_, tgt_col_idx), _) = self.resolve_edge_endpoint_columns_for_rel(
            context,
            edge_table_id,
            rel.rel_type.as_deref(),
        )?;
        let Some(target_table) = self
            .catalog_reader
            .get_table_by_id(context.txn_id, target_table_id)?
        else {
            return Ok(None);
        };
        let Some(return_col_idx) = self.find_column_index(&target_table.columns, return_property)
        else {
            return Ok(None);
        };
        let return_is_target_id = return_col_idx == 0;
        let target_id_index = if return_is_target_id {
            None
        } else {
            match self.find_first_column_btree_index_for_fast_graph(context, target_table_id)? {
                Some(index_id) => Some(index_id),
                None => return Ok(None),
            }
        };
        let Some(projected_columns) =
            self.table_column_ids_for_ordinals(context, edge_table_id, &[tgt_col_idx])?
        else {
            return Ok(None);
        };
        let mut stream = self.resolve_scan_stream(
            context,
            edge_table_id,
            &ScanAccessPath::SeqScan,
            Some(projected_columns),
        )?;
        let mut row_cache = HashMap::new();
        let mut rows = Vec::with_capacity(limit.min(1024));
        let mut result_bytes = 0u64;
        while let Some(record) = stream.next()? {
            context.check_deadline()?;
            let Some(target_id) = record.row.values.first() else {
                continue;
            };
            if target_id.is_null() {
                continue;
            }
            let mut normalized_target_id = target_id.clone();
            normalize_int_key(&mut normalized_target_id);
            let value = if return_is_target_id {
                normalized_target_id
            } else {
                let Some(index_id) = target_id_index else {
                    return Ok(None);
                };
                let Some(target_row) = self.fast_graph_lookup_first_col_row_cached(
                    context,
                    target_table_id,
                    index_id,
                    &normalized_target_id,
                    &mut row_cache,
                )?
                else {
                    continue;
                };
                target_row
                    .values
                    .get(return_col_idx)
                    .cloned()
                    .unwrap_or(Value::Null)
            };
            let row = Row::new(vec![value]);
            result_bytes =
                ensure_result_bytes_fit_and_track_query_row(context, &row, result_bytes)?;
            rows.push(row);
            if rows.len() >= limit {
                break;
            }
        }

        Ok(Some(ExecutionResult::Query {
            columns: plan.returns.iter().map(|r| r.field.clone()).collect(),
            rows,
        }))
    }

    fn try_execute_fast_unanchored_one_hop_count(
        &self,
        plan: &CypherQueryPlan,
        context: &ExecutionContext,
    ) -> DbResult<Option<ExecutionResult>> {
        if plan.pipeline.len() + plan.matches.len() != 1
            || !plan.creates.is_empty()
            || !plan.merges.is_empty()
            || !plan.sets.is_empty()
            || !plan.deletes.is_empty()
            || plan.distinct
            || plan.skip.is_some()
            || plan.limit.is_some()
            || plan.union.is_some()
            || !plan.order_by.is_empty()
            || plan.returns.len() != 1
        {
            return Ok(None);
        }
        let match_clause = match plan.pipeline.as_slice() {
            [CypherPipelineOp::Match(match_clause)] => match_clause,
            [] => &plan.matches[0],
            _ => return Ok(None),
        };
        if match_clause.optional
            || match_clause.filter.is_some()
            || match_clause.patterns.len() != 1
        {
            return Ok(None);
        }
        let pattern = &match_clause.patterns[0];
        if pattern.path_function.is_some()
            || pattern.nodes.len() != 2
            || pattern.relationships.len() != 1
        {
            return Ok(None);
        }
        let start = &pattern.nodes[0];
        let end = &pattern.nodes[1];
        let rel = &pattern.relationships[0];
        let Some(end_variable) = end.variable.as_deref() else {
            return Ok(None);
        };
        if start.table_id.is_none()
            || end.table_id.is_none()
            || !start.properties.is_empty()
            || !end.properties.is_empty()
            || rel.table_id.is_none()
            || rel.direction != CypherRelDirection::Outgoing
            || rel.variable.is_some()
            || rel.min_hops.is_some()
            || rel.max_hops.is_some()
            || !rel.properties.is_empty()
        {
            return Ok(None);
        }
        let TypedExprKind::AggCount {
            expr: Some(expr),
            distinct: false,
            filter: None,
        } = &plan.returns[0].expr.kind
        else {
            return Ok(None);
        };
        if column_ref_name(expr).map_or(true, |name| !name.eq_ignore_ascii_case(end_variable)) {
            return Ok(None);
        }
        let Some(edge_table_id) = rel.table_id else {
            return Ok(None);
        };
        let count =
            self.storage_dml
                .visible_row_count(context.txn_id, &context.snapshot, edge_table_id)?;
        let row = Row::new(vec![Value::BigInt(
            i64::try_from(count).unwrap_or(i64::MAX),
        )]);
        ensure_result_bytes_fit_and_track_query_row(context, &row, 0)?;
        Ok(Some(ExecutionResult::Query {
            columns: plan.returns.iter().map(|r| r.field.clone()).collect(),
            rows: vec![row],
        }))
    }

    fn try_execute_fast_unanchored_one_hop_group_count(
        &self,
        plan: &CypherQueryPlan,
        context: &ExecutionContext,
    ) -> DbResult<Option<ExecutionResult>> {
        if plan.pipeline.len() + plan.matches.len() != 1
            || !plan.creates.is_empty()
            || !plan.merges.is_empty()
            || !plan.sets.is_empty()
            || !plan.deletes.is_empty()
            || plan.distinct
            || plan.skip.is_some()
            || plan.limit.is_some()
            || plan.union.is_some()
            || !plan.order_by.is_empty()
            || plan.returns.len() != 2
        {
            return Ok(None);
        }

        let match_clause = match plan.pipeline.as_slice() {
            [CypherPipelineOp::Match(match_clause)] => match_clause,
            [] => &plan.matches[0],
            _ => return Ok(None),
        };
        if match_clause.optional
            || match_clause.filter.is_some()
            || match_clause.patterns.len() != 1
        {
            return Ok(None);
        }

        let pattern = &match_clause.patterns[0];
        if pattern.path_function.is_some()
            || pattern.nodes.len() != 2
            || pattern.relationships.len() != 1
        {
            return Ok(None);
        }

        let start = &pattern.nodes[0];
        let end = &pattern.nodes[1];
        let rel = &pattern.relationships[0];
        let Some(end_variable) = end.variable.as_deref() else {
            return Ok(None);
        };
        let Some(group_ref) = column_ref_name(&plan.returns[0].expr) else {
            return Ok(None);
        };
        let Some((group_variable, group_property)) = group_ref.split_once('.') else {
            return Ok(None);
        };
        if !group_variable.eq_ignore_ascii_case(end_variable) || group_property.is_empty() {
            return Ok(None);
        }
        let TypedExprKind::AggCount {
            expr: Some(expr),
            distinct: false,
            filter: None,
        } = &plan.returns[1].expr.kind
        else {
            return Ok(None);
        };
        if column_ref_name(expr).map_or(true, |name| !name.eq_ignore_ascii_case(end_variable)) {
            return Ok(None);
        }
        let filter_value = match match_clause.filter.as_ref() {
            Some(filter) => {
                let Some(value) =
                    exact_named_column_literal_gt(filter, &format!("{end_variable}.number"))
                else {
                    return Ok(None);
                };
                Some(value)
            }
            None => None,
        };
        if start.table_id.is_none()
            || end.table_id.is_none()
            || !start.properties.is_empty()
            || !end.properties.is_empty()
            || rel.table_id.is_none()
            || rel.direction != CypherRelDirection::Outgoing
            || rel.variable.is_some()
            || rel.min_hops.is_some()
            || rel.max_hops.is_some()
            || !rel.properties.is_empty()
        {
            return Ok(None);
        }

        let Some(edge_table_id) = rel.table_id else {
            return Ok(None);
        };
        let Some(target_table_id) = end.table_id else {
            return Ok(None);
        };
        let ((_, tgt_col_idx), _) = self.resolve_edge_endpoint_columns_for_rel(
            context,
            edge_table_id,
            rel.rel_type.as_deref(),
        )?;
        let Some(target_table) = self
            .catalog_reader
            .get_table_by_id(context.txn_id, target_table_id)?
        else {
            return Ok(None);
        };
        let Some(group_col_idx) = self.find_column_index(&target_table.columns, group_property)
        else {
            return Ok(None);
        };
        let filter_number_idx = if filter_value.is_some() {
            let Some(number_idx) = self.find_column_index(&target_table.columns, "number") else {
                return Ok(None);
            };
            Some(number_idx)
        } else {
            None
        };
        let Some(target_id_index) =
            self.find_first_column_btree_index_for_fast_graph(context, target_table_id)?
        else {
            return Ok(None);
        };
        let Some(projected_columns) =
            self.table_column_ids_for_ordinals(context, edge_table_id, &[tgt_col_idx])?
        else {
            return Ok(None);
        };

        let mut stream = self.resolve_scan_stream(
            context,
            edge_table_id,
            &ScanAccessPath::SeqScan,
            Some(projected_columns),
        )?;
        let mut target_cache = HashMap::new();
        let mut groups = HashMap::<ValueHashKey, (Value, u64)>::new();
        while let Some(record) = stream.next()? {
            context.check_deadline()?;
            let Some(target_id) = record.row.values.first() else {
                continue;
            };
            if target_id.is_null() {
                continue;
            }
            let mut normalized_target_id = target_id.clone();
            normalize_int_key(&mut normalized_target_id);
            let Some(target_row) = self.fast_graph_lookup_first_col_row_cached(
                context,
                target_table_id,
                target_id_index,
                &normalized_target_id,
                &mut target_cache,
            )?
            else {
                continue;
            };
            if let (Some(filter_value), Some(number_idx)) =
                (filter_value.as_ref(), filter_number_idx)
            {
                let number = target_row.values.get(number_idx).unwrap_or(&Value::Null);
                let Some(ordering) = compare_runtime_values(number, filter_value)? else {
                    continue;
                };
                if ordering != Ordering::Greater {
                    continue;
                }
            }
            let group_value = target_row
                .values
                .get(group_col_idx)
                .cloned()
                .unwrap_or(Value::Null);
            let group_key = build_hash_key(&group_value)?;
            match groups.entry(group_key) {
                std::collections::hash_map::Entry::Occupied(mut entry) => {
                    let count = &mut entry.get_mut().1;
                    *count = (*count).saturating_add(1);
                }
                std::collections::hash_map::Entry::Vacant(entry) => {
                    context.track_memory(estimate_value_bytes(&group_value).saturating_add(64))?;
                    entry.insert((group_value, 1));
                }
            }
        }

        let mut rows = Vec::with_capacity(groups.len());
        let mut result_bytes = 0u64;
        for (group_value, count) in groups.into_values() {
            let row = Row::new(vec![
                group_value,
                Value::BigInt(i64::try_from(count).unwrap_or(i64::MAX)),
            ]);
            result_bytes =
                ensure_result_bytes_fit_and_track_query_row(context, &row, result_bytes)?;
            rows.push(row);
        }

        Ok(Some(ExecutionResult::Query {
            columns: plan.returns.iter().map(|r| r.field.clone()).collect(),
            rows,
        }))
    }

    fn try_execute_fast_unanchored_target_filter_limit(
        &self,
        plan: &CypherQueryPlan,
        context: &ExecutionContext,
    ) -> DbResult<Option<ExecutionResult>> {
        if plan.pipeline.len() + plan.matches.len() != 1
            || !plan.creates.is_empty()
            || !plan.merges.is_empty()
            || !plan.sets.is_empty()
            || !plan.deletes.is_empty()
            || plan.distinct
            || plan.skip.is_some()
            || plan.union.is_some()
            || !plan.order_by.is_empty()
            || plan.returns.len() != 1
        {
            return Ok(None);
        }
        let Some(limit) = plan
            .limit
            .as_ref()
            .and_then(literal_i64)
            .and_then(|value| usize::try_from(value.max(0)).ok())
        else {
            return Ok(None);
        };
        if limit == 0 {
            return Ok(Some(ExecutionResult::Query {
                columns: plan.returns.iter().map(|r| r.field.clone()).collect(),
                rows: Vec::new(),
            }));
        }

        let match_clause = match plan.pipeline.as_slice() {
            [CypherPipelineOp::Match(match_clause)] => match_clause,
            [] => &plan.matches[0],
            _ => return Ok(None),
        };
        if match_clause.optional || match_clause.patterns.len() != 1 {
            return Ok(None);
        }
        let pattern = &match_clause.patterns[0];
        if pattern.path_function.is_some()
            || pattern.nodes.len() != 2
            || pattern.relationships.len() != 1
        {
            return Ok(None);
        }

        let rel = &pattern.relationships[0];
        let (source, target) = match rel.direction {
            CypherRelDirection::Outgoing => (&pattern.nodes[0], &pattern.nodes[1]),
            CypherRelDirection::Incoming => (&pattern.nodes[1], &pattern.nodes[0]),
            CypherRelDirection::Both => return Ok(None),
        };
        let Some(target_variable) = target.variable.as_deref() else {
            return Ok(None);
        };
        let expected_return = format!("{target_variable}.id");
        if column_ref_name(&plan.returns[0].expr) != Some(expected_return.as_str()) {
            return Ok(None);
        }
        if source.table_id.is_none()
            || target.table_id.is_none()
            || !source.properties.is_empty()
            || !target.properties.is_empty()
            || rel.table_id.is_none()
            || rel.variable.is_some()
            || rel.min_hops.is_some()
            || rel.max_hops.is_some()
            || !rel.properties.is_empty()
        {
            return Ok(None);
        }

        let Some(filter_value) = match_clause.filter.as_ref().and_then(|filter| {
            exact_named_column_literal_gt(filter, &format!("{target_variable}.number"))
        }) else {
            return Ok(None);
        };
        let Some(edge_table_id) = rel.table_id else {
            return Ok(None);
        };
        let Some(target_table_id) = target.table_id else {
            return Ok(None);
        };
        let ((_, tgt_col_idx), _) = self.resolve_edge_endpoint_columns_for_rel(
            context,
            edge_table_id,
            rel.rel_type.as_deref(),
        )?;
        let Some(allowed_target_ids) = self.fast_graph_collect_target_ids_gt_filter(
            context,
            target_table_id,
            "number",
            &filter_value,
        )?
        else {
            return Ok(None);
        };
        let Some(projected_columns) =
            self.table_column_ids_for_ordinals(context, edge_table_id, &[tgt_col_idx])?
        else {
            return Ok(None);
        };
        let mut stream = self.resolve_scan_stream(
            context,
            edge_table_id,
            &ScanAccessPath::SeqScan,
            Some(projected_columns),
        )?;
        let mut rows = Vec::with_capacity(limit.min(1024));
        let mut result_bytes = 0u64;
        while let Some(record) = stream.next()? {
            context.check_deadline()?;
            let Some(target_id) = record.row.values.first() else {
                continue;
            };
            if target_id.is_null() {
                continue;
            }
            let mut normalized_target_id = target_id.clone();
            normalize_int_key(&mut normalized_target_id);
            let Ok(target_key) = build_hash_key(&normalized_target_id) else {
                continue;
            };
            if !allowed_target_ids.contains(&target_key) {
                continue;
            }
            let row = Row::new(vec![normalized_target_id]);
            result_bytes =
                ensure_result_bytes_fit_and_track_query_row(context, &row, result_bytes)?;
            rows.push(row);
            if rows.len() >= limit {
                break;
            }
        }
        Ok(Some(ExecutionResult::Query {
            columns: plan.returns.iter().map(|r| r.field.clone()).collect(),
            rows,
        }))
    }

    fn try_execute_fast_unanchored_two_hop_limit(
        &self,
        plan: &CypherQueryPlan,
        context: &ExecutionContext,
    ) -> DbResult<Option<ExecutionResult>> {
        if plan.pipeline.len() + plan.matches.len() != 1
            || !plan.creates.is_empty()
            || !plan.merges.is_empty()
            || !plan.sets.is_empty()
            || !plan.deletes.is_empty()
            || plan.distinct
            || plan.skip.is_some()
            || plan.union.is_some()
            || !plan.order_by.is_empty()
            || plan.returns.len() != 1
        {
            return Ok(None);
        }
        let Some(limit) = plan
            .limit
            .as_ref()
            .and_then(literal_i64)
            .and_then(|value| usize::try_from(value.max(0)).ok())
        else {
            return Ok(None);
        };
        if limit == 0 {
            return Ok(Some(ExecutionResult::Query {
                columns: plan.returns.iter().map(|r| r.field.clone()).collect(),
                rows: Vec::new(),
            }));
        }

        let match_clause = match plan.pipeline.as_slice() {
            [CypherPipelineOp::Match(match_clause)] => match_clause,
            [] => &plan.matches[0],
            _ => return Ok(None),
        };
        if match_clause.optional
            || match_clause.filter.is_some()
            || match_clause.patterns.len() != 1
        {
            return Ok(None);
        }
        let pattern = &match_clause.patterns[0];
        if pattern.path_function.is_some()
            || pattern.nodes.len() != 3
            || pattern.relationships.len() != 2
        {
            return Ok(None);
        }
        let start = &pattern.nodes[0];
        let middle = &pattern.nodes[1];
        let end = &pattern.nodes[2];
        let first_rel = &pattern.relationships[0];
        let second_rel = &pattern.relationships[1];
        let Some(end_variable) = end.variable.as_deref() else {
            return Ok(None);
        };
        let expected_return = format!("{end_variable}.id");
        if column_ref_name(&plan.returns[0].expr) != Some(expected_return.as_str()) {
            return Ok(None);
        }
        if start.table_id.is_none()
            || middle.table_id.is_none()
            || end.table_id.is_none()
            || !start.properties.is_empty()
            || !middle.properties.is_empty()
            || !end.properties.is_empty()
            || first_rel.table_id.is_none()
            || second_rel.table_id.is_none()
            || first_rel.table_id != second_rel.table_id
            || first_rel.direction != CypherRelDirection::Outgoing
            || second_rel.direction != CypherRelDirection::Outgoing
            || first_rel.variable.is_some()
            || second_rel.variable.is_some()
            || first_rel.min_hops.is_some()
            || first_rel.max_hops.is_some()
            || second_rel.min_hops.is_some()
            || second_rel.max_hops.is_some()
            || !first_rel.properties.is_empty()
            || !second_rel.properties.is_empty()
        {
            return Ok(None);
        }

        let Some(edge_table_id) = first_rel.table_id else {
            return Ok(None);
        };
        let ((_, tgt_col_idx), _) = self.resolve_edge_endpoint_columns_for_rel(
            context,
            edge_table_id,
            first_rel.rel_type.as_deref(),
        )?;
        let Some(projected_columns) =
            self.table_column_ids_for_ordinals(context, edge_table_id, &[tgt_col_idx])?
        else {
            return Ok(None);
        };
        let mut stream = self.resolve_scan_stream(
            context,
            edge_table_id,
            &ScanAccessPath::SeqScan,
            Some(projected_columns),
        )?;
        let mut rows = Vec::with_capacity(limit.min(1024));
        let mut result_bytes = 0u64;
        while let Some(record) = stream.next()? {
            context.check_deadline()?;
            let mut middle_id = record.row.values.first().cloned().unwrap_or(Value::Null);
            if middle_id.is_null() {
                continue;
            }
            normalize_int_key(&mut middle_id);
            let mut next_ids = Vec::with_capacity(limit.saturating_sub(rows.len()).min(1024));
            if self
                .fast_graph_push_adjacency_neighbor_ids(
                    context,
                    edge_table_id,
                    &middle_id,
                    true,
                    Some(limit.saturating_sub(rows.len())),
                    &mut next_ids,
                )
                .is_err()
            {
                return Ok(None);
            }
            for next_id in next_ids {
                let row = Row::new(vec![next_id]);
                result_bytes =
                    ensure_result_bytes_fit_and_track_query_row(context, &row, result_bytes)?;
                rows.push(row);
                if rows.len() >= limit {
                    return Ok(Some(ExecutionResult::Query {
                        columns: plan.returns.iter().map(|r| r.field.clone()).collect(),
                        rows,
                    }));
                }
            }
        }
        Ok(Some(ExecutionResult::Query {
            columns: plan.returns.iter().map(|r| r.field.clone()).collect(),
            rows,
        }))
    }

    fn try_execute_fast_hybrid_graph_vector_rel(
        &self,
        plan: &CypherQueryPlan,
        context: &ExecutionContext,
    ) -> DbResult<Option<ExecutionResult>> {
        if plan.pipeline.len() + plan.matches.len() != 1
            || !plan.creates.is_empty()
            || !plan.merges.is_empty()
            || !plan.sets.is_empty()
            || !plan.deletes.is_empty()
            || plan.distinct
            || plan.skip.is_some()
            || plan.union.is_some()
            || plan.returns.len() != 3
        {
            return Ok(None);
        }

        let match_clause = match plan.pipeline.as_slice() {
            [CypherPipelineOp::Match(match_clause)] => match_clause,
            [] => &plan.matches[0],
            _ => return Ok(None),
        };
        if match_clause.optional || match_clause.patterns.len() != 1 {
            return Ok(None);
        }
        let pattern = &match_clause.patterns[0];
        if pattern.path_function.is_some()
            || pattern.nodes.len() != 3
            || pattern.relationships.len() != 2
        {
            return Ok(None);
        }

        let start = &pattern.nodes[0];
        let source = &pattern.nodes[1];
        let target = &pattern.nodes[2];
        let wrote_rel = &pattern.relationships[0];
        let cites_rel = &pattern.relationships[1];
        let Some(start_variable) = start.variable.as_deref() else {
            return Ok(None);
        };
        let Some(source_variable) = source.variable.as_deref() else {
            return Ok(None);
        };
        let Some(target_variable) = target.variable.as_deref() else {
            return Ok(None);
        };
        let (Some(start_table_id), Some(source_table_id), Some(target_table_id)) =
            (start.table_id, source.table_id, target.table_id)
        else {
            return Ok(None);
        };
        let (Some(wrote_table_id), Some(cites_table_id)) = (wrote_rel.table_id, cites_rel.table_id)
        else {
            return Ok(None);
        };
        if !start.properties.is_empty()
            || !source.properties.is_empty()
            || !target.properties.is_empty()
            || wrote_rel.direction != CypherRelDirection::Outgoing
            || cites_rel.direction != CypherRelDirection::Outgoing
            || wrote_rel.variable.is_some()
            || cites_rel.variable.is_some()
            || wrote_rel.min_hops.is_some()
            || wrote_rel.max_hops.is_some()
            || cites_rel.min_hops.is_some()
            || cites_rel.max_hops.is_some()
            || !wrote_rel.properties.is_empty()
            || !cites_rel.properties.is_empty()
        {
            return Ok(None);
        }

        let expected_returns = [
            format!("{start_variable}.name"),
            format!("{source_variable}.title"),
            format!("{target_variable}.title"),
        ];
        if plan
            .returns
            .iter()
            .zip(expected_returns.iter())
            .any(|(projection, expected)| column_ref_name(&projection.expr) != Some(expected))
        {
            return Ok(None);
        }
        let expected_order = format!("{start_variable}.name");
        if plan
            .order_by
            .iter()
            .any(|sort| column_ref_name(&sort.expr) != Some(expected_order.as_str()))
        {
            return Ok(None);
        }

        let Some(filter) = match_clause.filter.as_ref() else {
            return Ok(None);
        };
        let Some(hybrid_filter) =
            extract_hybrid_graph_vector_filter(filter, start_variable, target_variable)
        else {
            return Ok(None);
        };

        let Some(start_columns) = self
            .catalog_reader
            .get_table_by_id(context.txn_id, start_table_id)?
            .map(|table| table.columns)
        else {
            return Ok(None);
        };
        let Some(source_columns) = self
            .catalog_reader
            .get_table_by_id(context.txn_id, source_table_id)?
            .map(|table| table.columns)
        else {
            return Ok(None);
        };
        let Some(target_columns) = self
            .catalog_reader
            .get_table_by_id(context.txn_id, target_table_id)?
            .map(|table| table.columns)
        else {
            return Ok(None);
        };

        let start_id_idx = self.find_column_index(&start_columns, "id").unwrap_or(0);
        let Some(start_name_idx) = self.find_column_index(&start_columns, "name") else {
            return Ok(None);
        };
        let Some(start_tenant_idx) = self.find_column_index(&start_columns, "tenant_id") else {
            return Ok(None);
        };
        let source_id_idx = self.find_column_index(&source_columns, "id").unwrap_or(0);
        let Some(source_title_idx) = self.find_column_index(&source_columns, "title") else {
            return Ok(None);
        };
        let Some(target_title_idx) = self.find_column_index(&target_columns, "title") else {
            return Ok(None);
        };
        let Some(target_tenant_idx) = self.find_column_index(&target_columns, "tenant_id") else {
            return Ok(None);
        };
        let Some(target_embedding_idx) = self.find_column_index(&target_columns, "embedding")
        else {
            return Ok(None);
        };
        let start_tenant_index = self.find_named_column_btree_index_for_fast_graph(
            context,
            start_table_id,
            &start_columns,
            "tenant_id",
        )?;

        let source_id_index =
            self.find_first_column_btree_index_for_fast_graph(context, source_table_id)?;
        let target_id_index =
            self.find_first_column_btree_index_for_fast_graph(context, target_table_id)?;
        let Some(source_id_index) = source_id_index else {
            return Ok(None);
        };
        let Some(target_id_index) = target_id_index else {
            return Ok(None);
        };

        let mut source_cache: HashMap<ValueHashKey, Option<Row>> = HashMap::new();
        let mut target_cache: HashMap<ValueHashKey, Option<Row>> = HashMap::new();
        let mut rows = Vec::new();
        let distance_threshold_squared =
            hybrid_filter.distance_threshold * hybrid_filter.distance_threshold;

        let mut users = if let Some(start_tenant_index) = start_tenant_index {
            self.scan_index_locked(
                context,
                start_table_id,
                start_tenant_index,
                KeyRange::point(vec![hybrid_filter.start_tenant.clone()]),
                None,
            )?
        } else {
            self.scan_table_locked(context, start_table_id, None)?
        };
        while let Some(record) = users.next()? {
            context.check_deadline()?;
            let Some(start_tenant) = record.row.values.get(start_tenant_idx) else {
                continue;
            };
            if compare_runtime_values(start_tenant, &hybrid_filter.start_tenant)?
                != Some(std::cmp::Ordering::Equal)
            {
                continue;
            }
            let Some(user_id) = record.row.values.get(start_id_idx).cloned() else {
                continue;
            };
            let user_name = record
                .row
                .values
                .get(start_name_idx)
                .cloned()
                .unwrap_or(Value::Null);

            let source_ids = match self.fast_graph_adjacency_neighbors_cached(
                context,
                wrote_table_id,
                &user_id,
                true,
            ) {
                Ok(ids) => ids,
                Err(_) => return Ok(None),
            };
            for mut source_id in source_ids {
                context.check_deadline()?;
                if source_id.is_null() {
                    continue;
                }
                normalize_int_key(&mut source_id);
                let Some(source_row) = self.fast_graph_lookup_first_col_row_cached(
                    context,
                    source_table_id,
                    source_id_index,
                    &source_id,
                    &mut source_cache,
                )?
                else {
                    continue;
                };
                let source_title = source_row
                    .values
                    .get(source_title_idx)
                    .cloned()
                    .unwrap_or(Value::Null);
                let Some(source_node_id) = source_row.values.get(source_id_idx) else {
                    continue;
                };

                let target_ids = match self.fast_graph_adjacency_neighbors_cached(
                    context,
                    cites_table_id,
                    source_node_id,
                    true,
                ) {
                    Ok(ids) => ids,
                    Err(_) => return Ok(None),
                };
                for mut target_id in target_ids {
                    if target_id.is_null() {
                        continue;
                    }
                    normalize_int_key(&mut target_id);
                    let Some(target_row) = self.fast_graph_lookup_first_col_row_cached(
                        context,
                        target_table_id,
                        target_id_index,
                        &target_id,
                        &mut target_cache,
                    )?
                    else {
                        continue;
                    };
                    let Some(target_tenant) = target_row.values.get(target_tenant_idx) else {
                        continue;
                    };
                    if compare_runtime_values(target_tenant, &hybrid_filter.target_tenant)?
                        != Some(std::cmp::Ordering::Equal)
                    {
                        continue;
                    }
                    let Some(Value::Vector(embedding)) =
                        target_row.values.get(target_embedding_idx)
                    else {
                        continue;
                    };
                    if embedding.values.len() != hybrid_filter.query_vector.len() {
                        continue;
                    }
                    // SIMD-dispatched squared L2 with f64 accumulation —
                    // see the deep variant below for context.
                    let distance_squared = aiondb_vector::simd::dispatch::l2_squared_f64(
                        &embedding.values,
                        &hybrid_filter.query_vector,
                    );
                    if distance_squared >= distance_threshold_squared {
                        continue;
                    }

                    let target_title = target_row
                        .values
                        .get(target_title_idx)
                        .cloned()
                        .unwrap_or(Value::Null);
                    rows.push(Row::new(vec![
                        user_name.clone(),
                        source_title.clone(),
                        target_title,
                    ]));
                }
            }
        }

        if !plan.order_by.is_empty() {
            rows.sort_by(|left, right| {
                compare_sort_values(
                    left.values.first().unwrap_or(&Value::Null),
                    right.values.first().unwrap_or(&Value::Null),
                    plan.order_by[0].descending,
                    plan.order_by[0].nulls_first,
                )
                .unwrap_or(std::cmp::Ordering::Equal)
            });
        }
        let limit = plan
            .limit
            .as_ref()
            .and_then(literal_i64)
            .and_then(|value| usize::try_from(value.max(0)).ok());
        if let Some(limit) = limit {
            rows.truncate(limit);
        }

        let columns = plan.returns.iter().map(|r| r.field.clone()).collect();
        Ok(Some(ExecutionResult::Query { columns, rows }))
    }

    fn try_execute_fast_hybrid_deep_graph_vector_rel(
        &self,
        plan: &CypherQueryPlan,
        context: &ExecutionContext,
    ) -> DbResult<Option<ExecutionResult>> {
        if plan.pipeline.len() + plan.matches.len() != 1
            || !plan.creates.is_empty()
            || !plan.merges.is_empty()
            || !plan.sets.is_empty()
            || !plan.deletes.is_empty()
            || plan.distinct
            || plan.skip.is_some()
            || plan.union.is_some()
            || plan.returns.len() != 5
        {
            return Ok(None);
        }

        let match_clause = match plan.pipeline.as_slice() {
            [CypherPipelineOp::Match(match_clause)] => match_clause,
            [] => &plan.matches[0],
            _ => return Ok(None),
        };
        if match_clause.optional || match_clause.patterns.len() != 1 {
            return Ok(None);
        }
        let pattern = &match_clause.patterns[0];
        if pattern.path_function.is_some()
            || pattern.nodes.len() != 4
            || pattern.relationships.len() != 3
        {
            return Ok(None);
        }

        let start = &pattern.nodes[0];
        let friend = &pattern.nodes[1];
        let source = &pattern.nodes[2];
        let target = &pattern.nodes[3];
        let follows_rel = &pattern.relationships[0];
        let wrote_rel = &pattern.relationships[1];
        let cites_rel = &pattern.relationships[2];
        let Some(start_variable) = start.variable.as_deref() else {
            return Ok(None);
        };
        let Some(friend_variable) = friend.variable.as_deref() else {
            return Ok(None);
        };
        let Some(source_variable) = source.variable.as_deref() else {
            return Ok(None);
        };
        let Some(target_variable) = target.variable.as_deref() else {
            return Ok(None);
        };
        let (
            Some(start_table_id),
            Some(friend_table_id),
            Some(source_table_id),
            Some(target_table_id),
        ) = (
            start.table_id,
            friend.table_id,
            source.table_id,
            target.table_id,
        )
        else {
            return Ok(None);
        };
        let (Some(follows_table_id), Some(wrote_table_id), Some(cites_table_id)) =
            (follows_rel.table_id, wrote_rel.table_id, cites_rel.table_id)
        else {
            return Ok(None);
        };
        if !start.properties.is_empty()
            || !friend.properties.is_empty()
            || !source.properties.is_empty()
            || !target.properties.is_empty()
            || [follows_rel, wrote_rel, cites_rel].iter().any(|rel| {
                rel.direction != CypherRelDirection::Outgoing
                    || rel.variable.is_some()
                    || rel.min_hops.is_some()
                    || rel.max_hops.is_some()
                    || !rel.properties.is_empty()
            })
        {
            return Ok(None);
        }

        let expected_returns = [
            format!("{friend_variable}.id"),
            format!("{source_variable}.title"),
            format!("{target_variable}.title"),
            format!("{target_variable}.popularity"),
        ];
        if plan
            .returns
            .iter()
            .take(4)
            .zip(expected_returns.iter())
            .any(|(projection, expected)| column_ref_name(&projection.expr) != Some(expected))
            || !is_l2_distance_expr_for_variable(&plan.returns[4].expr, target_variable)
        {
            return Ok(None);
        }
        if plan.order_by.len() != 2
            || !is_l2_distance_expr_or_alias(&plan.order_by[0].expr, target_variable, "dist")
            || plan.order_by[0].descending
            || column_ref_name(&plan.order_by[1].expr)
                != Some(format!("{target_variable}.popularity").as_str())
            || !plan.order_by[1].descending
        {
            return Ok(None);
        }

        let Some(filter) = match_clause.filter.as_ref() else {
            return Ok(None);
        };
        let Some(hybrid_filter) = extract_hybrid_deep_graph_vector_filter(
            filter,
            start_variable,
            friend_variable,
            target_variable,
        ) else {
            return Ok(None);
        };
        let mut start_id = hybrid_filter.start_id.clone();
        normalize_int_key(&mut start_id);
        let limit = plan
            .limit
            .as_ref()
            .and_then(literal_i64)
            .and_then(|value| usize::try_from(value.max(0)).ok());

        let Some(meta) = self.hybrid_deep_graph_vector_meta_cached(
            context,
            start_table_id,
            friend_table_id,
            source_table_id,
            target_table_id,
        )?
        else {
            return Ok(None);
        };
        let HybridDeepGraphVectorMeta {
            start_id_idx,
            start_tenant_idx,
            friend_id_idx,
            friend_tenant_idx,
            source_id_idx,
            source_title_idx,
            target_title_idx,
            target_tenant_idx,
            target_popularity_idx,
            target_embedding_idx,
            start_id_index,
            friend_id_index,
            source_id_index,
            target_id_index,
        } = meta;

        let mut person_cache: HashMap<ValueHashKey, Option<Row>> = HashMap::new();
        let mut source_cache: HashMap<ValueHashKey, Option<Row>> = HashMap::new();
        let mut target_cache: HashMap<ValueHashKey, Option<Row>> = HashMap::new();
        let Some(start_row) = self.fast_graph_lookup_first_col_row_cached(
            context,
            start_table_id,
            start_id_index,
            &start_id,
            &mut person_cache,
        )?
        else {
            let columns = plan.returns.iter().map(|r| r.field.clone()).collect();
            return Ok(Some(ExecutionResult::Query {
                columns,
                rows: Vec::new(),
            }));
        };
        let Some(start_tenant) = start_row.values.get(start_tenant_idx).cloned() else {
            return Ok(None);
        };
        let Some(start_node_id) = start_row.values.get(start_id_idx).cloned() else {
            return Ok(None);
        };

        let distance_threshold_squared =
            hybrid_filter.distance_threshold * hybrid_filter.distance_threshold;
        let mut rows = Vec::new();
        let friend_ids = match self.fast_graph_adjacency_neighbors_cached(
            context,
            follows_table_id,
            &start_node_id,
            true,
        ) {
            Ok(ids) => ids,
            Err(_) => return Ok(None),
        };

        for mut friend_id in friend_ids {
            context.check_deadline()?;
            if friend_id.is_null() {
                continue;
            }
            normalize_int_key(&mut friend_id);
            let Some(friend_row) = self.fast_graph_lookup_first_col_row_cached(
                context,
                friend_table_id,
                friend_id_index,
                &friend_id,
                &mut person_cache,
            )?
            else {
                continue;
            };
            let Some(friend_tenant) = friend_row.values.get(friend_tenant_idx) else {
                continue;
            };
            if compare_runtime_values(friend_tenant, &start_tenant)? != Some(Ordering::Equal) {
                continue;
            }
            let friend_return_id = friend_row
                .values
                .get(friend_id_idx)
                .cloned()
                .unwrap_or(friend_id.clone());
            let source_ids = match self.fast_graph_adjacency_neighbors_cached(
                context,
                wrote_table_id,
                &friend_return_id,
                true,
            ) {
                Ok(ids) => ids,
                Err(_) => return Ok(None),
            };

            for mut source_id in source_ids {
                context.check_deadline()?;
                if source_id.is_null() {
                    continue;
                }
                normalize_int_key(&mut source_id);
                let Some(source_row) = self.fast_graph_lookup_first_col_row_cached(
                    context,
                    source_table_id,
                    source_id_index,
                    &source_id,
                    &mut source_cache,
                )?
                else {
                    continue;
                };
                let source_title = source_row
                    .values
                    .get(source_title_idx)
                    .cloned()
                    .unwrap_or(Value::Null);
                let Some(source_node_id) = source_row.values.get(source_id_idx) else {
                    continue;
                };
                let target_ids = match self.fast_graph_adjacency_neighbors_cached(
                    context,
                    cites_table_id,
                    source_node_id,
                    true,
                ) {
                    Ok(ids) => ids,
                    Err(_) => return Ok(None),
                };

                for mut target_id in target_ids {
                    if target_id.is_null() {
                        continue;
                    }
                    normalize_int_key(&mut target_id);
                    let Some(target_row) = self.fast_graph_lookup_first_col_row_cached(
                        context,
                        target_table_id,
                        target_id_index,
                        &target_id,
                        &mut target_cache,
                    )?
                    else {
                        continue;
                    };
                    let Some(target_tenant) = target_row.values.get(target_tenant_idx) else {
                        continue;
                    };
                    if compare_runtime_values(target_tenant, &start_tenant)?
                        != Some(Ordering::Equal)
                    {
                        continue;
                    }
                    let target_popularity = target_row
                        .values
                        .get(target_popularity_idx)
                        .cloned()
                        .unwrap_or(Value::Null);
                    if compare_runtime_values(
                        &target_popularity,
                        &hybrid_filter.popularity_threshold,
                    )? != Some(Ordering::Greater)
                    {
                        continue;
                    }
                    let Some(Value::Vector(embedding)) =
                        target_row.values.get(target_embedding_idx)
                    else {
                        continue;
                    };
                    if embedding.values.len() != hybrid_filter.query_vector.len() {
                        continue;
                    }
                    // SIMD-dispatched (AVX2 / NEON / scalar) squared L2 with
                    // f64 accumulation. Replaces a scalar `iter.zip.map.sum`
                    // loop that was the dominant per-target cost on hot
                    // deep-graph + vector hybrid queries.
                    let distance_squared = aiondb_vector::simd::dispatch::l2_squared_f64(
                        &embedding.values,
                        &hybrid_filter.query_vector,
                    );
                    if distance_squared >= distance_threshold_squared {
                        continue;
                    }

                    let target_title = target_row
                        .values
                        .get(target_title_idx)
                        .cloned()
                        .unwrap_or(Value::Null);
                    rows.push(Row::new(vec![
                        friend_return_id.clone(),
                        source_title.clone(),
                        target_title,
                        target_popularity,
                        Value::Double(distance_squared.sqrt()),
                    ]));
                }
            }
        }

        rows.sort_by(|left, right| {
            compare_sort_values(
                left.values.get(4).unwrap_or(&Value::Null),
                right.values.get(4).unwrap_or(&Value::Null),
                false,
                plan.order_by[0].nulls_first,
            )
            .unwrap_or(Ordering::Equal)
            .then_with(|| {
                compare_sort_values(
                    left.values.get(3).unwrap_or(&Value::Null),
                    right.values.get(3).unwrap_or(&Value::Null),
                    true,
                    plan.order_by[1].nulls_first,
                )
                .unwrap_or(Ordering::Equal)
            })
        });
        if let Some(limit) = limit {
            rows.truncate(limit);
        }

        let columns = plan.returns.iter().map(|r| r.field.clone()).collect();
        Ok(Some(ExecutionResult::Query { columns, rows }))
    }

    fn find_first_column_btree_index_for_fast_graph(
        &self,
        context: &ExecutionContext,
        table_id: RelationId,
    ) -> DbResult<Option<IndexId>> {
        let Some(table) = self
            .catalog_reader
            .get_table_by_id(context.txn_id, table_id)?
        else {
            return Ok(None);
        };
        let Some(id_column) = table.columns.first() else {
            return Ok(None);
        };
        for index in self.catalog_reader.list_indexes(context.txn_id, table_id)? {
            if index.kind == aiondb_catalog::IndexKind::BTree
                && index
                    .key_columns
                    .first()
                    .is_some_and(|key| key.column_id == id_column.column_id)
            {
                return Ok(Some(index.index_id));
            }
        }
        Ok(None)
    }

    fn find_named_column_btree_index_for_fast_graph(
        &self,
        context: &ExecutionContext,
        table_id: RelationId,
        columns: &[ColumnDescriptor],
        column_name: &str,
    ) -> DbResult<Option<IndexId>> {
        let Some(column) = columns
            .iter()
            .find(|column| column.name.eq_ignore_ascii_case(column_name))
        else {
            return Ok(None);
        };
        for index in self.catalog_reader.list_indexes(context.txn_id, table_id)? {
            if index.kind == aiondb_catalog::IndexKind::BTree
                && index
                    .key_columns
                    .first()
                    .is_some_and(|key| key.column_id == column.column_id)
            {
                return Ok(Some(index.index_id));
            }
        }
        Ok(None)
    }

    fn fast_graph_lookup_first_col_row_cached(
        &self,
        context: &ExecutionContext,
        table_id: RelationId,
        index_id: IndexId,
        value: &Value,
        cache: &mut HashMap<ValueHashKey, Option<Row>>,
    ) -> DbResult<Option<Row>> {
        let key = build_hash_key(value)?;
        if let Some(row) = cache.get(&key) {
            return Ok(row.clone());
        }
        let cache_key = self
            .storage_dml
            .cache_generation()
            .map(|_| GraphFirstColRowCacheKey {
                table_id,
                index_id,
                value_key: key.clone(),
            });
        if let Some(cache_key) = &cache_key {
            if let Some((cached_generation, row)) = self
                .graph_first_col_row_cache
                .read()
                .map_err(|error| {
                    DbError::internal(format!("graph row lookup cache poisoned: {error}"))
                })?
                .get(cache_key)
                .cloned()
            {
                if self.storage_dml.cache_generation() == Some(cached_generation) {
                    cache.insert(key, row.clone());
                    return Ok(row);
                }
            }
        }
        let key_range = KeyRange {
            lower: aiondb_storage_api::Bound::Included(vec![value.clone()]),
            upper: aiondb_storage_api::Bound::Included(vec![value.clone()]),
        };
        let mut stream = self.scan_index_locked(context, table_id, index_id, key_range, None)?;
        let row = stream.next()?.map(|record| record.row);
        cache.insert(key, row.clone());
        if let Some(cache_key) = cache_key {
            if let Some(generation) = self.storage_dml.cache_generation() {
                let mut global_cache = self.graph_first_col_row_cache.write().map_err(|error| {
                    DbError::internal(format!("graph row lookup cache poisoned: {error}"))
                })?;
                if global_cache.len() >= 8192 {
                    global_cache.clear();
                }
                global_cache.insert(cache_key, (generation, row.clone()));
            }
        }
        Ok(row)
    }

    // -----------------------------------------------------------------------
    // UNWIND
    // -----------------------------------------------------------------------

    /// Execute a FOREACH clause: for every input binding, evaluate the list
    /// expression, then run the body update clauses once per element with
    /// `variable` bound to that element. FOREACH performs side effects only;
    /// it never changes the outer binding cardinality, so the input bindings
    /// are returned unchanged.
    fn execute_cypher_foreach(
        &self,
        context: &ExecutionContext,
        foreach: &CypherForeachPlan,
        mut bindings: Vec<BindingRow>,
    ) -> DbResult<Vec<BindingRow>> {
        for binding in &mut bindings {
            context.check_deadline()?;
            let list_value =
                self.evaluate_cypher_expr_with_binding(&foreach.expr, &*binding, context)?;
            let elements = match list_value {
                Value::Array(elements) => elements,
                Value::Null => continue,
                other => vec![other],
            };
            for elem in elements {
                context.check_deadline()?;
                binding.insert_binding(foreach.variable.clone(), BoundValue::Scalar(elem));
                self.execute_cypher_foreach_body(context, &foreach.body, &mut *binding)?;
            }
            // FOREACH does not leak its loop variable to later clauses.
            binding.remove(&foreach.variable);
        }
        Ok(bindings)
    }

    /// Run the FOREACH body update clauses against a single binding row.
    ///
    /// SET mutates the row in place so a later RETURN observes the change.
    /// CREATE / MERGE only need their storage side effects here, so they run
    /// against a throwaway copy of the row; FOREACH never changes the outer
    /// binding cardinality.
    fn execute_cypher_foreach_body(
        &self,
        context: &ExecutionContext,
        body: &[CypherForeachOp],
        binding: &mut BindingRow,
    ) -> DbResult<()> {
        for op in body {
            context.check_deadline()?;
            match op {
                CypherForeachOp::Set(set_item) => {
                    self.execute_cypher_set(context, set_item, std::slice::from_mut(binding))?;
                }
                CypherForeachOp::Create(create_clause) => {
                    self.execute_cypher_create(context, create_clause, vec![binding.clone()])?;
                }
                CypherForeachOp::Merge(merge_clause) => {
                    self.execute_cypher_merge(context, merge_clause, vec![binding.clone()])?;
                }
                CypherForeachOp::Delete(delete_clause) => {
                    self.execute_cypher_delete(
                        context,
                        delete_clause,
                        std::slice::from_ref(binding),
                    )?;
                }
                CypherForeachOp::Foreach(nested) => {
                    let taken = std::mem::replace(binding, BindingRow::new());
                    let mut rows = self.execute_cypher_foreach(context, nested, vec![taken])?;
                    *binding = rows.pop().unwrap_or_else(BindingRow::new);
                }
            }
        }
        Ok(())
    }

    /// Execute an UNWIND clause: evaluate the list expression and expand each
    /// element into its own binding row with the given variable name.
    fn execute_cypher_unwind(
        &self,
        context: &ExecutionContext,
        unwind: &aiondb_plan::graph::CypherUnwindClause,
        input_bindings: Vec<BindingRow>,
    ) -> DbResult<Vec<BindingRow>> {
        let mut result = Vec::new();
        for binding in &input_bindings {
            context.check_deadline()?;
            let list_value =
                self.evaluate_cypher_expr_with_binding(&unwind.expr, binding, context)?;
            match list_value {
                Value::Array(elements) => {
                    for elem in elements {
                        let mut new_binding = binding.clone();
                        new_binding
                            .insert_binding(unwind.variable.clone(), BoundValue::Scalar(elem));
                        push_graph_binding(context, &mut result, new_binding)?;
                    }
                }
                Value::Null => {
                    // UNWIND null produces no rows
                }
                other => {
                    // UNWIND on a single value treats it as a one-element list
                    let mut new_binding = binding.clone();
                    new_binding.insert_binding(unwind.variable.clone(), BoundValue::Scalar(other));
                    push_graph_binding(context, &mut result, new_binding)?;
                }
            }
        }
        Ok(result)
    }

    // -----------------------------------------------------------------------
    // WITH
    // -----------------------------------------------------------------------

    /// Execute a WITH clause: evaluate expressions and project into new bindings,
    /// then apply ORDER BY, SKIP, and LIMIT.
    ///
    /// When a WITH item is a simple variable reference that is already bound as a
    /// Node or Edge, the binding is preserved (not flattened to a scalar) so that
    /// downstream clauses can still access properties.
    fn execute_cypher_with(
        &self,
        context: &ExecutionContext,
        with: &aiondb_plan::graph::CypherWithClause,
        input_bindings: Vec<BindingRow>,
    ) -> DbResult<Vec<BindingRow>> {
        let mut result = Vec::new();
        for binding in &input_bindings {
            context.check_deadline()?;
            let mut new_binding = BindingRow::new();
            for (index, item) in with.items.iter().enumerate() {
                let alias = &item.field.name;
                // Prefer the explicit planner metadata for plain variable
                // passthroughs like `WITH n AS m`. Fall back to the older
                // ColumnRef-based inference for already-constructed plans.
                let preserved = with
                    .preserve_binding_sources
                    .get(index)
                    .and_then(|source| source.as_deref())
                    .and_then(|source| binding.get_shared(source))
                    .or_else(|| {
                        if let aiondb_plan::TypedExprKind::ColumnRef { name, .. } = &item.expr.kind
                        {
                            let var_name = name.split('\0').next().unwrap_or(name.as_str());
                            if name.contains('\0') {
                                None
                            } else {
                                binding.get_shared(var_name)
                            }
                        } else {
                            None
                        }
                    });

                if let Some(bound) = preserved {
                    new_binding.insert_shared_binding(alias.clone(), bound);
                } else {
                    let value =
                        self.evaluate_cypher_expr_with_binding(&item.expr, binding, context)?;
                    new_binding.insert_binding(alias.clone(), BoundValue::Scalar(value));
                }
            }
            push_graph_binding(context, &mut result, new_binding)?;
        }

        if with.distinct {
            let mut seen = HashSet::<Vec<ValueHashKey>>::new();
            let mut deduped = Vec::with_capacity(result.len());
            for binding in result.drain(..) {
                context.check_deadline()?;
                let key = self
                    .build_flat_row(&binding)
                    .values
                    .iter()
                    .map(build_hash_key)
                    .collect::<DbResult<Vec<_>>>()?;
                if seen.insert(key) {
                    ensure_graph_result_row_capacity(context, deduped.len())?;
                    deduped.push(binding);
                }
            }
            result = deduped;
        }

        if let Some(filter_expr) = with.filter.as_ref() {
            let mut filtered = Vec::with_capacity(result.len());
            for binding in result.drain(..) {
                context.check_deadline()?;
                if self.evaluate_graph_predicate(context, filter_expr, &binding)? {
                    ensure_graph_result_row_capacity(context, filtered.len())?;
                    filtered.push(binding);
                }
            }
            result = filtered;
        }

        // Apply ORDER BY on bindings.
        if !with.order_by.is_empty() {
            let order_by = &with.order_by;
            let mut keyed: Vec<(Vec<Value>, BindingRow)> = Vec::with_capacity(result.len());
            for binding in result.drain(..) {
                context.check_deadline()?;
                let mut keys = Vec::with_capacity(order_by.len());
                for ob in order_by {
                    let key =
                        self.evaluate_cypher_expr_with_binding(&ob.expr, &binding, context)?;
                    context.track_memory(estimate_value_bytes(&key).saturating_add(32))?;
                    keys.push(key);
                }
                keyed.push((keys, binding));
            }
            let failed = std::cell::Cell::new(false);
            let error: std::cell::RefCell<Option<DbError>> = std::cell::RefCell::new(None);
            keyed.sort_by(|(a_keys, _), (b_keys, _)| {
                if failed.get() {
                    return Ordering::Equal;
                }
                if let Err(e) = context.check_deadline() {
                    failed.set(true);
                    *error.borrow_mut() = Some(e);
                    return Ordering::Equal;
                }
                for (i, (a, b)) in a_keys.iter().zip(b_keys.iter()).enumerate() {
                    let descending = order_by.get(i).is_some_and(|o| o.descending);
                    let nulls_first = order_by.get(i).and_then(|o| o.nulls_first);
                    let cmp = match compare_sort_values(a, b, descending, nulls_first) {
                        Ok(cmp) => cmp,
                        Err(e) => {
                            failed.set(true);
                            *error.borrow_mut() = Some(e);
                            return Ordering::Equal;
                        }
                    };
                    if cmp != Ordering::Equal {
                        return cmp;
                    }
                }
                Ordering::Equal
            });
            if let Some(e) = error.into_inner() {
                return Err(e);
            }
            result = Vec::with_capacity(keyed.len());
            for (_, binding) in keyed {
                ensure_graph_result_row_capacity(context, result.len())?;
                result.push(binding);
            }
        }

        // Apply SKIP on bindings. Cypher requires non-negative integer
        // arguments - float or negative values raise SyntaxError.
        if let Some(ref skip_expr) = with.skip {
            let skip_val = self.evaluate_expr(skip_expr, context)?;
            let n = match skip_val {
                Value::BigInt(n) if n >= 0 => nonneg_i64_to_usize(n),
                Value::Int(n) if n >= 0 => nonneg_i64_to_usize(i64::from(n)),
                Value::BigInt(_) | Value::Int(_) => {
                    return Err(DbError::syntax_error(
                        "SKIP requires a non-negative integer value",
                    ));
                }
                Value::Real(_) | Value::Double(_) | Value::Numeric(_) => {
                    return Err(DbError::syntax_error("SKIP requires an integer value"));
                }
                _ => 0,
            };
            result = result.into_iter().skip(n).collect();
        }

        // Apply LIMIT on bindings (same Cypher integer guard as SKIP).
        if let Some(ref limit_expr) = with.limit {
            let limit_val = self.evaluate_expr(limit_expr, context)?;
            let n = match limit_val {
                Value::BigInt(n) if n >= 0 => nonneg_i64_to_usize(n),
                Value::Int(n) if n >= 0 => nonneg_i64_to_usize(i64::from(n)),
                Value::BigInt(_) | Value::Int(_) => {
                    return Err(DbError::syntax_error(
                        "LIMIT requires a non-negative integer value",
                    ));
                }
                Value::Real(_) | Value::Double(_) | Value::Numeric(_) => {
                    return Err(DbError::syntax_error("LIMIT requires an integer value"));
                }
                _ => result.len(),
            };
            result.truncate(n);
        }

        Ok(result)
    }

    fn execute_cypher_call_subquery(
        &self,
        context: &ExecutionContext,
        subquery: &CypherQueryPlan,
        input_bindings: Vec<BindingRow>,
    ) -> DbResult<Vec<BindingRow>> {
        if subquery.union.is_some() {
            return Err(DbError::feature_not_supported(
                "UNION inside Cypher CALL subqueries is not yet supported",
            ));
        }

        let return_as_with = if subquery.returns.is_empty() {
            None
        } else {
            Some(aiondb_plan::graph::CypherWithClause {
                distinct: subquery.distinct,
                items: subquery.returns.clone(),
                preserve_binding_sources: vec![None; subquery.returns.len()],
                filter: None,
                order_by: subquery.order_by.clone(),
                skip: subquery.skip.clone(),
                limit: subquery.limit.clone(),
            })
        };

        let mut output = Vec::new();
        for outer in input_bindings {
            context.check_deadline()?;
            let sub_bindings =
                self.execute_cypher_subquery_body(context, subquery, vec![outer.clone()])?;

            let Some(return_as_with) = return_as_with.as_ref() else {
                ensure_graph_result_row_capacity(context, output.len())?;
                output.push(outer);
                continue;
            };

            let returned = self.execute_cypher_with(context, return_as_with, sub_bindings)?;
            for row in returned {
                let mut merged = outer.clone();
                for (name, value) in row.entries {
                    merged.insert_shared_binding(name, value);
                }
                ensure_graph_result_row_capacity(context, output.len())?;
                output.push(merged);
            }
        }

        Ok(output)
    }

    fn execute_cypher_subquery_body(
        &self,
        context: &ExecutionContext,
        subquery: &CypherQueryPlan,
        mut bindings: Vec<BindingRow>,
    ) -> DbResult<Vec<BindingRow>> {
        for op in &subquery.pipeline {
            context.check_deadline()?;
            match op {
                CypherPipelineOp::Unwind(u) => {
                    bindings = self.execute_cypher_unwind(context, u, bindings)?;
                }
                CypherPipelineOp::With(ref w) => {
                    bindings = self.execute_cypher_with(context, w, bindings)?;
                }
                CypherPipelineOp::Match(m) => {
                    bindings = self.execute_cypher_match(context, m, bindings)?;
                }
                CypherPipelineOp::ProcedureCall(call) => {
                    bindings = self.execute_cypher_procedure_call(context, call, bindings)?;
                }
                CypherPipelineOp::CallSubquery(nested) => {
                    bindings = self.execute_cypher_call_subquery(context, nested, bindings)?;
                }
                CypherPipelineOp::Foreach(foreach) => {
                    bindings = self.execute_cypher_foreach(context, foreach, bindings)?;
                }
            }
        }

        for match_clause in &subquery.matches {
            context.check_deadline()?;
            bindings = self.execute_cypher_match(context, match_clause, bindings)?;
        }

        for create_clause in &subquery.creates {
            context.check_deadline()?;
            let (new_bindings, _) = self.execute_cypher_create(context, create_clause, bindings)?;
            bindings = new_bindings;
        }

        for merge_clause in &subquery.merges {
            context.check_deadline()?;
            bindings = self.execute_cypher_merge(context, merge_clause, bindings)?;
        }

        for set_item in &subquery.sets {
            context.check_deadline()?;
            self.execute_cypher_set(context, set_item, &mut bindings)?;
        }

        for delete_clause in &subquery.deletes {
            context.check_deadline()?;
            let _ = self.execute_cypher_delete(context, delete_clause, &bindings)?;
        }

        Ok(bindings)
    }

    // -----------------------------------------------------------------------
    // Helpers
    // -----------------------------------------------------------------------

    /// Build a flat `Row` from a binding row by concatenating all bound rows
    /// in deterministic (sorted variable name) order.
    ///
    /// Uses `raw_row` (without system columns) so that column ordinals match
    /// the type-checker's synthetic relation (which is built from the binder's
    /// table column descriptors, also without system columns).
    pub(super) fn build_flat_row(&self, binding: &BindingRow) -> Row {
        let mut values = Vec::new();
        let mut keys: Vec<&String> = binding
            .iter()
            .map(|(k, _)| k)
            .filter(|k| !k.starts_with("__"))
            .collect();
        keys.sort();
        for key in keys {
            match binding.get(key.as_str()) {
                Some(BoundValue::Node { raw_row, .. } | BoundValue::Edge { raw_row, .. }) => {
                    values.extend_from_slice(&raw_row.values);
                }
                Some(BoundValue::Scalar(v)) => {
                    values.push(v.clone());
                }
                Some(BoundValue::Path {
                    nodes,
                    relationships,
                    directions,
                }) => {
                    values.push(Value::Text(format_cypher_path_literal(
                        binding,
                        nodes,
                        relationships,
                        directions,
                    )));
                }
                Some(BoundValue::PathValues {
                    nodes,
                    relationships,
                    directions,
                }) => {
                    values.push(Value::Text(format_cypher_path_value_literal(
                        nodes,
                        relationships,
                        directions,
                    )));
                }
                Some(BoundValue::Null { column_count }) => {
                    for _ in 0..*column_count {
                        values.push(Value::Null);
                    }
                }
                None => {}
            }
        }
        Row::new(values)
    }

    /// Resolve a Cypher variable reference to its scalar value from bindings.
    ///
    /// For scalar bindings (UNWIND), returns the value directly.
    /// For Node/Edge bindings, returns the Cypher textual literal
    /// `(:Label {props})` / `[:TYPE {props}]` so RETURN/ORDER BY/printer
    /// downstream see the formatted node/edge instead of falling back to
    /// the raw id column.
    fn resolve_cypher_variable(&self, binding: &BindingRow, name: &str) -> Option<Value> {
        match binding.get(name) {
            Some(BoundValue::Scalar(v)) => Some(v.clone()),
            Some(BoundValue::Null { .. }) => Some(Value::Null),
            Some(BoundValue::Node {
                row,
                column_names,
                labels,
                ..
            }) => Some(Value::Text(format_cypher_node_literal(
                column_names,
                row,
                labels,
            ))),
            Some(BoundValue::Edge {
                row,
                column_names,
                rel_type,
                ..
            }) => Some(Value::Text(format_cypher_edge_literal(
                column_names,
                row,
                rel_type,
            ))),
            Some(BoundValue::Path {
                nodes,
                relationships,
                directions,
            }) => Some(Value::Text(format_cypher_path_literal(
                binding,
                nodes,
                relationships,
                directions,
            ))),
            Some(BoundValue::PathValues {
                nodes,
                relationships,
                directions,
            }) => Some(Value::Text(format_cypher_path_value_literal(
                nodes,
                relationships,
                directions,
            ))),
            None => None,
        }
    }

    /// Evaluate a predicate expression against a binding row.
    pub(super) fn evaluate_graph_predicate(
        &self,
        context: &ExecutionContext,
        expr: &TypedExpr,
        binding: &BindingRow,
    ) -> DbResult<bool> {
        predicate_matches(Some(
            self.evaluate_cypher_expr_with_binding(expr, binding, context),
        ))
    }

    /// Check whether property expressions on a node pattern match a row.
    pub(super) fn check_property_filters(
        &self,
        context: &ExecutionContext,
        properties: &[CypherPropertyExpr],
        column_names: &[String],
        compat_row: &Row,
        binding: &BindingRow,
    ) -> DbResult<bool> {
        for prop in properties {
            let expected = self.evaluate_cypher_expr_with_binding(&prop.value, binding, context)?;
            let actual = column_names
                .iter()
                .position(|name| name.eq_ignore_ascii_case(&prop.key))
                .and_then(|idx| compat_row.values.get(idx));
            let actual_ref = actual.unwrap_or(&Value::Null);

            let equal = if *actual_ref == expected {
                true
            } else {
                matches!(
                    compare_runtime_values(actual_ref, &expected)?,
                    Some(std::cmp::Ordering::Equal)
                )
            };

            if !equal {
                return Ok(false);
            }
        }
        Ok(true)
    }

    /// Check whether an already-bound node still matches property filters.
    pub(super) fn node_properties_match(
        &self,
        context: &ExecutionContext,
        node: &CypherNodePattern,
        bound: &BoundValue,
        binding: &BindingRow,
    ) -> DbResult<bool> {
        match bound {
            BoundValue::Node {
                row, column_names, ..
            } => self.check_property_filters(
                context,
                &node.properties,
                column_names.as_ref(),
                row,
                binding,
            ),
            _ => Ok(false),
        }
    }

    /// Check whether an edge row's endpoints are adjacent to the most
    /// recently bound node.
    pub(super) fn check_adjacency(
        &self,
        binding: &BindingRow,
        current_node: Option<&CypherNodePattern>,
        direction: CypherRelDirection,
        source_id: &Value,
        target_id: &Value,
    ) -> bool {
        let current_node_id = self.find_current_node_id_for_pattern(binding, current_node);
        let Some(current_id) = current_node_id else {
            return true; // No prior node bound.
        };

        match direction {
            CypherRelDirection::Outgoing => current_id == *source_id,
            CypherRelDirection::Incoming => current_id == *target_id,
            CypherRelDirection::Both => current_id == *source_id || current_id == *target_id,
        }
    }

    fn binding_key_for_node_pattern(node: &CypherNodePattern) -> Option<String> {
        node.variable.clone().or_else(|| {
            node.table_id
                .map(|table_id| format!("__anon_node_{}__", table_id.get()))
        })
    }

    /// Find the current node id for a specific pattern step.
    ///
    /// This must prefer the node that immediately precedes the relationship
    /// in the current pattern instead of an arbitrary previously bound node.
    pub(super) fn find_current_node_id_for_pattern(
        &self,
        binding: &BindingRow,
        current_node: Option<&CypherNodePattern>,
    ) -> Option<Value> {
        if let Some(node) = current_node {
            if let Some(key) = Self::binding_key_for_node_pattern(node) {
                match binding.get(&key) {
                    Some(BoundValue::Node { id_value, .. }) => return Some(id_value.clone()),
                    Some(BoundValue::Null { .. }) => return None,
                    _ => {}
                }
            }
        }

        // Fall back to the synthetic next-node marker only when we cannot
        // anchor the step to an explicit node from the current pattern.
        if let Some(BoundValue::Node { row, .. }) = binding.get("__edge_next_node_id__") {
            if !row.values.is_empty() {
                return Some(row.values[0].clone());
            }
        }

        self.find_current_node_id(binding)
    }

    /// Find the `id_value` of the most recently bound node.
    pub(super) fn find_current_node_id(&self, binding: &BindingRow) -> Option<Value> {
        // Prefer the synthetic next-node marker from a prior relationship step.
        if let Some(BoundValue::Node { row, .. }) = binding.get("__edge_next_node_id__") {
            if !row.values.is_empty() {
                return Some(row.values[0].clone());
            }
        }
        // Fallback: find the last node binding by iterating values.
        let mut last_id = None;
        for value in binding.values() {
            if let BoundValue::Node { id_value, .. } = value.as_ref() {
                last_id = Some(id_value.clone());
            }
        }
        last_id
    }

    /// Extract the node identity value from a bound variable.
    pub(super) fn extract_node_id(&self, binding: &BindingRow, variable: &str) -> DbResult<Value> {
        match binding.get(variable) {
            Some(BoundValue::Node { id_value, .. }) => Ok(id_value.clone()),
            Some(BoundValue::Null { .. }) => Ok(Value::Null),
            Some(_) => Err(DbError::internal(format!(
                "variable '{variable}' is not bound to a node"
            ))),
            None => Err(DbError::internal(format!(
                "variable '{variable}' is not bound"
            ))),
        }
    }

    /// Resolve the source and target endpoint column ordinals for an edge table.
    ///
    /// Legacy edge labels use `source_id` / `target_id`. FK-backed edge labels
    /// can override those names through `EdgeLabelDescriptor::endpoints`.
    /// Returns (`source_column_index`, `target_column_index`).
    pub(super) fn resolve_edge_endpoint_columns(
        &self,
        context: &ExecutionContext,
        edge_table_id: RelationId,
    ) -> DbResult<(usize, usize)> {
        let edge = self.edge_label_for_table_id(context, edge_table_id)?;
        self.resolve_edge_endpoint_columns_for_table_and_descriptor(
            context,
            edge_table_id,
            edge.as_ref(),
        )
    }

    pub(super) fn resolve_edge_endpoint_columns_for_label(
        &self,
        context: &ExecutionContext,
        edge: &EdgeLabelDescriptor,
    ) -> DbResult<(usize, usize)> {
        self.resolve_edge_endpoint_columns_for_table_and_descriptor(
            context,
            edge.table_id,
            Some(edge),
        )
    }

    pub(super) fn resolve_edge_endpoint_columns_for_rel(
        &self,
        context: &ExecutionContext,
        edge_table_id: RelationId,
        rel_type: Option<&str>,
    ) -> DbResult<((usize, usize), bool)> {
        let edge = match rel_type {
            Some(label) => self.catalog_reader.get_edge_label(context.txn_id, label)?,
            None => self.edge_label_for_table_id(context, edge_table_id)?,
        };
        let columns = self.resolve_edge_endpoint_columns_for_table_and_descriptor(
            context,
            edge_table_id,
            edge.as_ref(),
        )?;
        let can_use_table_adjacency = edge.as_ref().map_or(true, |edge| edge.endpoints.is_none());
        Ok((columns, can_use_table_adjacency))
    }

    fn resolve_edge_endpoint_columns_for_table_and_descriptor(
        &self,
        context: &ExecutionContext,
        edge_table_id: RelationId,
        edge: Option<&EdgeLabelDescriptor>,
    ) -> DbResult<(usize, usize)> {
        let table = self
            .catalog_reader
            .get_table_by_id(context.txn_id, edge_table_id)?
            .ok_or_else(|| DbError::internal("edge table not found"))?;
        let endpoints = edge.and_then(|edge| edge.endpoints.as_ref());
        let source_column =
            endpoints.map_or("source_id", |endpoints| endpoints.source_id_column.as_str());
        let target_column =
            endpoints.map_or("target_id", |endpoints| endpoints.target_id_column.as_str());
        let src_idx = self
            .find_column_index(&table.columns, source_column)
            .ok_or_else(|| DbError::internal("edge table missing source endpoint column"))?;
        let tgt_idx = self
            .find_column_index(&table.columns, target_column)
            .ok_or_else(|| DbError::internal("edge table missing target endpoint column"))?;
        Ok((src_idx, tgt_idx))
    }

    pub(super) fn edge_label_for_table_id(
        &self,
        context: &ExecutionContext,
        edge_table_id: RelationId,
    ) -> DbResult<Option<EdgeLabelDescriptor>> {
        Ok(self
            .catalog_reader
            .list_edge_labels(context.txn_id)?
            .into_iter()
            .find(|edge| edge.table_id == edge_table_id))
    }

    pub(super) fn projected_edge_label_for_table_id(
        &self,
        context: &ExecutionContext,
        edge_table_id: RelationId,
    ) -> DbResult<Option<EdgeLabelDescriptor>> {
        Ok(self
            .edge_label_for_table_id(context, edge_table_id)?
            .filter(|edge| edge.endpoints.is_some()))
    }

    pub(super) fn find_btree_index_for_column_ordinal(
        &self,
        context: &ExecutionContext,
        table_id: RelationId,
        column_ordinal: usize,
    ) -> DbResult<Option<IndexId>> {
        let Some(table) = self
            .catalog_reader
            .get_table_by_id(context.txn_id, table_id)?
        else {
            return Ok(None);
        };
        let Some(column) = table.columns.get(column_ordinal) else {
            return Ok(None);
        };
        let mut best: Option<(IndexId, bool, usize)> = None;
        for index in self.catalog_reader.list_indexes(context.txn_id, table_id)? {
            if index.kind != aiondb_catalog::IndexKind::BTree {
                continue;
            }
            if !index
                .key_columns
                .first()
                .is_some_and(|key| key.column_id == column.column_id)
            {
                continue;
            }
            let candidate = (index.index_id, index.unique, index.key_columns.len());
            match best {
                None => best = Some(candidate),
                Some((_, best_unique, best_key_len))
                    if (candidate.1 && !best_unique)
                        || (candidate.1 == best_unique && candidate.2 < best_key_len) =>
                {
                    best = Some(candidate);
                }
                _ => {}
            }
        }
        Ok(best.map(|(index_id, _, _)| index_id))
    }

    /// Find the column index by name in a column descriptor list.
    pub(super) fn find_column_index(
        &self,
        columns: &[ColumnDescriptor],
        name: &str,
    ) -> Option<usize> {
        columns
            .iter()
            .position(|c| c.name.eq_ignore_ascii_case(name))
    }
}
