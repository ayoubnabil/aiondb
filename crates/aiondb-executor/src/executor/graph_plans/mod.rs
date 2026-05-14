//! Executor for Cypher graph queries.
//!
//! Handles MATCH pattern matching, CREATE/MERGE/DELETE graph mutations,
//! SET property updates, and RETURN clause projection.
//! The plan types come from `aiondb_plan::graph` (`CypherQueryPlan`, etc.).
//! Node and edge data is stored in regular SQL tables; the executor scans,
//! inserts, updates and deletes through the same storage API used by DML.

mod graph_match;
mod graph_mutate;

use graph_mutate::{dedup_rows_by_values, value_to_bfs_key};

use std::cmp::Ordering;
use std::collections::{HashMap, HashSet, VecDeque};
use std::mem::size_of;
use std::sync::Arc;

use aiondb_catalog::{
    ColumnDescriptor, EdgeLabelDescriptor, NodeLabelDescriptor, QualifiedName, SequenceDescriptor,
    TableDescriptor,
};
use aiondb_core::{
    ColumnId, DataType, DbError, DbResult, IndexId, RelationId, Row, SchemaId, SequenceId,
    SqlState, TupleId, Value,
};
use aiondb_eval::{build_hash_key, compare_runtime_values, ValueHashKey};
use aiondb_graph::{
    pattern::AdjacentEdge as GraphAdjacentEdge, shortest_path as graph_shortest_path,
    PathElement as GraphPathElement, RowProvider as GraphRowProvider,
};
use aiondb_plan::graph::{
    CypherCreateClause, CypherDeleteClause, CypherMatchClause, CypherMergeClause,
    CypherNodePattern, CypherPathFunction, CypherPattern, CypherPipelineOp, CypherPropertyExpr,
    CypherQueryPlan, CypherRelDirection, CypherRelPattern, CypherSetItem, IndexScanInfo,
};
use aiondb_plan::{ProjectionExpr, ScalarFunction, SortExpr, TypedExpr, TypedExprKind};

use tracing::debug;

use super::*;
pub(super) use aiondb_core::convert::usize_to_u32_saturating as usize_to_u32;
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

/// Format a graph node as the Cypher textual literal `(:Label[:Label2] {props})`.
/// `column_names` and `row` come from the node's backing table; columns whose
/// name starts with `__` are treated as system columns and skipped, plus any
/// column whose value is NULL is omitted from the property bag. The
/// synthetic `_default` label used to back anonymous `({prop: ...})` nodes
/// is hidden from the output so the literal round-trips through Cypher.
fn format_cypher_node_literal(column_names: &[String], row: &Row, labels: &[String]) -> String {
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
fn format_cypher_edge_literal(column_names: &[String], row: &Row, rel_type: &str) -> String {
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

fn format_cypher_property_value(value: &Value) -> String {
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
/// from one place (for example bidirectional BFS or weighted shortest path).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum PathSearchMode {
    SingleShortest,
    AllShortest,
}

impl PathSearchMode {
    pub(super) fn from_path_function(func: CypherPathFunction) -> Self {
        match func {
            CypherPathFunction::ShortestPath => Self::SingleShortest,
            CypherPathFunction::AllShortestPaths => Self::AllShortest,
        }
    }

    pub(super) fn allows_multiple_shortest_paths(self) -> bool {
        matches!(self, Self::AllShortest)
    }
}

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

struct ExecutorGraphRowProvider<'a> {
    executor: &'a Executor,
    context: &'a ExecutionContext,
    edge_endpoint_overrides: HashMap<RelationId, (usize, usize)>,
}

impl<'a> GraphRowProvider for ExecutorGraphRowProvider<'a> {
    fn scan_table(&self, table_id: RelationId) -> DbResult<Vec<Row>> {
        let mut rows = Vec::new();
        let mut stream = self
            .executor
            .scan_table_locked(self.context, table_id, None)?;
        while let Some(record) = stream.next()? {
            self.context.check_deadline()?;
            rows.push(self.executor.compat_scan_row_for_table_id(
                self.context,
                table_id,
                &record,
            )?);
        }
        Ok(rows)
    }

    fn column_index(&self, table_id: RelationId, column: &str) -> DbResult<Option<usize>> {
        if let Some((source_idx, target_idx)) = self.edge_endpoint_overrides.get(&table_id) {
            if column.eq_ignore_ascii_case("source_id") {
                return Ok(Some(*source_idx));
            }
            if column.eq_ignore_ascii_case("target_id") {
                return Ok(Some(*target_idx));
            }
        }
        Ok(self
            .executor
            .catalog_reader
            .get_table_by_id(self.context.txn_id, table_id)?
            .and_then(|table| {
                table
                    .columns
                    .iter()
                    .position(|entry| entry.name.eq_ignore_ascii_case(column))
            }))
    }

    fn column_names(&self, table_id: RelationId) -> DbResult<Vec<String>> {
        Ok(self
            .executor
            .catalog_reader
            .get_table_by_id(self.context.txn_id, table_id)?
            .map(|table| {
                table
                    .columns
                    .into_iter()
                    .map(|column| column.name)
                    .collect()
            })
            .unwrap_or_default())
    }

    fn adjacency_lookup_edges(
        &self,
        edge_table_id: RelationId,
        node_id: &Value,
        direction: aiondb_graph::traversal::TraversalDirection,
    ) -> DbResult<Vec<GraphAdjacentEdge>> {
        let directions: &[bool] = match direction {
            aiondb_graph::traversal::TraversalDirection::Outgoing => &[true],
            aiondb_graph::traversal::TraversalDirection::Incoming => &[false],
            aiondb_graph::traversal::TraversalDirection::Both => &[true, false],
        };

        if self.edge_endpoint_overrides.contains_key(&edge_table_id) {
            return self.fallback_adjacency_lookup_edges(edge_table_id, node_id, direction);
        }

        let mut result = Vec::new();
        let mut seen = HashSet::new();

        for &outgoing in directions {
            match self.executor.storage_dml.adjacency_lookup(
                self.context.txn_id,
                &self.context.snapshot,
                edge_table_id,
                node_id,
                outgoing,
            ) {
                Ok(tuple_ids) => {
                    for tuple_id in tuple_ids {
                        if !seen.insert(tuple_id) {
                            continue;
                        }
                        let Some(row) = self.executor.storage_dml.fetch(
                            self.context.txn_id,
                            &self.context.snapshot,
                            edge_table_id,
                            tuple_id,
                            None,
                        )?
                        else {
                            continue;
                        };
                        let record = aiondb_storage_api::TupleRecord {
                            tuple_id,
                            heap_position: tuple_id.get(),
                            row,
                        };
                        result.push(GraphAdjacentEdge {
                            row: self.executor.compat_scan_row_for_table_id(
                                self.context,
                                edge_table_id,
                                &record,
                            )?,
                            tuple_id,
                        });
                    }
                }
                Err(error) if error.sqlstate() == SqlState::FeatureNotSupported => {
                    return self.fallback_adjacency_lookup_edges(edge_table_id, node_id, direction);
                }
                Err(error) => return Err(error),
            }
        }

        Ok(result)
    }
}

impl<'a> ExecutorGraphRowProvider<'a> {
    fn fallback_adjacency_lookup_edges(
        &self,
        edge_table_id: RelationId,
        node_id: &Value,
        direction: aiondb_graph::traversal::TraversalDirection,
    ) -> DbResult<Vec<GraphAdjacentEdge>> {
        let (src_idx, tgt_idx) =
            if let Some(endpoints) = self.edge_endpoint_overrides.get(&edge_table_id).copied() {
                endpoints
            } else {
                let src_idx = self
                    .column_index(edge_table_id, "source_id")?
                    .ok_or_else(|| DbError::internal("edge table missing source_id column"))?;
                let tgt_idx = self
                    .column_index(edge_table_id, "target_id")?
                    .ok_or_else(|| DbError::internal("edge table missing target_id column"))?;
                (src_idx, tgt_idx)
            };

        let mut result = Vec::new();
        let mut stream = self
            .executor
            .scan_table_locked(self.context, edge_table_id, None)?;
        while let Some(record) = stream.next()? {
            self.context.check_deadline()?;
            let compat =
                self.executor
                    .compat_scan_row_for_table_id(self.context, edge_table_id, &record)?;
            let src = compat.values.get(src_idx).unwrap_or(&Value::Null);
            let tgt = compat.values.get(tgt_idx).unwrap_or(&Value::Null);
            let matches = match direction {
                aiondb_graph::traversal::TraversalDirection::Outgoing => src == node_id,
                aiondb_graph::traversal::TraversalDirection::Incoming => tgt == node_id,
                aiondb_graph::traversal::TraversalDirection::Both => {
                    src == node_id || tgt == node_id
                }
            };
            if matches {
                result.push(GraphAdjacentEdge {
                    row: compat,
                    tuple_id: record.tuple_id,
                });
            }
        }
        Ok(result)
    }
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

fn materialize_named_path_pattern(pattern: &CypherPattern) -> CypherPattern {
    let Some(path_variable) = pattern.path_variable.as_deref() else {
        return pattern.clone();
    };
    let mut materialized = pattern.clone();
    let safe_name = path_variable
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || ch == '_' {
                ch
            } else {
                '_'
            }
        })
        .collect::<String>();

    for (idx, node) in materialized.nodes.iter_mut().enumerate() {
        if node.variable.is_none() {
            node.variable = Some(format!("__path_{safe_name}_node_{idx}"));
        }
    }
    for (idx, rel) in materialized.relationships.iter_mut().enumerate() {
        if rel.variable.is_none() {
            rel.variable = Some(format!("__path_{safe_name}_rel_{idx}"));
        }
    }
    materialized
}

fn validate_named_path_pattern(pattern: &CypherPattern) -> DbResult<()> {
    if pattern.path_variable.is_none() {
        return Ok(());
    }
    if pattern.path_function.is_some() {
        return Err(DbError::feature_not_supported(
            "named shortestPath bindings are not supported yet",
        ));
    }
    let has_variable_length = pattern
        .relationships
        .iter()
        .any(|rel| rel.min_hops.is_some() || rel.max_hops.is_some());
    if has_variable_length && (pattern.relationships.len() != 1 || pattern.nodes.len() != 2) {
        return Err(DbError::feature_not_supported(
            "named multi-segment variable-length path bindings are not supported yet",
        ));
    }
    Ok(())
}

fn bind_named_path_variable(
    pattern: &CypherPattern,
    mut bindings: Vec<BindingRow>,
) -> Vec<BindingRow> {
    let Some(path_variable) = pattern.path_variable.as_ref() else {
        return bindings;
    };
    let nodes = Arc::new(
        pattern
            .nodes
            .iter()
            .filter_map(|node| node.variable.clone())
            .collect::<Vec<_>>(),
    );
    let relationships = Arc::new(
        pattern
            .relationships
            .iter()
            .filter_map(|rel| rel.variable.clone())
            .collect::<Vec<_>>(),
    );
    let directions = Arc::new(
        pattern
            .relationships
            .iter()
            .map(|rel| rel.direction)
            .collect::<Vec<_>>(),
    );
    for binding in &mut bindings {
        if binding.get(path_variable).is_some() {
            continue;
        }
        binding.insert_binding(
            path_variable.clone(),
            BoundValue::Path {
                nodes: Arc::clone(&nodes),
                relationships: Arc::clone(&relationships),
                directions: Arc::clone(&directions),
            },
        );
    }
    bindings
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
fn pick_match_pivot_index(pattern: &CypherPattern) -> Option<usize> {
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
fn flip_relationship_direction(rel: &CypherRelPattern) -> CypherRelPattern {
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

fn collect_graph_filter_conjuncts<'a>(expr: &'a TypedExpr, out: &mut Vec<&'a TypedExpr>) {
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

struct GraphFilterConjunct<'a> {
    expr: &'a TypedExpr,
    referenced_vars: Option<HashSet<String>>,
}

impl<'a> GraphFilterConjunct<'a> {
    fn new(expr: &'a TypedExpr) -> Self {
        Self {
            expr,
            referenced_vars: referenced_graph_variables(expr),
        }
    }

    fn is_ready(&self, binding: &BindingRow) -> bool {
        let Some(vars) = self.referenced_vars.as_ref() else {
            return false;
        };
        vars.iter()
            .all(|variable| binding.get(variable.as_str()).is_some())
    }
}

fn build_graph_filter_conjuncts(filter: &TypedExpr) -> Vec<GraphFilterConjunct<'_>> {
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

fn exact_column_literal_equality(expr: &TypedExpr) -> Option<(&str, Value)> {
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
fn extract_column_literal_range(
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

        let values = self.storage_dml.adjacency_neighbors(
            context.txn_id,
            &context.snapshot,
            edge_table_id,
            node_id,
            outgoing,
        )?;

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
    ) -> DbResult<Option<HashSet<ValueHashKey, join_plans::JoinFxBuildHasher>>> {
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

        if let Some(result) = self.try_execute_fast_one_hop_id_lookup(plan, context)? {
            return Ok(result);
        }
        if let Some(result) = self.try_execute_fast_one_hop_endpoint_id_lookup(plan, context)? {
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
                CypherPipelineOp::CallSubquery(subquery) => {
                    bindings = self.execute_cypher_call_subquery(context, subquery, bindings)?;
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

        let mut ids = match self.fast_graph_adjacency_neighbors_cached(
            context,
            edge_table_id,
            &start_id,
            true,
        ) {
            Ok(tuple_ids) => tuple_ids,
            Err(_) => return Ok(None),
        };

        ids.retain(|id| !id.is_null());

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
            let mut neighbors = match self.fast_graph_adjacency_neighbors_cached(
                context,
                edge_table_id,
                &anchor_id,
                outgoing,
            ) {
                Ok(ids) => ids,
                Err(_) => return Ok(None),
            };
            neighbors.retain(|id| !id.is_null());
            ids.append(&mut neighbors);
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

        let mut ids = Vec::new();
        for mut middle_id in middle_ids {
            if middle_id.is_null() {
                continue;
            }
            normalize_int_key(&mut middle_id);
            let mut next_ids = match self.fast_graph_adjacency_neighbors_cached(
                context,
                edge_table_id,
                &middle_id,
                true,
            ) {
                Ok(ids) => ids,
                Err(_) => return Ok(None),
            };
            ids.append(&mut next_ids);
        }
        ids.retain(|id| !id.is_null());

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

        let mut ids = Vec::new();
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
                let mut third_ids = match self.fast_graph_adjacency_neighbors_cached(
                    context,
                    first_rel_table_id,
                    &second_id,
                    true,
                ) {
                    Ok(ids) => ids,
                    Err(_) => return Ok(None),
                };
                third_ids.retain(|id| !id.is_null());
                ids.append(&mut third_ids);
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

        let first_start = &first.nodes[0];
        let first_end = &first.nodes[1];
        let second_start = &second.nodes[0];
        let second_end = &second.nodes[1];
        let first_rel = &first.relationships[0];
        let second_rel = &second.relationships[0];
        let (Some(start_var), Some(first_end_var), Some(second_start_var), Some(second_end_var)) = (
            first_start.variable.as_deref(),
            first_end.variable.as_deref(),
            second_start.variable.as_deref(),
            second_end.variable.as_deref(),
        ) else {
            return Ok(None);
        };
        let expected_first_return = format!("{first_end_var}.id");
        let expected_second_return = format!("{second_end_var}.id");
        if start_var != second_start_var
            || column_ref_name(&plan.returns[0].expr) != Some(expected_first_return.as_str())
            || column_ref_name(&plan.returns[1].expr) != Some(expected_second_return.as_str())
        {
            return Ok(None);
        }
        if second_start
            .table_id
            .is_some_and(|table_id| Some(table_id) != first_start.table_id)
        {
            return Ok(None);
        }
        if first_start.table_id.is_none()
            || first_end.table_id.is_none()
            || second_end.table_id.is_none()
            || !first_start.properties.is_empty()
            || !first_end.properties.is_empty()
            || !second_start.properties.is_empty()
            || !second_end.properties.is_empty()
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

        let filter_value = match match_clause.filter.as_ref() {
            Some(filter) => {
                let Some(value) =
                    exact_named_column_literal_gt(filter, &format!("{first_end_var}.number"))
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
        let Some(first_target_table_id) = first_end.table_id else {
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
            let mut left_targets = Vec::with_capacity(neighbor_ids.len());
            for target_id in &neighbor_ids {
                if target_id.is_null() {
                    continue;
                }
                let first_target_allowed =
                    if let Some(Some(allowed_ids)) = allowed_left_target_ids.as_ref() {
                        let mut normalized_target_id = target_id.clone();
                        normalize_int_key(&mut normalized_target_id);
                        allowed_ids.contains(&build_hash_key(&normalized_target_id)?)
                    } else {
                        true
                    };
                if first_target_allowed {
                    left_targets.push(target_id.clone());
                }
            }
            if left_targets.is_empty() {
                continue;
            }
            for left in &left_targets {
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

        let start = &pattern.nodes[0];
        let end = &pattern.nodes[1];
        let rel = &pattern.relationships[0];
        let Some(end_variable) = end.variable.as_deref() else {
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
            || rel.variable.is_some()
            || rel.min_hops.is_some()
            || rel.max_hops.is_some()
            || !rel.properties.is_empty()
        {
            return Ok(None);
        }

        let Some(filter_value) = match_clause.filter.as_ref().and_then(|filter| {
            exact_named_column_literal_gt(filter, &format!("{end_variable}.number"))
        }) else {
            return Ok(None);
        };
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
            let next_ids = match self.fast_graph_adjacency_neighbors_cached(
                context,
                edge_table_id,
                &middle_id,
                true,
            ) {
                Ok(ids) => ids,
                Err(_) => return Ok(None),
            };
            for next_id in next_ids {
                if next_id.is_null() {
                    continue;
                }
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
                CypherPipelineOp::CallSubquery(nested) => {
                    bindings = self.execute_cypher_call_subquery(context, nested, bindings)?;
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
    // MATCH
    // -----------------------------------------------------------------------

    /// Execute a single MATCH clause against the storage layer.
    fn execute_cypher_match(
        &self,
        context: &ExecutionContext,
        clause: &CypherMatchClause,
        input_bindings: Vec<BindingRow>,
    ) -> DbResult<Vec<BindingRow>> {
        // Pre-compute column counts for every table referenced in the
        // clause's patterns.  These are only needed for the OPTIONAL MATCH
        // null-binding fallback, but computing them once here avoids
        // repeated catalog lookups inside the per-binding loop.
        let col_count_cache: HashMap<RelationId, usize> = if clause.optional {
            let mut cache = HashMap::new();
            for pattern in &clause.patterns {
                for node in &pattern.nodes {
                    if let Some(tid) = node.table_id {
                        if let std::collections::hash_map::Entry::Vacant(entry) = cache.entry(tid) {
                            let count = self
                                .catalog_reader
                                .get_table_by_id(context.txn_id, tid)?
                                .map_or(0, |t| t.columns.len());
                            entry.insert(count);
                        }
                    }
                }
                for rel in &pattern.relationships {
                    if let Some(tid) = rel.table_id {
                        if let std::collections::hash_map::Entry::Vacant(entry) = cache.entry(tid) {
                            let count = self
                                .catalog_reader
                                .get_table_by_id(context.txn_id, tid)?
                                .map_or(0, |t| t.columns.len());
                            entry.insert(count);
                        }
                    }
                }
            }
            cache
        } else {
            HashMap::new()
        };

        let filter_conjuncts = clause
            .filter
            .as_ref()
            .map(|filter| build_graph_filter_conjuncts(filter))
            .unwrap_or_default();
        // ALWAYS apply the inline-property index-hint pass so
        // patterns like `(a:Person {id: 50})` pick up the PK btree
        // and avoid a full SeqScan when no top-level WHERE clause
        // is present. The WHERE-based pass adds the same hint when
        // it sees `WHERE a.id = 50`, but inline-property syntax
        // never went through it before — so a pattern of the shape
        // `MATCH (x)-->(a {id:50})-->(b)` was full-scanning the
        // backing table for `a` instead of doing the O(log n) PK
        // lookup. The two passes are complementary: the inline
        // pass fills in hints from `node.properties`, the WHERE
        // pass adds them from `WHERE` predicates.
        let hinted_patterns;
        let inline_first;
        let patterns: &[CypherPattern] = if let Some(filter) = clause.filter.as_ref() {
            inline_first = clause
                .patterns
                .iter()
                .map(|pattern| self.apply_inline_property_index_hints(context, pattern))
                .collect::<DbResult<Vec<_>>>()?;
            hinted_patterns = inline_first
                .iter()
                .map(|pattern| self.apply_match_filter_index_hints(context, pattern, filter))
                .collect::<DbResult<Vec<_>>>()?;
            &hinted_patterns
        } else {
            inline_first = clause
                .patterns
                .iter()
                .map(|pattern| self.apply_inline_property_index_hints(context, pattern))
                .collect::<DbResult<Vec<_>>>()?;
            &inline_first
        };

        let mut result_bindings = Vec::new();

        for input_binding in &input_bindings {
            context.check_deadline()?;
            let mut current_bindings = vec![input_binding.clone()];

            // Process each pattern in the clause.
            for pattern in patterns {
                validate_named_path_pattern(pattern)?;
                let materialized_pattern;
                let pattern = if pattern.path_variable.is_some() {
                    materialized_pattern = materialize_named_path_pattern(pattern);
                    &materialized_pattern
                } else {
                    pattern
                };
                current_bindings =
                    self.match_pattern(context, pattern, current_bindings, &filter_conjuncts)?;
                current_bindings = bind_named_path_variable(pattern, current_bindings);
                if current_bindings.is_empty() && !clause.optional {
                    break;
                }
            }

            // Apply WHERE filter.
            if let Some(ref filter) = clause.filter {
                let mut filtered = Vec::new();
                for binding in current_bindings {
                    match self.evaluate_graph_predicate(context, filter, &binding) {
                        Ok(true) => filtered.push(binding),
                        Ok(false) => {}
                        Err(e) => return Err(e),
                    }
                }
                current_bindings = filtered;
            }

            if current_bindings.is_empty() && clause.optional {
                // OPTIONAL MATCH: keep input binding with NULLs for pattern
                // variables that were not matched.  Column counts come from
                // the pre-computed cache to avoid per-binding catalog lookups.
                let mut null_binding = input_binding.clone();
                for pattern in &clause.patterns {
                    for node in &pattern.nodes {
                        if let Some(ref var) = node.variable {
                            let col_count = node
                                .table_id
                                .and_then(|tid| col_count_cache.get(&tid).copied())
                                .unwrap_or(0);
                            null_binding.insert_binding(
                                var.clone(),
                                BoundValue::Null {
                                    column_count: col_count,
                                },
                            );
                        }
                    }
                    for rel in &pattern.relationships {
                        if let Some(ref var) = rel.variable {
                            let col_count = rel
                                .table_id
                                .and_then(|tid| col_count_cache.get(&tid).copied())
                                .unwrap_or(0);
                            null_binding.insert_binding(
                                var.clone(),
                                BoundValue::Null {
                                    column_count: col_count,
                                },
                            );
                        }
                    }
                    if let Some(ref var) = pattern.path_variable {
                        null_binding
                            .insert_binding(var.clone(), BoundValue::Null { column_count: 1 });
                    }
                }
                ensure_graph_result_row_capacity(context, result_bindings.len())?;
                context.track_memory(estimate_binding_row_bytes(&null_binding))?;
                result_bindings.push(null_binding);
            } else {
                for binding in current_bindings {
                    ensure_graph_result_row_capacity(context, result_bindings.len())?;
                    context.track_memory(estimate_binding_row_bytes(&binding))?;
                    result_bindings.push(binding);
                }
            }
        }

        Ok(result_bindings)
    }

    /// Promote an inline node property predicate `(a:Person
    /// {id: 50})` into an `IndexScanInfo` when the property maps
    /// onto a btree index leading column AND the value is a
    /// literal. Without this hint the matcher fell through to a
    /// full SeqScan of the backing table for the constrained node
    /// — the dominant overhead in multi-hop patterns whose only
    /// constraint sits in the middle of the chain (e.g.
    /// `(x)-->(a {id:50})-->(b)`: the planner correctly enumerates
    /// `a` via the PK index after this hint, vs. scanning all
    /// 10k `x`'s in the previous left-to-right walk).
    fn apply_inline_property_index_hints(
        &self,
        context: &ExecutionContext,
        pattern: &CypherPattern,
    ) -> DbResult<CypherPattern> {
        let mut hinted = pattern.clone();
        for node in &mut hinted.nodes {
            if node.index_scan.is_some() {
                continue;
            }
            let Some(table_id) = node.table_id else {
                continue;
            };
            if node.properties.is_empty() {
                continue;
            }
            let Some(table) = self
                .catalog_reader
                .get_table_by_id(context.txn_id, table_id)?
            else {
                continue;
            };
            for prop in &node.properties {
                let scan_value = match &prop.value {
                    TypedExpr {
                        kind: TypedExprKind::Literal(value),
                        ..
                    } if !matches!(value, Value::Null) => value.clone(),
                    _ => continue,
                };
                let Some(column_index) = self.find_column_index(&table.columns, &prop.key) else {
                    continue;
                };
                let Some(index_id) =
                    self.find_btree_index_for_column_ordinal(context, table_id, column_index)?
                else {
                    continue;
                };
                node.index_scan = Some(IndexScanInfo {
                    index_id,
                    column_index,
                    scan_value,
                });
                break;
            }
        }
        Ok(hinted)
    }

    fn apply_match_filter_index_hints(
        &self,
        context: &ExecutionContext,
        pattern: &CypherPattern,
        filter: &TypedExpr,
    ) -> DbResult<CypherPattern> {
        let mut hinted = pattern.clone();
        let mut conjuncts = Vec::new();
        collect_graph_filter_conjuncts(filter, &mut conjuncts);

        for node in &mut hinted.nodes {
            if node.index_scan.is_some() {
                continue;
            }
            let (Some(table_id), Some(variable)) = (node.table_id, node.variable.as_deref()) else {
                continue;
            };
            let Some(table) = self
                .catalog_reader
                .get_table_by_id(context.txn_id, table_id)?
            else {
                continue;
            };
            // Collect every literal-equality predicate the WHERE
            // clause exposes for this node so we can either set
            // an `index_scan` (best — uses a btree) or inject the
            // predicate as an inline-style property hint that
            // `scan_node_candidates` then routes through
            // `scan_table_eq_filter` (storage-side count map +
            // Base-table tight loop). Without the second arm,
            // shapes like
            // `MATCH (a:P)-->(b) WHERE a.number = 1` paid for a
            // full SeqScan + per-row predicate eval through the
            // executor's generic ExpressionEvaluator even when the
            // backing storage could push down the equality
            // cheaply.
            let mut chosen_index = false;
            for conjunct in &conjuncts {
                let Some((column_ref, scan_value)) = exact_column_literal_equality(conjunct) else {
                    continue;
                };
                let Some(property) = column_ref
                    .strip_prefix(variable)
                    .and_then(|tail| tail.strip_prefix('.'))
                else {
                    continue;
                };
                let Some(column_index) = self.find_column_index(&table.columns, property) else {
                    continue;
                };
                if !chosen_index {
                    if let Some(index_id) =
                        self.find_btree_index_for_column_ordinal(context, table_id, column_index)?
                    {
                        node.index_scan = Some(IndexScanInfo {
                            index_id,
                            column_index,
                            scan_value: scan_value.clone(),
                        });
                        chosen_index = true;
                        continue;
                    }
                }
                // Non-indexed eq — inject as a property hint if
                // it isn't already there. Skip duplicates so
                // node.properties doesn't grow on repeated planning.
                let already = node
                    .properties
                    .iter()
                    .any(|p| p.key.eq_ignore_ascii_case(property));
                if !already {
                    let value_type = scan_value
                        .data_type()
                        .unwrap_or(aiondb_core::DataType::Text);
                    node.properties
                        .push(aiondb_plan::graph::CypherPropertyExpr {
                            key: property.to_owned(),
                            value: TypedExpr {
                                kind: TypedExprKind::Literal(scan_value),
                                data_type: value_type,
                                nullable: true,
                            },
                        });
                }
            }
            // Range pushdown: walk WHERE conjuncts a SECOND time
            // to harvest comparisons (`<`, `<=`, `>`, `>=`,
            // `BETWEEN`) we couldn't promote to either an
            // index_scan or an inline-eq property hint. Each
            // bound is stored on `node.range_pushdown` so the
            // matcher can route through
            // `scan_table_multi_range_filter` and let storage
            // filter inline at decode time. Lifts shapes like
            // `MATCH (a:Person)-->(b) WHERE a.number < 20` from
            // a full SeqScan + per-row generic filter to a single
            // pushdown call.
            for conjunct in &conjuncts {
                let Some((column_ref, lower, upper)) = extract_column_literal_range(conjunct)
                else {
                    continue;
                };
                let Some(property) = column_ref
                    .strip_prefix(variable)
                    .and_then(|tail| tail.strip_prefix('.'))
                else {
                    continue;
                };
                let Some(column_index) = self.find_column_index(&table.columns, property) else {
                    continue;
                };
                let Some(column) = table.columns.get(column_index) else {
                    continue;
                };
                // Skip duplicates: if we already have a range on
                // the same column (e.g. from another conjunct),
                // intersect by keeping the tighter bound. For
                // simplicity, just append — `multi_range_filter`
                // applies AND semantics anyway.
                node.range_pushdown
                    .push(aiondb_plan::graph::CypherRangePushdown {
                        column_id: column.column_id,
                        lower,
                        upper,
                    });
            }
        }

        Ok(hinted)
    }

    /// Match a single `CypherPattern` (a chain of alternating nodes and
    /// relationships).  For `(a)-[r]->(b)`, nodes = [a, b] and
    /// relationships = [r].
    fn match_pattern(
        &self,
        context: &ExecutionContext,
        pattern: &CypherPattern,
        mut bindings: Vec<BindingRow>,
        filter_conjuncts: &[GraphFilterConjunct<'_>],
    ) -> DbResult<Vec<BindingRow>> {
        for binding in &mut bindings {
            binding.remove("__edge_next_node_id__");
        }

        // Dispatch to shortest-path matching if a path function is present.
        if let Some(ref func) = pattern.path_function {
            return self.match_shortest_path(context, pattern, *func, bindings);
        }

        // Pattern pivot: pick the most-selective node as the
        // matching start so a shape like
        // `(x)-->(a {id: 50})-->(b)` jumps straight to the PK
        // lookup on `a` and expands outwards, instead of
        // full-scanning `x` and joining inward. The previous
        // strict left-to-right walk made every chained pattern
        // pay for an unconstrained-leftmost-node SeqScan even
        // when a later node had a literal-equality constraint
        // on an indexed column.
        //
        // Pivot rules:
        // * `node.index_scan.is_some()` → score 0 (best)
        // * `node.properties non-empty`  → score 1
        // * label-only or no constraint → score 2
        //
        // When the chosen pivot is NOT the leftmost node we
        // rewrite the pattern: walk the LEFT arm in reversed
        // direction (incoming becomes outgoing and vice-versa),
        // then the RIGHT arm in original direction. The pivot is
        // matched once at the start, both arms reuse the bindings
        // produced for it. This is the same transform PG's
        // planner runs when it pulls an inner-join's
        // most-selective relation up to the driver position.
        if let Some(pivot) = pick_match_pivot_index(pattern) {
            return self.match_pattern_pivoted(context, pattern, bindings, filter_conjuncts, pivot);
        }

        for (i, node) in pattern.nodes.iter().enumerate() {
            bindings = self.match_node(context, node, bindings)?;
            bindings =
                self.apply_ready_graph_filter_conjuncts(context, bindings, filter_conjuncts)?;
            if bindings.is_empty() {
                return Ok(bindings);
            }
            // After each node except the last, process the relationship and
            // then the next node will be handled by the next loop iteration.
            if i < pattern.relationships.len() {
                let rel = &pattern.relationships[i];
                let next_node = pattern.nodes.get(i + 1);
                bindings = self.match_relationship(
                    context,
                    node,
                    rel,
                    next_node,
                    bindings,
                    pattern.path_variable.as_deref(),
                )?;
                bindings =
                    self.apply_ready_graph_filter_conjuncts(context, bindings, filter_conjuncts)?;
                if bindings.is_empty() {
                    return Ok(bindings);
                }
            }
        }
        Ok(bindings)
    }

    /// Pivoted match: anchor at `pivot` (the most-selective node
    /// chosen by `pick_match_pivot_index`) and expand left-then-right.
    ///
    /// Left expansion walks `pivot - 1, pivot - 2, …, 0`, processing
    /// the relationship that originally connected `node[i]` to
    /// `node[i+1]` with its direction flipped (Outgoing↔Incoming;
    /// Both stays Both) so the adjacency lookup goes from the
    /// already-bound right side back to the unbound left side.
    /// Right expansion walks `pivot + 1, pivot + 2, …` with the
    /// original relationship direction.
    fn match_pattern_pivoted(
        &self,
        context: &ExecutionContext,
        pattern: &CypherPattern,
        bindings: Vec<BindingRow>,
        filter_conjuncts: &[GraphFilterConjunct<'_>],
        pivot: usize,
    ) -> DbResult<Vec<BindingRow>> {
        let mut bindings = self.match_node(context, &pattern.nodes[pivot], bindings)?;
        bindings = self.apply_ready_graph_filter_conjuncts(context, bindings, filter_conjuncts)?;
        if bindings.is_empty() {
            return Ok(bindings);
        }

        // Left arm: walk from pivot back to index 0. Each
        // relationship[i] connects node[i] (left) to node[i+1]
        // (right). When we walk right→left we treat the
        // "current" as node[i+1] and the "next" as node[i], so
        // the direction must be flipped.
        for left_node_idx in (0..pivot).rev() {
            let rel_idx = left_node_idx;
            let original_rel = &pattern.relationships[rel_idx];
            let flipped_rel = flip_relationship_direction(original_rel);
            let current_node = &pattern.nodes[left_node_idx + 1];
            let next_node = &pattern.nodes[left_node_idx];
            for binding in &mut bindings {
                binding.remove("__edge_next_node_id__");
            }
            bindings = self.match_relationship(
                context,
                current_node,
                &flipped_rel,
                Some(next_node),
                bindings,
                None,
            )?;
            bindings =
                self.apply_ready_graph_filter_conjuncts(context, bindings, filter_conjuncts)?;
            if bindings.is_empty() {
                return Ok(bindings);
            }
            bindings = self.match_node(context, next_node, bindings)?;
            bindings =
                self.apply_ready_graph_filter_conjuncts(context, bindings, filter_conjuncts)?;
            if bindings.is_empty() {
                return Ok(bindings);
            }
        }

        // Right arm: walk from pivot forward to the end with the
        // original relationship direction.
        for right_node_idx in (pivot + 1)..pattern.nodes.len() {
            let rel_idx = right_node_idx - 1;
            let original_rel = &pattern.relationships[rel_idx];
            let current_node = &pattern.nodes[right_node_idx - 1];
            let next_node = &pattern.nodes[right_node_idx];
            for binding in &mut bindings {
                binding.remove("__edge_next_node_id__");
            }
            bindings = self.match_relationship(
                context,
                current_node,
                original_rel,
                Some(next_node),
                bindings,
                None,
            )?;
            bindings =
                self.apply_ready_graph_filter_conjuncts(context, bindings, filter_conjuncts)?;
            if bindings.is_empty() {
                return Ok(bindings);
            }
            bindings = self.match_node(context, next_node, bindings)?;
            bindings =
                self.apply_ready_graph_filter_conjuncts(context, bindings, filter_conjuncts)?;
            if bindings.is_empty() {
                return Ok(bindings);
            }
        }
        Ok(bindings)
    }

    fn apply_ready_graph_filter_conjuncts(
        &self,
        context: &ExecutionContext,
        bindings: Vec<BindingRow>,
        filter_conjuncts: &[GraphFilterConjunct<'_>],
    ) -> DbResult<Vec<BindingRow>> {
        if filter_conjuncts.is_empty() || bindings.is_empty() {
            return Ok(bindings);
        }

        let mut filtered = Vec::with_capacity(bindings.len());
        'binding: for binding in bindings {
            for conjunct in filter_conjuncts {
                if !conjunct.is_ready(&binding) {
                    continue;
                }
                if !self.evaluate_graph_predicate(context, conjunct.expr, &binding)? {
                    continue 'binding;
                }
            }
            filtered.push(binding);
        }
        Ok(filtered)
    }

    /// Execute a shortest-path or all-shortest-paths match.
    ///
    /// The pattern must have exactly two nodes and one relationship (the
    /// typical `(a)-[*..N]->(b)` form).  BFS is used to find the shortest
    /// path(s) between every pair of candidate start/end nodes.  The results
    /// are returned as binding rows containing the start node, end node, and
    /// (if the relationship is named) the first edge on the path.
    fn match_shortest_path(
        &self,
        context: &ExecutionContext,
        pattern: &CypherPattern,
        func: CypherPathFunction,
        input_bindings: Vec<BindingRow>,
    ) -> DbResult<Vec<BindingRow>> {
        // Validate the pattern shape.
        if pattern.nodes.len() != 2 || pattern.relationships.len() != 1 {
            return Err(DbError::internal(
                "shortestPath/allShortestPaths requires exactly two nodes and one relationship",
            ));
        }
        let start_node_pat = &pattern.nodes[0];
        let end_node_pat = &pattern.nodes[1];
        let rel_pat = &pattern.relationships[0];

        let max_depth = rel_pat.max_hops.unwrap_or(15);

        // Resolve edge table id.
        let Some(edge_table_id) = rel_pat.table_id else {
            return Err(DbError::internal(
                "shortestPath requires a typed relationship pattern (e.g. [:KNOWS*])",
            ));
        };

        // Determine source/target column indices in the edge table.
        let edge_table = self
            .catalog_reader
            .get_table_by_id(context.txn_id, edge_table_id)?
            .ok_or_else(|| DbError::internal("edge table not found"))?;
        let edge_col_names: SharedStrings =
            Arc::new(edge_table.columns.iter().map(|c| c.name.clone()).collect());
        let ((source_idx, target_idx), use_table_adjacency) = self
            .resolve_edge_endpoint_columns_for_rel(
                context,
                edge_table_id,
                rel_pat.rel_type.as_deref(),
            )?;

        // First, expand start and end node candidates using normal matching.
        let start_bindings = self.match_node(context, start_node_pat, input_bindings)?;
        // For each start binding, expand end-node candidates.
        // We need all-labels support here so we use a temporary pattern.
        let start_and_end_bindings = self.match_node(context, end_node_pat, start_bindings)?;

        let start_var = start_node_pat.variable.as_deref().unwrap_or("__sp_start__");
        let end_var = end_node_pat.variable.as_deref().unwrap_or("__sp_end__");
        let rel_var = rel_pat.variable.as_deref();
        let rel_type_name: SharedText = Arc::from(rel_pat.rel_type.clone().unwrap_or_default());

        let mut output = Vec::new();

        let mode = PathSearchMode::from_path_function(func);
        let mut edge_endpoint_overrides = HashMap::new();
        if !use_table_adjacency {
            edge_endpoint_overrides.insert(edge_table_id, (source_idx, target_idx));
        }
        let graph_provider = ExecutorGraphRowProvider {
            executor: self,
            context,
            edge_endpoint_overrides,
        };
        let use_storage_backed_shortest_path = matches!(mode, PathSearchMode::SingleShortest);

        for binding in &start_and_end_bindings {
            context.check_deadline()?;

            // Extract start and end node ids.
            let start_id = match binding.get(start_var) {
                Some(BoundValue::Node { id_value, .. }) => id_value.clone(),
                _ => continue,
            };
            let end_id = match binding.get(end_var) {
                Some(BoundValue::Node { id_value, .. }) => id_value.clone(),
                _ => continue,
            };

            // Skip self-loops unless they make sense.
            if start_id == end_id {
                // For shortest path from a node to itself, the path is trivial
                // (just the node). We emit the binding as-is.
                ensure_graph_result_row_capacity(context, output.len())?;
                context.track_memory(estimate_binding_row_bytes(binding))?;
                output.push(binding.clone());
                continue;
            }

            if use_storage_backed_shortest_path {
                let (start_table_id, start_row) = match binding.get(start_var) {
                    Some(BoundValue::Node { table_id, row, .. }) => (*table_id, row.as_ref()),
                    _ => continue,
                };
                let (end_table_id, end_row) = match binding.get(end_var) {
                    Some(BoundValue::Node { table_id, row, .. }) => (*table_id, row.as_ref()),
                    _ => continue,
                };
                let Some(path) = graph_shortest_path(
                    start_table_id,
                    start_row,
                    end_table_id,
                    end_row,
                    edge_table_id,
                    &graph_provider,
                    max_depth,
                )?
                else {
                    continue;
                };

                let mut new_binding = binding.clone();
                if let Some(rv) = rel_var {
                    let first_edge_tuple_id = path.iter().find_map(|element| match element {
                        GraphPathElement::Edge { tuple_id, .. } => Some(*tuple_id),
                        GraphPathElement::Node { .. } => None,
                    });
                    if let Some(tuple_id) = first_edge_tuple_id {
                        let Some(raw_row) = self.storage_dml.fetch(
                            context.txn_id,
                            &context.snapshot,
                            edge_table_id,
                            tuple_id,
                            None,
                        )?
                        else {
                            continue;
                        };
                        let record = aiondb_storage_api::TupleRecord {
                            tuple_id,
                            heap_position: tuple_id.get(),
                            row: raw_row,
                        };
                        let compat_row =
                            self.compat_scan_row_for_table_id(context, edge_table_id, &record)?;
                        new_binding.insert_binding(
                            rv.to_owned(),
                            BoundValue::Edge {
                                table_id: edge_table_id,
                                row: Arc::new(compat_row),
                                raw_row: Arc::new(record.row),
                                tuple_id,
                                rel_type: Arc::clone(&rel_type_name),
                                column_names: Arc::clone(&edge_col_names),
                            },
                        );
                    }
                }
                ensure_graph_result_row_capacity(context, output.len())?;
                context.track_memory(estimate_binding_row_bytes(&new_binding))?;
                output.push(new_binding);
            } else {
                let paths = self.search_paths_bfs_adjacency(
                    context,
                    mode,
                    &start_id,
                    &end_id,
                    edge_table_id,
                    target_idx,
                    &graph_provider,
                    max_depth,
                )?;

                if paths.is_empty() {
                    continue;
                }

                for path_edges in &paths {
                    let mut new_binding = binding.clone();

                    if let Some(rv) = rel_var {
                        if let Some(tuple_id) = path_edges.first() {
                            let Some(raw_row) = self.storage_dml.fetch(
                                context.txn_id,
                                &context.snapshot,
                                edge_table_id,
                                *tuple_id,
                                None,
                            )?
                            else {
                                continue;
                            };
                            let record = aiondb_storage_api::TupleRecord {
                                tuple_id: *tuple_id,
                                heap_position: tuple_id.get(),
                                row: raw_row,
                            };
                            let compat_row =
                                self.compat_scan_row_for_table_id(context, edge_table_id, &record)?;
                            new_binding.insert_binding(
                                rv.to_owned(),
                                BoundValue::Edge {
                                    table_id: edge_table_id,
                                    row: Arc::new(compat_row),
                                    raw_row: Arc::new(record.row),
                                    tuple_id: *tuple_id,
                                    rel_type: Arc::clone(&rel_type_name),
                                    column_names: Arc::clone(&edge_col_names),
                                },
                            );
                        }
                    }

                    ensure_graph_result_row_capacity(context, output.len())?;
                    context.track_memory(estimate_binding_row_bytes(&new_binding))?;
                    output.push(new_binding);
                }
            }
        }

        Ok(output)
    }

    /// BFS shortest-path(s) between two node ids using adjacency expansion.
    ///
    /// Returns a vec of paths, where each path is a vec of edge tuple ids.
    fn search_paths_bfs_adjacency(
        &self,
        context: &ExecutionContext,
        mode: PathSearchMode,
        start_id: &Value,
        end_id: &Value,
        edge_table_id: RelationId,
        target_idx: usize,
        provider: &dyn GraphRowProvider,
        max_depth: u32,
    ) -> DbResult<Vec<Vec<TupleId>>> {
        let all = mode.allows_multiple_shortest_paths();
        let mut queue: VecDeque<(Value, Vec<TupleId>, HashSet<TupleId>)> = VecDeque::new();
        context.track_memory(estimate_shortest_path_queue_entry_bytes(start_id, 0, 0))?;
        ensure_graph_workset_capacity(context, queue.len(), "shortest-path queue")?;
        queue.push_back((start_id.clone(), Vec::new(), HashSet::new()));

        // For the single-shortest variant, track visited nodes to prune.
        // For all-shortest, we track (node, depth) to allow multiple arrivals
        // at the same depth.
        let mut visited: HashMap<ValueHashKey, u32> = HashMap::new();
        if let Some(k) = value_to_bfs_key(start_id) {
            visited.insert(k, 0);
            context.track_memory(
                size_of_u64::<ValueHashKey>()
                    .saturating_add(size_of_u64::<u32>())
                    .saturating_add(16),
            )?;
        }

        let mut found_paths: Vec<Vec<TupleId>> = Vec::new();
        let mut found_depth: Option<u32> = None;

        for _depth in 0..max_depth {
            let frontier_len = queue.len();
            if frontier_len == 0 {
                break;
            }

            // If we already found paths and we're past the shortest depth, stop.
            if let Some(fd) = found_depth {
                if _depth > fd {
                    break;
                }
            }

            for _ in 0..frontier_len {
                let Some((current_val, path, path_set)) = queue.pop_front() else {
                    break;
                };

                let edges = provider.adjacency_lookup_edges(
                    edge_table_id,
                    &current_val,
                    aiondb_graph::traversal::TraversalDirection::Outgoing,
                )?;
                for edge in &edges {
                    context.check_deadline()?;
                    let edge_tgt = edge
                        .row
                        .values
                        .get(target_idx)
                        .cloned()
                        .unwrap_or(Value::Null);

                    // Don't re-use the same edge in a path (O(1) HashSet lookup).
                    if path_set.contains(&edge.tuple_id) {
                        continue;
                    }

                    let mut new_path = path.clone();
                    new_path.push(edge.tuple_id);
                    let mut new_path_set = path_set.clone();
                    new_path_set.insert(edge.tuple_id);

                    // Found the target?
                    if edge_tgt == *end_id {
                        let depth = usize_to_u32(new_path.len());
                        match found_depth {
                            None => {
                                found_depth = Some(depth);
                                ensure_graph_result_row_capacity(context, found_paths.len())?;
                                context.track_memory(estimate_bfs_path_bytes(new_path.len()))?;
                                found_paths.push(new_path);
                                if !all {
                                    return Ok(found_paths);
                                }
                            }
                            Some(fd) if depth == fd => {
                                ensure_graph_result_row_capacity(context, found_paths.len())?;
                                context.track_memory(estimate_bfs_path_bytes(new_path.len()))?;
                                found_paths.push(new_path);
                            }
                            _ => {
                                // Longer than shortest found -- skip.
                            }
                        }
                        continue;
                    }

                    // Node-level cycle detection.
                    if let Some(k) = value_to_bfs_key(&edge_tgt) {
                        if let Some(&prev_depth) = visited.get(&k) {
                            if !all || usize_to_u32(new_path.len()) > prev_depth {
                                continue;
                            }
                        }
                        if visited.insert(k, usize_to_u32(new_path.len())).is_none() {
                            context.track_memory(
                                size_of_u64::<u64>()
                                    .saturating_add(size_of_u64::<u32>())
                                    .saturating_add(16),
                            )?;
                        }
                    }

                    context.track_memory(estimate_shortest_path_queue_entry_bytes(
                        &edge_tgt,
                        new_path.len(),
                        new_path_set.len(),
                    ))?;
                    ensure_graph_workset_capacity(context, queue.len(), "shortest-path queue")?;
                    queue.push_back((edge_tgt, new_path, new_path_set));
                }
            }
        }

        Ok(found_paths)
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

    /// Check if the current node's id matches a given value.
    pub(super) fn current_node_id_matches(
        &self,
        binding: &BindingRow,
        current_node: Option<&CypherNodePattern>,
        value: &Value,
    ) -> bool {
        self.find_current_node_id_for_pattern(binding, current_node)
            .is_some_and(|id| id == *value)
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
