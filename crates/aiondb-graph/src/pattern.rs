//! Graph pattern matching engine.
//!
//! Implements the core logic for matching Cypher-style patterns against
//! node and edge data.  This module is storage-agnostic: it operates on
//! in-memory row iterators provided by the executor.

use std::collections::{HashMap, HashSet};

use aiondb_core::{DbError, DbResult, RelationId, Row, TupleId, Value};

use crate::traversal::TraversalDirection;

// ---------------------------------------------------------------
// Bound values and bindings
// ---------------------------------------------------------------

/// A value bound to a pattern variable.
#[derive(Clone, Debug)]
pub enum BoundValue {
    /// A matched node row.
    Node {
        table_id: RelationId,
        row: Row,
        raw_row: Row,
        id_value: Value,
        tuple_id: TupleId,
        labels: Vec<String>,
        column_names: Vec<String>,
    },
    /// A matched edge row.
    Edge {
        table_id: RelationId,
        row: Row,
        raw_row: Row,
        tuple_id: TupleId,
        rel_type: String,
        column_names: Vec<String>,
    },
    /// A path (sequence of alternating nodes and edges).
    Path(Vec<PathElement>),
    /// Null (for OPTIONAL MATCH with no match).
    Null,
}

/// An element within a path result.
#[derive(Clone, Debug)]
pub enum PathElement {
    Node {
        table_id: RelationId,
        row: Row,
    },
    Edge {
        table_id: RelationId,
        row: Row,
        tuple_id: TupleId,
    },
}

/// An adjacent edge returned by a [`RowProvider`].
#[derive(Clone, Debug)]
pub struct AdjacentEdge {
    pub row: Row,
    pub tuple_id: TupleId,
}

/// A matched binding: variable name -> bound value.
#[derive(Clone, Debug)]
pub struct Binding {
    entries: HashMap<String, BoundValue>,
    bind_order: Vec<String>,
}

impl Binding {
    /// Create an empty binding.
    #[must_use]
    pub fn new() -> Self {
        Self {
            entries: HashMap::new(),
            bind_order: Vec::new(),
        }
    }

    /// Bind a variable to a value.
    pub fn bind(&mut self, variable: String, value: BoundValue) {
        if !self.entries.contains_key(&variable) {
            self.bind_order.push(variable.clone());
        }
        self.entries.insert(variable, value);
    }

    /// Look up a variable.
    #[must_use]
    pub fn get(&self, variable: &str) -> Option<&BoundValue> {
        self.entries.get(variable)
    }

    /// Return `true` if the variable is already bound.
    #[must_use]
    pub fn contains(&self, variable: &str) -> bool {
        self.entries.contains_key(variable)
    }

    /// Merge two bindings.  Entries in `other` overwrite entries in `self`
    /// if there is a key collision.
    #[must_use]
    pub fn merge(&self, other: &Binding) -> Binding {
        let mut merged = self.clone();
        for variable in &other.bind_order {
            if let Some(value) = other.entries.get(variable) {
                merged.bind(variable.clone(), value.clone());
            }
        }
        merged
    }

    /// Iterate over all bound variable names.
    pub fn variables(&self) -> impl Iterator<Item = &str> {
        self.bind_order.iter().map(String::as_str)
    }
}

impl Default for Binding {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------
// Pattern specifications
// ---------------------------------------------------------------

/// Specification for a node match operation.
#[derive(Clone, Debug)]
pub struct NodeMatchSpec {
    /// Optional variable name to bind the matched node to.
    pub variable: Option<String>,
    /// Optional label filter.
    pub label: Option<String>,
    /// The backing table id (required for scanning).
    pub table_id: Option<RelationId>,
}

/// Specification for a relationship match operation.
#[derive(Clone, Debug)]
pub struct RelMatchSpec {
    /// Optional variable name to bind the matched edge to.
    pub variable: Option<String>,
    /// Optional label filter.
    pub label: Option<String>,
    /// The backing table id (required for scanning).
    pub table_id: Option<RelationId>,
    /// The traversal direction.
    pub direction: TraversalDirection,
    /// Minimum number of hops.
    pub min_hops: u32,
    /// Maximum number of hops.
    pub max_hops: u32,
}

/// A single step in a pattern: either a node scan or a relationship
/// traversal.
#[derive(Clone, Debug)]
pub enum PatternStep {
    ScanNode(NodeMatchSpec),
    TraverseRel(RelMatchSpec),
}

/// A full pattern to match: a sequence of alternating node / relationship
/// steps starting and ending with a node.
#[derive(Clone, Debug)]
pub struct MatchPattern {
    pub steps: Vec<PatternStep>,
}

impl MatchPattern {
    /// Validate the structural integrity of the pattern.
    ///
    /// Rules:
    /// - It must start and end with a `ScanNode` step.
    /// - Node and relationship steps must strictly alternate.
    pub fn validate(&self) -> DbResult<()> {
        if self.steps.is_empty() {
            return Err(DbError::internal("match pattern must not be empty"));
        }

        // Must start with a node.
        if !matches!(self.steps[0], PatternStep::ScanNode(_)) {
            return Err(DbError::internal(
                "match pattern must start with a node step",
            ));
        }

        // Must end with a node.
        if !matches!(self.steps.last(), Some(PatternStep::ScanNode(_))) {
            return Err(DbError::internal("match pattern must end with a node step"));
        }

        // Alternating check.
        let mut expect_node = true;
        for (i, step) in self.steps.iter().enumerate() {
            let is_node = matches!(step, PatternStep::ScanNode(_));
            if is_node != expect_node {
                return Err(DbError::internal(format!(
                    "match pattern step {i} has unexpected kind (expected {})",
                    if expect_node { "node" } else { "relationship" },
                )));
            }
            expect_node = !expect_node;
        }

        Ok(())
    }

    /// Return every `table_id` referenced by the pattern (for privilege
    /// checking, plan cost estimation, etc.).
    #[must_use]
    pub fn referenced_tables(&self) -> Vec<RelationId> {
        let mut ids = Vec::new();
        for step in &self.steps {
            match step {
                PatternStep::ScanNode(spec) => {
                    if let Some(id) = spec.table_id {
                        ids.push(id);
                    }
                }
                PatternStep::TraverseRel(spec) => {
                    if let Some(id) = spec.table_id {
                        ids.push(id);
                    }
                }
            }
        }
        // `Vec::dedup` only collapses consecutive duplicates; a pattern
        // like `(:A)-[]->(:B)-[]->(:A)` would otherwise leave A twice in
        // the unique-relation list. Sort first so dedup catches every dup.
        ids.sort_by_key(|id| id.get());
        ids.dedup();
        ids
    }
}

// ---------------------------------------------------------------
// Result type
// ---------------------------------------------------------------

/// The result of matching a pattern against data.
#[derive(Clone, Debug)]
pub struct MatchResult {
    pub bindings: Vec<Binding>,
}

// ---------------------------------------------------------------
// Row provider trait
// ---------------------------------------------------------------

/// Trait for providing row data to the pattern matcher.
///
/// The executor implements this to bridge storage and pattern matching.
pub trait RowProvider {
    /// Scan all rows for a table.
    fn scan_table(&self, table_id: RelationId) -> DbResult<Vec<Row>>;

    /// Get the column index for a given name in a table.
    fn column_index(&self, table_id: RelationId, column: &str) -> DbResult<Option<usize>>;

    /// Get all column names for a table.
    fn column_names(&self, table_id: RelationId) -> DbResult<Vec<String>>;

    /// Look up edges adjacent to a given node in the specified direction.
    ///
    /// Returns all edge rows from `edge_table_id` where:
    /// - `Outgoing`: the source column matches `node_id`
    /// - `Incoming`: the target column matches `node_id`
    /// - `Both`: either the source or target column matches `node_id`
    ///
    /// The default implementation performs a full `scan_table` and filters,
    /// but storage-aware implementations can use index lookups for better
    /// performance on large graphs.
    fn adjacency_lookup(
        &self,
        edge_table_id: RelationId,
        node_id: &Value,
        direction: TraversalDirection,
    ) -> DbResult<Vec<Row>> {
        let all_edges = self.scan_table(edge_table_id)?;
        let src_idx = self
            .column_index(edge_table_id, SOURCE_ID_COLUMN)?
            .ok_or_else(|| {
                DbError::internal(format!(
                    "edge table {} missing {SOURCE_ID_COLUMN} column",
                    edge_table_id.get()
                ))
            })?;
        let tgt_idx = self
            .column_index(edge_table_id, TARGET_ID_COLUMN)?
            .ok_or_else(|| {
                DbError::internal(format!(
                    "edge table {} missing {TARGET_ID_COLUMN} column",
                    edge_table_id.get()
                ))
            })?;

        let mut result = Vec::new();
        let null_default = Value::Null;
        for edge in &all_edges {
            let src = edge.values.get(src_idx).unwrap_or(&null_default);
            let tgt = edge.values.get(tgt_idx).unwrap_or(&null_default);
            let matches = match direction {
                TraversalDirection::Outgoing => src == node_id,
                TraversalDirection::Incoming => tgt == node_id,
                TraversalDirection::Both => src == node_id || tgt == node_id,
            };
            if matches {
                if result.len() >= MAX_GRAPH_SEED_EXPANSIONS_PER_STATE {
                    return Err(DbError::program_limit(format!(
                        "graph adjacency lookup exceeded maximum of {MAX_GRAPH_SEED_EXPANSIONS_PER_STATE} rows"
                    )));
                }
                result.push(AdjacentEdge {
                    row: edge.clone(),
                    tuple_id: TupleId::new(0),
                });
            }
        }
        Ok(result.into_iter().map(|edge| edge.row).collect())
    }

    /// Look up adjacent edges with their storage identity when available.
    ///
    /// The default implementation preserves compatibility by delegating to
    /// [`RowProvider::adjacency_lookup`] and synthesizing `TupleId(0)`.
    fn adjacency_lookup_edges(
        &self,
        edge_table_id: RelationId,
        node_id: &Value,
        direction: TraversalDirection,
    ) -> DbResult<Vec<AdjacentEdge>> {
        Ok(self
            .adjacency_lookup(edge_table_id, node_id, direction)?
            .into_iter()
            .map(|row| AdjacentEdge {
                row,
                tuple_id: TupleId::new(0),
            })
            .collect())
    }
}

// ---------------------------------------------------------------
// Core matching logic
// ---------------------------------------------------------------

/// The well-known column name for edge source ids.
pub const SOURCE_ID_COLUMN: &str = "source_id";
/// The well-known column name for edge target ids.
pub const TARGET_ID_COLUMN: &str = "target_id";

/// Core pattern matching function.
///
/// Takes a pattern and a row provider, returns all matching bindings.
pub fn match_pattern(pattern: &MatchPattern, provider: &dyn RowProvider) -> DbResult<MatchResult> {
    pattern.validate()?;

    let mut current_bindings = vec![Binding::new()];

    for step in &pattern.steps {
        current_bindings = match step {
            PatternStep::ScanNode(spec) => expand_node_scan(spec, provider, current_bindings)?,
            PatternStep::TraverseRel(spec) => {
                expand_rel_traverse(spec, provider, current_bindings)?
            }
        };

        if current_bindings.is_empty() {
            break;
        }
    }

    Ok(MatchResult {
        bindings: current_bindings,
    })
}

/// Perform OPTIONAL MATCH: if no matches, return bindings with NULLs for
/// all newly-introduced variables.
pub fn optional_match_pattern(
    pattern: &MatchPattern,
    provider: &dyn RowProvider,
    input_bindings: Vec<Binding>,
) -> DbResult<Vec<Binding>> {
    let result = match_pattern(pattern, provider)?;
    if result.bindings.is_empty() {
        // Collect variables introduced by the pattern.
        let null_vars: Vec<String> = pattern
            .steps
            .iter()
            .filter_map(|s| match s {
                PatternStep::ScanNode(n) => n.variable.clone(),
                PatternStep::TraverseRel(r) => r.variable.clone(),
            })
            .collect();

        let mut null_bindings = input_bindings;
        for binding in &mut null_bindings {
            for var in &null_vars {
                if !binding.contains(var) {
                    binding.bind(var.clone(), BoundValue::Null);
                }
            }
        }
        Ok(null_bindings)
    } else {
        Ok(result.bindings)
    }
}

// ---------------------------------------------------------------
// Expand helpers
// ---------------------------------------------------------------

/// Expand bindings by scanning nodes matching the spec.
pub(crate) fn expand_node_scan(
    spec: &NodeMatchSpec,
    provider: &dyn RowProvider,
    input_bindings: Vec<Binding>,
) -> DbResult<Vec<Binding>> {
    // If the variable is already bound in a binding, we just keep it
    // (identity check -- the node is already determined).
    if let Some(ref var) = spec.variable {
        let all_bound = input_bindings.iter().all(|b| b.contains(var));
        if all_bound && !input_bindings.is_empty() {
            return Ok(input_bindings);
        }
    }

    let Some(table_id) = spec.table_id else {
        // No table id means we cannot scan -- return empty.
        return Ok(Vec::new());
    };

    let rows = provider.scan_table(table_id)?;

    let mut output = Vec::new();
    for binding in &input_bindings {
        for row in &rows {
            let mut new_binding = binding.clone();
            if let Some(ref var) = spec.variable {
                new_binding.bind(
                    var.clone(),
                    BoundValue::Node {
                        table_id,
                        row: row.clone(),
                        raw_row: row.clone(),
                        id_value: row.values.first().cloned().unwrap_or(Value::Null),
                        tuple_id: TupleId::new(0),
                        labels: spec.label.iter().cloned().collect(),
                        column_names: Vec::new(),
                    },
                );
            }
            push_binding_with_limit(&mut output, new_binding, "graph node expansion")?;
        }
    }

    Ok(output)
}

/// Expand bindings by traversing relationships.
pub(crate) fn expand_rel_traverse(
    spec: &RelMatchSpec,
    provider: &dyn RowProvider,
    input_bindings: Vec<Binding>,
) -> DbResult<Vec<Binding>> {
    if spec.min_hops == 1 && spec.max_hops == 1 {
        expand_single_hop(spec, provider, input_bindings)
    } else {
        expand_variable_length(spec, provider, input_bindings)
    }
}

/// Single-hop relationship traversal with storage-backed adjacency lookup.
///
/// When the current node is already bound we ask the provider for only the
/// adjacent edges. If traversal starts from an unbound node we keep the
/// existing full-scan fallback.
fn expand_single_hop(
    spec: &RelMatchSpec,
    provider: &dyn RowProvider,
    input_bindings: Vec<Binding>,
) -> DbResult<Vec<Binding>> {
    let Some(table_id) = spec.table_id else {
        return Ok(Vec::new());
    };

    let mut output = Vec::new();

    for binding in &input_bindings {
        let current_node_id = last_bound_node_id(binding);

        if let Some(ref node_val) = current_node_id {
            let edge_rows = provider.adjacency_lookup_edges(table_id, node_val, spec.direction)?;
            for edge in &edge_rows {
                let mut new_binding = binding.clone();
                if let Some(ref var) = spec.variable {
                    new_binding.bind(
                        var.clone(),
                        BoundValue::Edge {
                            table_id,
                            row: edge.row.clone(),
                            raw_row: edge.row.clone(),
                            tuple_id: edge.tuple_id,
                            rel_type: spec.label.clone().unwrap_or_default(),
                            column_names: Vec::new(),
                        },
                    );
                }
                push_binding_with_limit(
                    &mut output,
                    new_binding,
                    "graph single-hop relationship expansion",
                )?;
            }
        } else {
            let edge_rows = scan_all_edges_with_limit(provider, table_id, "single-hop traversal")?;
            for edge_row in &edge_rows {
                let mut new_binding = binding.clone();
                if let Some(ref var) = spec.variable {
                    new_binding.bind(
                        var.clone(),
                        BoundValue::Edge {
                            table_id,
                            row: edge_row.clone(),
                            raw_row: edge_row.clone(),
                            tuple_id: TupleId::new(0),
                            rel_type: spec.label.clone().unwrap_or_default(),
                            column_names: Vec::new(),
                        },
                    );
                }
                push_binding_with_limit(
                    &mut output,
                    new_binding,
                    "graph single-hop relationship expansion",
                )?;
            }
        }
    }

    Ok(output)
}

/// Safety limit for unbounded variable-length patterns (`*` with no max).
/// Prevents infinite expansion on cyclic graphs.
const DEFAULT_MAX_HOPS_LIMIT: u32 = 100;
/// Hard cap for graph binding cardinality materialized in-memory.
const MAX_GRAPH_BINDINGS: usize = 200_000;
/// Hard cap for per-depth frontier state count in variable-length traversal.
const MAX_GRAPH_FRONTIER_STATES: usize = 100_000;
/// Hard cap for path element count in a single materialized path.
const MAX_GRAPH_PATH_ELEMENTS: usize = 2_048;
/// Hard cap for distinct traversed edges tracked per path state.
const MAX_GRAPH_VISITED_EDGES: usize = 2_048;
/// Hard cap for synthetic neighbor expansions when starting from an unbound node.
const MAX_GRAPH_SEED_EXPANSIONS_PER_STATE: usize = 200_000;
/// Hard cap on edge rows tolerated for full-scan fallback edge expansion.
const MAX_GRAPH_EDGE_ROWS_FOR_IN_MEMORY_EXPANSION: usize = 500_000;

#[derive(Clone)]
struct BoundNodeSeed {
    table_id: RelationId,
    row: Row,
    id_value: Value,
}

/// Variable-length relationship traversal with cycle detection and
/// storage-backed adjacency expansion.
///
/// Uses per-node adjacency lookups whenever a frontier node is bound, so
/// each expansion step only examines its actual neighbors. When traversal
/// starts from an unbound node we keep the existing full-scan fallback.
///
/// Performs iterative BFS expansion from depth 1 to `max_hops`, collecting
/// bindings at each depth >= `min_hops`.  Edge-level visited sets prevent
/// re-traversing the same edge within a single path (Cypher semantics:
/// edges are unique per path, but nodes may be revisited).
///
/// For zero-length patterns (`min_hops == 0`), the identity binding (the
/// start node with an empty path) is included.
fn expand_variable_length(
    spec: &RelMatchSpec,
    provider: &dyn RowProvider,
    input_bindings: Vec<Binding>,
) -> DbResult<Vec<Binding>> {
    let Some(table_id) = spec.table_id else {
        return Ok(Vec::new());
    };

    let src_idx = provider
        .column_index(table_id, SOURCE_ID_COLUMN)?
        .ok_or_else(|| {
            DbError::internal(format!(
                "edge table {} missing {SOURCE_ID_COLUMN} column",
                table_id.get()
            ))
        })?;
    let tgt_idx = provider
        .column_index(table_id, TARGET_ID_COLUMN)?
        .ok_or_else(|| {
            DbError::internal(format!(
                "edge table {} missing {TARGET_ID_COLUMN} column",
                table_id.get()
            ))
        })?;

    // `max_hops == 0` means "zero-length only" (e.g. `*0..0`), distinct from
    // as "unbounded" - bound the upper bound at DEFAULT_MAX_HOPS_LIMIT but
    // honour an explicit zero cap.
    let effective_max = if spec.max_hops > DEFAULT_MAX_HOPS_LIMIT {
        DEFAULT_MAX_HOPS_LIMIT
    } else {
        spec.max_hops
    };

    let mut result = Vec::new();
    let mut fallback_edge_rows: Option<Vec<Row>> = None;

    for binding in &input_bindings {
        let start_node = last_bound_node(binding);

        // Handle zero-length pattern: emit the identity binding (start
        // node with an empty path).
        if spec.min_hops == 0 {
            let mut zero_binding = binding.clone();
            if let Some(ref var) = spec.variable {
                zero_binding.bind(var.clone(), BoundValue::Path(Vec::new()));
            }
            push_binding_with_limit(&mut result, zero_binding, "graph variable-length traversal")?;
        }

        // BFS frontier: (current_node_value, node_table_id, path_so_far, visited_edge_keys).
        let mut frontier: Vec<(Option<Value>, RelationId, Vec<PathElement>, HashSet<String>)> =
            Vec::new();
        match start_node {
            Some(node) => frontier.push((
                Some(node.id_value),
                node.table_id,
                vec![PathElement::Node {
                    table_id: node.table_id,
                    row: node.row,
                }],
                HashSet::new(),
            )),
            None => frontier.push((None, RelationId::new(0), Vec::new(), HashSet::new())),
        }

        for depth in 1..=effective_max {
            let mut next_frontier = Vec::new();

            for (current_val, node_table_id, path, visited) in &frontier {
                if let Some(node_val) = current_val {
                    let edge_rows =
                        provider.adjacency_lookup_edges(table_id, node_val, spec.direction)?;
                    for edge in &edge_rows {
                        let src = edge.row.values.get(src_idx).cloned().unwrap_or(Value::Null);
                        let tgt = edge.row.values.get(tgt_idx).cloned().unwrap_or(Value::Null);
                        let edge_key = edge_identity_key(edge.tuple_id, &edge.row);

                        match spec.direction {
                            TraversalDirection::Outgoing => {
                                if src == *node_val {
                                    push_variable_length_state(
                                        binding,
                                        spec,
                                        table_id,
                                        depth,
                                        *node_table_id,
                                        &edge.row,
                                        edge.tuple_id,
                                        edge_key.as_str(),
                                        src,
                                        tgt,
                                        path,
                                        visited,
                                        &mut result,
                                        &mut next_frontier,
                                    )?;
                                }
                            }
                            TraversalDirection::Incoming => {
                                if tgt == *node_val {
                                    push_variable_length_state(
                                        binding,
                                        spec,
                                        table_id,
                                        depth,
                                        *node_table_id,
                                        &edge.row,
                                        edge.tuple_id,
                                        edge_key.as_str(),
                                        tgt,
                                        src,
                                        path,
                                        visited,
                                        &mut result,
                                        &mut next_frontier,
                                    )?;
                                }
                            }
                            TraversalDirection::Both => {
                                if src == *node_val {
                                    push_variable_length_state(
                                        binding,
                                        spec,
                                        table_id,
                                        depth,
                                        *node_table_id,
                                        &edge.row,
                                        edge.tuple_id,
                                        edge_key.as_str(),
                                        src.clone(),
                                        tgt.clone(),
                                        path,
                                        visited,
                                        &mut result,
                                        &mut next_frontier,
                                    )?;
                                }
                                if tgt == *node_val && src != tgt {
                                    push_variable_length_state(
                                        binding,
                                        spec,
                                        table_id,
                                        depth,
                                        *node_table_id,
                                        &edge.row,
                                        edge.tuple_id,
                                        edge_key.as_str(),
                                        tgt,
                                        src,
                                        path,
                                        visited,
                                        &mut result,
                                        &mut next_frontier,
                                    )?;
                                }
                            }
                        }
                    }
                } else {
                    let edge_rows = fallback_edge_rows.get_or_insert(scan_all_edges_with_limit(
                        provider,
                        table_id,
                        "variable-length traversal",
                    )?);
                    let mut synthetic_neighbor_count = 0usize;
                    for edge_row in edge_rows.iter() {
                        let src = edge_row.values.get(src_idx).cloned().unwrap_or(Value::Null);
                        let tgt = edge_row.values.get(tgt_idx).cloned().unwrap_or(Value::Null);
                        let edge_key = edge_identity_key(TupleId::new(0), edge_row);

                        match spec.direction {
                            TraversalDirection::Outgoing => {
                                synthetic_neighbor_count =
                                    synthetic_neighbor_count.saturating_add(1);
                                if synthetic_neighbor_count > MAX_GRAPH_SEED_EXPANSIONS_PER_STATE {
                                    return Err(DbError::program_limit(format!(
                                        "graph seed expansion exceeded maximum of {MAX_GRAPH_SEED_EXPANSIONS_PER_STATE} neighbors"
                                    )));
                                }
                                push_variable_length_state(
                                    binding,
                                    spec,
                                    table_id,
                                    depth,
                                    *node_table_id,
                                    edge_row,
                                    TupleId::new(0),
                                    edge_key.as_str(),
                                    src,
                                    tgt,
                                    path,
                                    visited,
                                    &mut result,
                                    &mut next_frontier,
                                )?;
                            }
                            TraversalDirection::Incoming => {
                                synthetic_neighbor_count =
                                    synthetic_neighbor_count.saturating_add(1);
                                if synthetic_neighbor_count > MAX_GRAPH_SEED_EXPANSIONS_PER_STATE {
                                    return Err(DbError::program_limit(format!(
                                        "graph seed expansion exceeded maximum of {MAX_GRAPH_SEED_EXPANSIONS_PER_STATE} neighbors"
                                    )));
                                }
                                push_variable_length_state(
                                    binding,
                                    spec,
                                    table_id,
                                    depth,
                                    *node_table_id,
                                    edge_row,
                                    TupleId::new(0),
                                    edge_key.as_str(),
                                    tgt,
                                    src,
                                    path,
                                    visited,
                                    &mut result,
                                    &mut next_frontier,
                                )?;
                            }
                            TraversalDirection::Both => {
                                synthetic_neighbor_count =
                                    synthetic_neighbor_count.saturating_add(1);
                                if synthetic_neighbor_count > MAX_GRAPH_SEED_EXPANSIONS_PER_STATE {
                                    return Err(DbError::program_limit(format!(
                                        "graph seed expansion exceeded maximum of {MAX_GRAPH_SEED_EXPANSIONS_PER_STATE} neighbors"
                                    )));
                                }
                                push_variable_length_state(
                                    binding,
                                    spec,
                                    table_id,
                                    depth,
                                    *node_table_id,
                                    edge_row,
                                    TupleId::new(0),
                                    edge_key.as_str(),
                                    src.clone(),
                                    tgt.clone(),
                                    path,
                                    visited,
                                    &mut result,
                                    &mut next_frontier,
                                )?;
                                if src != tgt {
                                    synthetic_neighbor_count =
                                        synthetic_neighbor_count.saturating_add(1);
                                    if synthetic_neighbor_count
                                        > MAX_GRAPH_SEED_EXPANSIONS_PER_STATE
                                    {
                                        return Err(DbError::program_limit(format!(
                                            "graph seed expansion exceeded maximum of {MAX_GRAPH_SEED_EXPANSIONS_PER_STATE} neighbors"
                                        )));
                                    }
                                    push_variable_length_state(
                                        binding,
                                        spec,
                                        table_id,
                                        depth,
                                        *node_table_id,
                                        edge_row,
                                        TupleId::new(0),
                                        edge_key.as_str(),
                                        tgt,
                                        src,
                                        path,
                                        visited,
                                        &mut result,
                                        &mut next_frontier,
                                    )?;
                                }
                            }
                        }
                    }
                }
            }

            frontier = next_frontier;
            if frontier.is_empty() {
                break;
            }
        }
    }

    Ok(result)
}

/// Extract the id value of the most recently bound node in a binding.
///
/// Looks at all bound values and returns the first column value of the
/// last-bound node row (convention: the first column is the identity key).
fn last_bound_node(binding: &Binding) -> Option<BoundNodeSeed> {
    for variable in binding.bind_order.iter().rev() {
        let Some(value) = binding.entries.get(variable) else {
            continue;
        };
        if let BoundValue::Node {
            table_id,
            row,
            id_value,
            ..
        } = value
        {
            return Some(BoundNodeSeed {
                table_id: *table_id,
                row: row.clone(),
                id_value: id_value.clone(),
            });
        }
    }
    None
}

fn last_bound_node_id(binding: &Binding) -> Option<Value> {
    last_bound_node(binding).map(|node| node.id_value)
}

fn push_binding_with_limit(
    output: &mut Vec<Binding>,
    binding: Binding,
    context: &str,
) -> DbResult<()> {
    if output.len() >= MAX_GRAPH_BINDINGS {
        return Err(DbError::program_limit(format!(
            "{context} exceeded maximum binding count ({MAX_GRAPH_BINDINGS})"
        )));
    }
    output.push(binding);
    Ok(())
}

fn scan_all_edges_with_limit(
    provider: &dyn RowProvider,
    table_id: RelationId,
    context: &str,
) -> DbResult<Vec<Row>> {
    let edge_rows = provider.scan_table(table_id)?;
    if edge_rows.len() > MAX_GRAPH_EDGE_ROWS_FOR_IN_MEMORY_EXPANSION {
        return Err(DbError::program_limit(format!(
            "graph {context} requires scanning too many edges ({})",
            edge_rows.len()
        )));
    }
    Ok(edge_rows)
}

fn edge_identity_key(tuple_id: TupleId, edge_row: &Row) -> String {
    if tuple_id.get() != 0 {
        format!("tid:{}", tuple_id.get())
    } else {
        format!("row:{edge_row:?}")
    }
}

fn push_variable_length_state(
    binding: &Binding,
    spec: &RelMatchSpec,
    edge_table_id: RelationId,
    depth: u32,
    node_table_id: RelationId,
    edge_row: &Row,
    edge_tuple_id: TupleId,
    edge_key: &str,
    start_value: Value,
    next_value: Value,
    path: &[PathElement],
    visited: &HashSet<String>,
    result: &mut Vec<Binding>,
    next_frontier: &mut Vec<(Option<Value>, RelationId, Vec<PathElement>, HashSet<String>)>,
) -> DbResult<()> {
    if visited.contains(edge_key) {
        return Ok(());
    }
    if visited.len() >= MAX_GRAPH_VISITED_EDGES {
        return Err(DbError::program_limit(format!(
            "graph traversal visited-edge limit reached ({MAX_GRAPH_VISITED_EDGES})"
        )));
    }
    let additional_path_elements = if path.is_empty() { 3 } else { 2 };
    if path.len().saturating_add(additional_path_elements) > MAX_GRAPH_PATH_ELEMENTS {
        return Err(DbError::program_limit(format!(
            "graph traversal path length limit reached ({MAX_GRAPH_PATH_ELEMENTS})"
        )));
    }
    let mut new_path = path.to_vec();
    if new_path.is_empty() {
        new_path.push(PathElement::Node {
            table_id: node_table_id,
            row: Row::new(vec![start_value]),
        });
    }
    new_path.push(PathElement::Edge {
        table_id: edge_table_id,
        row: edge_row.clone(),
        tuple_id: edge_tuple_id,
    });
    new_path.push(PathElement::Node {
        table_id: node_table_id,
        row: Row::new(vec![next_value.clone()]),
    });

    let mut new_visited = visited.clone();
    new_visited.insert(edge_key.to_owned());
    if new_visited.len() > MAX_GRAPH_VISITED_EDGES {
        return Err(DbError::program_limit(format!(
            "graph traversal visited-edge limit reached ({MAX_GRAPH_VISITED_EDGES})"
        )));
    }

    if depth >= spec.min_hops {
        let mut new_binding = binding.clone();
        if let Some(ref var) = spec.variable {
            new_binding.bind(var.clone(), BoundValue::Path(new_path.clone()));
        }
        push_binding_with_limit(result, new_binding, "graph variable-length traversal")?;
    }

    if next_frontier.len() >= MAX_GRAPH_FRONTIER_STATES {
        return Err(DbError::program_limit(format!(
            "graph traversal frontier exceeded maximum of {MAX_GRAPH_FRONTIER_STATES} states"
        )));
    }
    next_frontier.push((Some(next_value), node_table_id, new_path, new_visited));
    Ok(())
}

// ---------------------------------------------------------------
// Tests
// ---------------------------------------------------------------
