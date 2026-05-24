//! Graph-specific query optimizer for Cypher patterns.
//!
//! Optimizes `CypherQueryPlan` plans by:
//! - Reordering patterns within a MATCH clause to start with the most selective
//!   label (fewest rows).
//! - Pushing WHERE predicates into pattern inline property filters when they
//!   reference a single node/edge variable.
//! - Tightening variable-length relationship bounds when WHERE clauses constrain
//!   path length.
//!
//! The optimizer is invoked from `Optimizer::optimize` when the logical plan is
//! a `LogicalPlan::CypherQuery`.

use std::collections::HashMap;

use aiondb_graph_cbo::{
    plan_query_graph, ExpansionPlan, GraphStatistics, IndexKind, PhysicalOp, PlannerConfig,
    PredicateOp, PropertyPredicate, QueryGraph, QueryNode, QueryRel, RelDirection,
};
use aiondb_plan::graph::{
    CypherMatchClause, CypherNodePattern, CypherPattern, CypherPipelineOp, CypherPropertyExpr,
    CypherQueryPlan, CypherRelDirection, CypherRelPattern,
};
use aiondb_plan::TypedExpr;

use crate::u64_to_f64;

// ---------------------------------------------------------------------------
// Graph statistics
// ---------------------------------------------------------------------------

/// Statistics about the graph topology used for cost-based optimization.
///
/// Every field is sourced from statistics that are already persisted by
/// `ANALYZE` on the backing SQL tables (see `aiondb-catalog`'s
/// `TableStatistics` / `ColumnStatistics`, recovered from the catalog WAL).
/// Nothing here is a separate, unreliable side-channel: the graph adapter
/// just re-projects table/column facts into graph terms so the cost-based
/// planner gets real numbers instead of constants.
#[derive(Clone, Debug, Default)]
pub struct GraphStats {
    /// Number of nodes per label.
    pub label_cardinality: HashMap<String, u64>,
    /// Number of edges per relationship type.
    pub edge_cardinality: HashMap<String, u64>,
    /// Average outgoing degree per edge type (edges / distinct source nodes).
    pub avg_out_degree: HashMap<String, f64>,
    /// Average incoming degree per edge type (edges / distinct target nodes).
    pub avg_in_degree: HashMap<String, f64>,
    /// Distinct value count per `(label, property)`, taken from the backing
    /// node table's persisted per-column `ndistinct`. Drives equality
    /// selectivity in the cost model instead of a generic constant.
    pub distinct: HashMap<(String, String), f64>,
    /// Declared `(source_label, target_label)` endpoint labels per edge type,
    /// from the edge label descriptor. In this label-backed model an edge
    /// type connects exactly one label pair, so the typed-triple count
    /// `count((:A)-[:T]->(:B))` is the full edge-table row count when the
    /// queried labels match the declared endpoints, and zero otherwise.
    pub edge_endpoints: HashMap<String, (String, String)>,
}

impl GraphStats {
    /// Create an empty stats instance (all lookups return defaults).
    pub fn empty() -> Self {
        Self::default()
    }

    /// Return the cardinality of a node label, falling back to a default
    /// estimate when no statistics are available.
    pub fn node_count(&self, label: &str) -> u64 {
        self.label_cardinality
            .get(label)
            .copied()
            .unwrap_or(DEFAULT_NODE_COUNT)
    }

    /// Return the cardinality of an edge type, falling back to a default.
    pub fn edge_count(&self, rel_type: &str) -> u64 {
        self.edge_cardinality
            .get(rel_type)
            .copied()
            .unwrap_or(DEFAULT_EDGE_COUNT)
    }

    /// Return the average outgoing degree for an edge type.
    pub fn out_degree(&self, rel_type: &str) -> f64 {
        self.avg_out_degree
            .get(rel_type)
            .copied()
            .unwrap_or(DEFAULT_DEGREE)
    }

    /// Return the average incoming degree for an edge type.
    pub fn in_degree(&self, rel_type: &str) -> f64 {
        self.avg_in_degree
            .get(rel_type)
            .copied()
            .unwrap_or(DEFAULT_DEGREE)
    }
}

impl GraphStatistics for GraphStats {
    fn total_nodes(&self) -> f64 {
        let total: u64 = self.label_cardinality.values().copied().sum();
        if total == 0 {
            u64_to_f64(DEFAULT_NODE_COUNT)
        } else {
            u64_to_f64(total)
        }
    }

    fn label_cardinality(&self, label: Option<&str>) -> f64 {
        label.map_or_else(
            || self.total_nodes(),
            |label| u64_to_f64(self.node_count(label)),
        )
    }

    fn relationship_cardinality(&self, rel_type: Option<&str>) -> f64 {
        rel_type.map_or_else(
            || u64_to_f64(DEFAULT_EDGE_COUNT),
            |rel_type| u64_to_f64(self.edge_count(rel_type)),
        )
    }

    fn distinct_values(&self, label: Option<&str>, property: &str) -> Option<f64> {
        let label = label?;
        self.distinct
            .get(&(label.to_owned(), property.to_owned()))
            .copied()
    }

    fn triple_cardinality(
        &self,
        from: Option<&str>,
        rel_type: Option<&str>,
        to: Option<&str>,
    ) -> Option<f64> {
        let (from, rel_type, to) = (from?, rel_type?, to?);
        let (src, tgt) = self.edge_endpoints.get(rel_type)?;
        let count = u64_to_f64(*self.edge_cardinality.get(rel_type)?);
        if src.eq_ignore_ascii_case(from) && tgt.eq_ignore_ascii_case(to) {
            Some(count)
        } else {
            // Label-backed model: an edge type links exactly one label pair,
            // so a query that pairs it with a different label genuinely
            // matches no edges.
            Some(0.0)
        }
    }
}

/// Default node count when statistics are unavailable.
const DEFAULT_NODE_COUNT: u64 = 1_000;

/// Default edge count when statistics are unavailable.
const DEFAULT_EDGE_COUNT: u64 = 5_000;

/// Default average degree when statistics are unavailable.
const DEFAULT_DEGREE: f64 = 5.0;

/// Selectivity factor for an inline property filter on a node.
const PROPERTY_FILTER_SELECTIVITY: f64 = 0.1;

// ---------------------------------------------------------------------------
// Cost estimation
// ---------------------------------------------------------------------------

/// Estimate the cost of executing a list of path patterns in order.
///
/// The cost model accounts for:
/// - Node scan cost (proportional to label cardinality)
/// - Edge traversal cost (proportional to degree * source cardinality)
/// - Filter reduction (inline property filters and pushed WHERE predicates
///   reduce the intermediate cardinality)
pub fn estimate_pattern_cost(
    patterns: &[CypherPattern],
    stats: &GraphStats,
    filters: &[TypedExpr],
) -> f64 {
    let mut total_cost = 0.0;
    let mut bound_variables: HashMap<String, f64> = HashMap::new();

    for pattern in patterns {
        let cost = estimate_single_pattern_cost(pattern, stats, filters, &mut bound_variables);
        total_cost += cost;
    }

    total_cost
}

fn estimate_pattern_cost_with_existing_bindings(
    pattern: &CypherPattern,
    stats: &GraphStats,
    filters: &[TypedExpr],
    bound_variables: &HashMap<String, f64>,
) -> f64 {
    let mut bound_variables = bound_variables.clone();
    estimate_single_pattern_cost(pattern, stats, filters, &mut bound_variables)
}

/// Estimate cost for a single path pattern.
fn estimate_single_pattern_cost(
    pattern: &CypherPattern,
    stats: &GraphStats,
    filters: &[TypedExpr],
    bound_variables: &mut HashMap<String, f64>,
) -> f64 {
    if pattern.nodes.is_empty() {
        return 0.0;
    }

    let mut cost = 0.0;

    // Cost of scanning the first node.
    let first_node = &pattern.nodes[0];
    let first_card = node_cardinality(first_node, stats, filters);

    let mut current_card = first_card;

    if let Some(ref var) = first_node.variable {
        if let Some(&existing_card) = bound_variables.get(var) {
            // Variable already bound from a previous pattern -- use the smaller
            // cardinality (intersection semantics).
            let effective = existing_card.min(first_card);
            cost += effective;
            current_card = effective;
        } else {
            cost += first_card;
            bound_variables.insert(var.clone(), first_card);
        }
    } else {
        cost += first_card;
    }

    // Walk the chain of relationships.
    for (i, rel) in pattern.relationships.iter().enumerate() {
        let next_node = &pattern.nodes[i + 1];
        let traversal_cost = relationship_traversal_cost(rel, current_card, stats);
        cost += traversal_cost;

        // Cardinality after traversal.
        let degree = effective_degree(rel, stats);
        let mut next_card = current_card * degree;

        // Reduce by target node label selectivity.
        if let Some(ref label) = next_node.label {
            let label_card = u64_to_f64(stats.node_count(label));
            // If the target label is smaller than the expansion, clamp.
            next_card = next_card.min(label_card);
        }

        // Reduce by inline property filters on the target node.
        for _ in &next_node.properties {
            next_card *= PROPERTY_FILTER_SELECTIVITY;
        }
        next_card *= where_property_filter_selectivity(next_node, filters);

        // Reduce by inline property filters on the relationship.
        for _ in &rel.properties {
            next_card *= PROPERTY_FILTER_SELECTIVITY;
        }

        if let Some(ref var) = next_node.variable {
            if let Some(&existing_card) = bound_variables.get(var) {
                next_card = next_card.min(existing_card);
            }
        }

        next_card = next_card.max(1.0);
        current_card = next_card;

        if let Some(ref var) = next_node.variable {
            bound_variables
                .entry(var.clone())
                .and_modify(|existing| *existing = existing.min(current_card))
                .or_insert(current_card);
        }
    }

    cost
}

/// Estimate the cardinality of a node pattern (label + inline filters).
fn node_cardinality(node: &CypherNodePattern, stats: &GraphStats, filters: &[TypedExpr]) -> f64 {
    let base = if let Some(ref label) = node.label {
        u64_to_f64(stats.node_count(label))
    } else {
        // No label: scan all nodes -- sum of all known labels, or default.
        let total: u64 = stats.label_cardinality.values().sum();
        if total > 0 {
            u64_to_f64(total)
        } else {
            u64_to_f64(DEFAULT_NODE_COUNT)
        }
    };

    let mut card = base;
    for _ in &node.properties {
        card *= PROPERTY_FILTER_SELECTIVITY;
    }
    card *= where_property_filter_selectivity(node, filters);
    card.max(1.0)
}

fn where_property_filter_selectivity(node: &CypherNodePattern, filters: &[TypedExpr]) -> f64 {
    let Some(variable) = node.variable.as_deref() else {
        return 1.0;
    };
    filters.iter().fold(1.0, |selectivity, filter| {
        if matches_variable_property_literal_filter(filter, variable) {
            selectivity * PROPERTY_FILTER_SELECTIVITY
        } else {
            selectivity
        }
    })
}

fn matches_variable_property_literal_filter(expr: &TypedExpr, variable: &str) -> bool {
    extract_variable_property_literal_filter(expr)
        .is_some_and(|(candidate, _property)| candidate == variable)
}

fn extract_variable_property_literal_filter(expr: &TypedExpr) -> Option<(String, String)> {
    use aiondb_plan::TypedExprKind;

    match &expr.kind {
        TypedExprKind::BinaryEq { left, right }
        | TypedExprKind::BinaryGt { left, right }
        | TypedExprKind::BinaryGe { left, right }
        | TypedExprKind::BinaryLt { left, right }
        | TypedExprKind::BinaryLe { left, right } => {
            if is_literal_or_const(right) {
                return extract_variable_property_ref(left);
            }
            if is_literal_or_const(left) {
                return extract_variable_property_ref(right);
            }
            None
        }
        TypedExprKind::Between {
            expr,
            low,
            high,
            negated,
        } if !negated && is_literal_or_const(low) && is_literal_or_const(high) => {
            extract_variable_property_ref(expr)
        }
        _ => None,
    }
}

fn extract_variable_property_ref(expr: &TypedExpr) -> Option<(String, String)> {
    let name = extract_variable_name(expr)?;
    let (variable, property) = name.split_once('.').or_else(|| name.split_once('\0'))?;
    if variable.is_empty() || property.is_empty() || property.contains('\0') {
        return None;
    }
    Some((variable.to_owned(), property.to_owned()))
}

fn typed_expr_predicate_op(expr: &TypedExpr) -> Option<PredicateOp> {
    use aiondb_plan::TypedExprKind;

    match &expr.kind {
        TypedExprKind::BinaryEq { .. } => Some(PredicateOp::Equality),
        TypedExprKind::BinaryGt { .. }
        | TypedExprKind::BinaryGe { .. }
        | TypedExprKind::BinaryLt { .. }
        | TypedExprKind::BinaryLe { .. }
        | TypedExprKind::Between { .. } => Some(PredicateOp::Range),
        _ => None,
    }
}

fn cypher_direction_to_cbo(direction: CypherRelDirection) -> RelDirection {
    match direction {
        CypherRelDirection::Outgoing => RelDirection::Outgoing,
        CypherRelDirection::Incoming => RelDirection::Incoming,
        CypherRelDirection::Both => RelDirection::Both,
    }
}

fn cbo_direction_to_cypher(direction: RelDirection) -> CypherRelDirection {
    match direction {
        RelDirection::Outgoing => CypherRelDirection::Outgoing,
        RelDirection::Incoming => CypherRelDirection::Incoming,
        RelDirection::Both => CypherRelDirection::Both,
    }
}

/// Flatten a linear [`ExpansionPlan`] into `(seed_node, steps)` where each step
/// is `(rel_id, from_node, to_node, direction)` in execution order (the expand
/// adjacent to the seed first).
///
/// Returns `None` for any shape the executor's left-to-right path runner cannot
/// represent: `HashJoin`, `CartesianProduct`, or an `ExpandInto` (`into:
/// true`). Because the recursion descends into `input` before recording the
/// step, the collected steps are already seed-first — no post-reversal needed.
fn cbo_linear_sequence(
    plan: &ExpansionPlan,
) -> Option<(usize, Vec<(usize, usize, usize, RelDirection)>)> {
    fn walk(
        plan: &ExpansionPlan,
        steps: &mut Vec<(usize, usize, usize, RelDirection)>,
    ) -> Option<usize> {
        match &plan.op {
            PhysicalOp::AllNodesScan { node }
            | PhysicalOp::NodeByLabelScan { node, .. }
            | PhysicalOp::NodeIndexSeek { node, .. } => Some(node.0),
            PhysicalOp::Expand {
                input,
                rel,
                from,
                to,
                direction,
                into: false,
                ..
            } => {
                let seed = walk(input, steps)?;
                steps.push((rel.0, from.0, to.0, *direction));
                Some(seed)
            }
            PhysicalOp::Expand { into: true, .. }
            | PhysicalOp::HashJoin { .. }
            | PhysicalOp::CartesianProduct { .. } => None,
        }
    }
    let mut steps = Vec::new();
    let seed = walk(plan, &mut steps)?;
    Some((seed, steps))
}

fn cbo_leaf_seed(plan: &ExpansionPlan) -> Option<usize> {
    match &plan.op {
        PhysicalOp::AllNodesScan { node }
        | PhysicalOp::NodeByLabelScan { node, .. }
        | PhysicalOp::NodeIndexSeek { node, .. } => Some(node.0),
        PhysicalOp::Expand { input, .. } => cbo_leaf_seed(input),
        PhysicalOp::HashJoin { left, .. } | PhysicalOp::CartesianProduct { left, .. } => {
            cbo_leaf_seed(left)
        }
    }
}

/// Estimate the cost of traversing a relationship from `source_card` source
/// nodes.
fn relationship_traversal_cost(
    rel: &CypherRelPattern,
    source_card: f64,
    stats: &GraphStats,
) -> f64 {
    let degree = effective_degree(rel, stats);

    // For variable-length patterns, multiply by the expected path expansion.
    let expansion = variable_length_expansion(rel, degree);

    source_card * expansion
}

/// Compute the effective degree for a relationship, considering direction.
fn effective_degree(rel: &CypherRelPattern, stats: &GraphStats) -> f64 {
    match rel.direction {
        CypherRelDirection::Outgoing => relationship_type_names(rel)
            .iter()
            .map(|rel_type| stats.out_degree(rel_type))
            .sum(),
        CypherRelDirection::Incoming => relationship_type_names(rel)
            .iter()
            .map(|rel_type| stats.in_degree(rel_type))
            .sum(),
        CypherRelDirection::Both => relationship_type_names(rel)
            .iter()
            .map(|rel_type| stats.out_degree(rel_type) + stats.in_degree(rel_type))
            .sum(),
    }
}

fn relationship_type_names(rel: &CypherRelPattern) -> Vec<&str> {
    let mut names = Vec::new();
    if let Some(rel_type) = rel.rel_type.as_deref() {
        names.push(rel_type);
    }
    for rel_type in &rel.rel_type_alternatives {
        if !names.contains(&rel_type.as_str()) {
            names.push(rel_type.as_str());
        }
    }
    if names.is_empty() {
        names.push("");
    }
    names
}

/// Estimate the expansion factor for variable-length relationships.
///
/// For `*min..max`, the expansion is approximately:
///   sum(degree^k for k in min..=max)
/// We cap this to avoid runaway estimates.
fn variable_length_expansion(rel: &CypherRelPattern, degree: f64) -> f64 {
    let min_hops = rel.min_hops.unwrap_or(1);
    let max_hops = rel.max_hops.unwrap_or(1);

    if min_hops == 1 && max_hops == 1 {
        // Fixed single hop.
        return degree;
    }

    let mut expansion = 0.0;
    let effective_max = max_hops.min(10); // Cap to prevent huge estimates.
    for k in min_hops..=effective_max {
        let exponent = i32::try_from(k).unwrap_or(i32::MAX);
        expansion += degree.powi(exponent);
    }
    expansion.max(1.0)
}

// ---------------------------------------------------------------------------
// Graph optimizer
// ---------------------------------------------------------------------------

/// The graph query optimizer. Applies cost-based and rule-based optimizations
/// to `CypherQueryPlan` plans.
pub struct GraphOptimizer {
    stats: GraphStats,
}

impl GraphOptimizer {
    /// Create a new optimizer with the given statistics.
    pub fn new(stats: GraphStats) -> Self {
        Self { stats }
    }

    /// Create an optimizer with no statistics (all defaults).
    pub fn with_defaults() -> Self {
        Self {
            stats: GraphStats::empty(),
        }
    }

    /// Optimize a Cypher query plan, returning the optimized version.
    pub fn optimize_cypher_query(&self, mut plan: CypherQueryPlan) -> CypherQueryPlan {
        self.optimize_cypher_query_in_place(&mut plan);
        plan
    }

    /// Return the CBO-chosen seed node index for `pattern` when the pattern
    /// shape can be lowered to the graph planner.
    ///
    /// This is narrower than full pattern projection: callers such as the
    /// executor can still exploit the seed choice through their own pivoted
    /// walk even when `project_cbo_plan(...)` cannot rewrite the pattern into a
    /// contiguous left-to-right path.
    pub fn cbo_seed_index(&self, pattern: &CypherPattern, filters: &[TypedExpr]) -> Option<usize> {
        let plan = self.cbo_plan_for_pattern(pattern, filters)?;
        cbo_leaf_seed(&plan)
    }

    fn optimize_cypher_query_in_place(&self, plan: &mut CypherQueryPlan) {
        // Phase 1: Optimize each MATCH clause.
        for match_clause in &mut plan.matches {
            self.optimize_match_clause(match_clause);
        }

        // Phase 2: Optimize MATCH clauses inside the pipeline.
        for op in &mut plan.pipeline {
            match op {
                CypherPipelineOp::Match(match_clause) => {
                    self.optimize_match_clause(match_clause);
                }
                CypherPipelineOp::CallSubquery(subquery) => {
                    self.optimize_cypher_query_in_place(subquery);
                }
                CypherPipelineOp::Unwind(_)
                | CypherPipelineOp::With(_)
                | CypherPipelineOp::ProcedureCall(_)
                | CypherPipelineOp::Foreach(_) => {}
            }
        }

        if let Some(union) = &mut plan.union {
            self.optimize_cypher_query_in_place(&mut union.right);
        }
    }

    /// Apply all optimizations to a single MATCH clause.
    fn optimize_match_clause(&self, match_clause: &mut CypherMatchClause) {
        // Step 1: Push WHERE predicates into pattern inline properties.
        self.push_predicates_into_patterns(match_clause);
        let filters: Vec<TypedExpr> = match_clause.filter.iter().cloned().collect();

        // Step 2: Reorder patterns within the MATCH to start with the most
        // selective one.
        self.reorder_patterns(match_clause, &filters);

        // Step 3: For each pattern, reorder the starting node. The CBO drives
        // every path now: it projects per-pattern internal order, while Step 2
        // (`reorder_patterns`) independently fixes inter-pattern order, so
        // running it per pattern is safe regardless of pattern count.
        let allow_cbo_start = true;
        for pattern in &mut match_clause.patterns {
            self.optimize_pattern_start(pattern, &filters, allow_cbo_start);
        }

        // Step 4: Tighten variable-length bounds.
        self.tighten_variable_length_bounds(match_clause);
    }

    // -----------------------------------------------------------------------
    // Pattern reordering
    // -----------------------------------------------------------------------

    /// Reorder patterns within a MATCH clause so that the most selective
    /// pattern (lowest estimated cost) is matched first.
    fn reorder_patterns(&self, match_clause: &mut CypherMatchClause, filters: &[TypedExpr]) {
        if match_clause.patterns.len() <= 1 {
            return;
        }

        // Drain the patterns into Option<> slots so we can hand them out by
        // move during the greedy pick loop. The original code cloned the
        // entire vector and then cloned each chosen pattern; for large
        // multi-pattern MATCH clauses that was O(N^2) full-pattern clones.
        let mut original_patterns: Vec<Option<CypherPattern>> =
            std::mem::take(&mut match_clause.patterns)
                .into_iter()
                .map(Some)
                .collect();
        let mut remaining: Vec<usize> = (0..original_patterns.len()).collect();
        let mut reordered: Vec<CypherPattern> = Vec::with_capacity(original_patterns.len());
        let mut bound_variables = HashMap::new();

        while !remaining.is_empty() {
            let (best_pos, best_index) = remaining
                .iter()
                .enumerate()
                .min_by(|(_, left), (_, right)| {
                    let cost_left = estimate_pattern_cost_with_existing_bindings(
                        original_patterns[**left]
                            .as_ref()
                            .expect("pattern still present"),
                        &self.stats,
                        filters,
                        &bound_variables,
                    );
                    let cost_right = estimate_pattern_cost_with_existing_bindings(
                        original_patterns[**right]
                            .as_ref()
                            .expect("pattern still present"),
                        &self.stats,
                        filters,
                        &bound_variables,
                    );
                    cost_left
                        .partial_cmp(&cost_right)
                        .unwrap_or(std::cmp::Ordering::Equal)
                })
                .map(|(pos, &index)| (pos, index))
                .expect("remaining pattern indices should not be empty");

            estimate_single_pattern_cost(
                original_patterns[best_index]
                    .as_ref()
                    .expect("pattern still present"),
                &self.stats,
                filters,
                &mut bound_variables,
            );
            reordered.push(
                original_patterns[best_index]
                    .take()
                    .expect("pattern still present"),
            );
            remaining.remove(best_pos);
        }

        match_clause.patterns = reordered;
    }

    /// Within a single path pattern, consider reversing the traversal direction
    /// to start from the node with the smallest cardinality.
    fn optimize_pattern_start(
        &self,
        pattern: &mut CypherPattern,
        filters: &[TypedExpr],
        allow_cbo_start: bool,
    ) {
        if pattern.nodes.len() < 2 || pattern.relationships.is_empty() {
            return;
        }
        if pattern.path_variable.is_some() || pattern.path_function.is_some() {
            return;
        }
        // Variable-length rels are now CBO-driven too: the cost model already
        // prices them (geometric hop fan-out), and reversing a `*min..max`
        // walk is path-set-equivalent. `project_cbo_plan` clones each rel, so
        // `min_hops`/`max_hops` ride along with the reorder unchanged. (Named
        // paths and shortestPath stay anchored — guarded above.)
        if allow_cbo_start {
            if let Some(plan) = self.cbo_plan_for_pattern(pattern, filters) {
                if self.project_cbo_plan(pattern, &plan) {
                    return;
                }
            }
        }

        let first_card = self.node_est_cardinality(&pattern.nodes[0], filters);
        let last_node = match pattern.nodes.last() {
            Some(n) => n,
            None => return,
        };
        let last_card = self.node_est_cardinality(last_node, filters);

        // Only reverse if the last node is significantly more selective.
        if last_card < first_card * 0.5 {
            self.reverse_pattern(pattern);
        }
    }

    fn cbo_plan_for_pattern(
        &self,
        pattern: &CypherPattern,
        filters: &[TypedExpr],
    ) -> Option<ExpansionPlan> {
        if pattern
            .relationships
            .iter()
            .any(|rel| rel.direction == CypherRelDirection::Both)
        {
            return None;
        }
        let query_graph = self.lower_pattern_to_query_graph(pattern, filters)?;
        plan_query_graph(&query_graph, &self.stats, &PlannerConfig::default()).ok()
    }

    /// Project a cost-based [`ExpansionPlan`] back onto `pattern`: reorder its
    /// nodes/relationships into the planner's traversal order and set each
    /// relationship's direction to the orientation the planner chose.
    ///
    /// `lower_pattern_to_query_graph` maps `NodeId(i)` to `pattern.nodes[i]` and
    /// `RelId(i)` to `pattern.relationships[i]`, so plan ids index the original
    /// vectors directly. Only a *contiguous left-deep path* (seed at one path
    /// end, every expand extending the running endpoint) can be expressed as a
    /// `CypherPattern`; a middle seed, `ExpandInto`, or `HashJoin` is not a
    /// simple path, so `false` is returned and the caller falls back to the
    /// cardinality heuristic.
    fn project_cbo_plan(&self, pattern: &mut CypherPattern, plan: &ExpansionPlan) -> bool {
        let Some((seed, steps)) = cbo_linear_sequence(plan) else {
            return false;
        };
        if steps.len() != pattern.relationships.len() || seed >= pattern.nodes.len() {
            return false;
        }

        let old_nodes = pattern.nodes.clone();
        let old_rels = pattern.relationships.clone();
        let mut nodes = Vec::with_capacity(old_nodes.len());
        let mut rels = Vec::with_capacity(old_rels.len());

        nodes.push(old_nodes[seed].clone());
        let mut endpoint = seed;
        for (rel_id, from, to, direction) in steps {
            if from != endpoint || rel_id >= old_rels.len() || to >= old_nodes.len() {
                return false;
            }
            let mut rel = old_rels[rel_id].clone();
            rel.direction = cbo_direction_to_cypher(direction);
            rels.push(rel);
            nodes.push(old_nodes[to].clone());
            endpoint = to;
        }

        if nodes.len() != old_nodes.len() {
            return false;
        }
        pattern.nodes = nodes;
        pattern.relationships = rels;
        true
    }

    fn lower_pattern_to_query_graph(
        &self,
        pattern: &CypherPattern,
        filters: &[TypedExpr],
    ) -> Option<QueryGraph> {
        let mut predicate_map: HashMap<String, Vec<PropertyPredicate>> = HashMap::new();
        for filter in filters {
            // Unwrap the comparison (`b.x > 20`, `BETWEEN`, …) to its
            // column-ref operand; `extract_variable_property_ref` alone only
            // matches a bare ref, so range/inequality WHERE predicates were
            // silently dropped and never reached the cost model.
            let Some((variable, property)) = extract_variable_property_literal_filter(filter)
            else {
                continue;
            };
            let Some(op) = typed_expr_predicate_op(filter) else {
                continue;
            };
            predicate_map
                .entry(variable)
                .or_default()
                .push(PropertyPredicate::new(property, op));
        }

        let mut query_graph = QueryGraph::new();
        for (index, node) in pattern.nodes.iter().enumerate() {
            let mut query_node = if let Some(label) = node.label.as_deref() {
                QueryNode::labelled(index, label)
            } else {
                QueryNode::anonymous(index)
            };
            for property in &node.properties {
                query_node = query_node.with_predicate(PropertyPredicate::equality(&property.key));
            }
            if let Some(variable) = node.variable.as_deref() {
                if let Some(predicates) = predicate_map.get(variable) {
                    for predicate in predicates {
                        query_node = query_node.with_predicate(predicate.clone());
                    }
                }
            }
            if let Some(index_scan) = &node.index_scan {
                let property = node
                    .properties
                    .first()
                    .map(|property| property.key.as_str())
                    .or_else(|| {
                        node.variable
                            .as_deref()
                            .and_then(|variable| predicate_map.get(variable))
                            .and_then(|predicates| predicates.first())
                            .map(|predicate| predicate.property.as_str())
                    })
                    .unwrap_or("id");
                let index_kind = if property.eq_ignore_ascii_case("id") {
                    IndexKind::Unique
                } else {
                    IndexKind::NonUnique
                };
                let _ = index_scan;
                query_node = query_node.with_index(property, index_kind);
            }
            query_graph.add_node(query_node);
        }

        for (index, rel) in pattern.relationships.iter().enumerate() {
            let mut query_rel = QueryRel::new(
                index,
                index,
                index + 1,
                rel.rel_type.as_deref(),
                cypher_direction_to_cbo(rel.direction),
            );
            for property in &rel.properties {
                query_rel = query_rel.with_predicate(PropertyPredicate::equality(&property.key));
            }
            if rel.min_hops.is_some() || rel.max_hops.is_some() {
                query_rel = query_rel.with_var_length(rel.min_hops.unwrap_or(1), rel.max_hops);
            }
            query_graph.add_rel(query_rel);
        }

        query_graph.validate().ok()?;
        Some(query_graph)
    }

    /// Estimate cardinality of a single node pattern for ordering decisions.
    fn node_est_cardinality(&self, node: &CypherNodePattern, filters: &[TypedExpr]) -> f64 {
        let base = if let Some(ref label) = node.label {
            u64_to_f64(self.stats.node_count(label))
        } else {
            u64_to_f64(DEFAULT_NODE_COUNT)
        };
        let mut card = base;
        for _ in &node.properties {
            card *= PROPERTY_FILTER_SELECTIVITY;
        }
        card *= where_property_filter_selectivity(node, filters);
        card
    }

    /// Reverse a path pattern: reverse node order and relationship order,
    /// flipping relationship directions.
    fn reverse_pattern(&self, pattern: &mut CypherPattern) {
        pattern.nodes.reverse();
        pattern.relationships.reverse();
        for rel in &mut pattern.relationships {
            rel.direction = match rel.direction {
                CypherRelDirection::Outgoing => CypherRelDirection::Incoming,
                CypherRelDirection::Incoming => CypherRelDirection::Outgoing,
                CypherRelDirection::Both => CypherRelDirection::Both,
            };
        }
    }

    // -----------------------------------------------------------------------
    // Predicate pushdown
    // -----------------------------------------------------------------------

    /// Push WHERE clause predicates into pattern inline property filters when
    /// the predicate references a single node/edge variable and is an equality
    /// check on a property.
    ///
    /// For example:
    ///   `MATCH (n:Person) WHERE n.name = 'Alice'`
    /// becomes:
    ///   `MATCH (n:Person {name: 'Alice'})`
    ///
    /// This allows the executor to filter during pattern matching rather than
    /// post-filtering the full result set.
    fn push_predicates_into_patterns(&self, match_clause: &mut CypherMatchClause) {
        let filter = match match_clause.filter.take() {
            Some(f) => f,
            None => return,
        };

        let mut conjuncts = Vec::new();
        collect_and_conjuncts(&filter, &mut conjuncts);

        // Collect all variable names from patterns.
        let mut node_vars: HashMap<String, (usize, usize)> = HashMap::new(); // var -> (pattern_idx, node_idx)
        let mut rel_vars: HashMap<String, (usize, usize)> = HashMap::new(); // var -> (pattern_idx, rel_idx)

        for (pi, pattern) in match_clause.patterns.iter().enumerate() {
            for (ni, node) in pattern.nodes.iter().enumerate() {
                if let Some(ref var) = node.variable {
                    node_vars.insert(var.clone(), (pi, ni));
                }
            }
            for (ri, rel) in pattern.relationships.iter().enumerate() {
                if let Some(ref var) = rel.variable {
                    rel_vars.insert(var.clone(), (pi, ri));
                }
            }
        }

        let mut remaining_conjuncts = Vec::new();

        for conjunct in conjuncts {
            if let Some((var, prop_key, prop_value)) = extract_property_equality(&conjunct) {
                if let Some(&(pi, ni)) = node_vars.get(&var) {
                    // Push into node's inline properties.
                    push_property_filter(
                        &mut match_clause.patterns[pi].nodes[ni].properties,
                        prop_key,
                        prop_value,
                    );
                    continue;
                }
                if let Some(&(pi, ri)) = rel_vars.get(&var) {
                    // Push into relationship's inline properties.
                    push_property_filter(
                        &mut match_clause.patterns[pi].relationships[ri].properties,
                        prop_key,
                        prop_value,
                    );
                    continue;
                }
            }
            remaining_conjuncts.push(conjunct);
        }

        // Reconstruct the remaining filter.
        match_clause.filter = combine_conjuncts(remaining_conjuncts);
    }

    // -----------------------------------------------------------------------
    // Variable-length bound tightening
    // -----------------------------------------------------------------------

    /// Tighten min/max hops for variable-length relationships when the WHERE
    /// clause contains `length(path) >= N` or `length(path) <= N` constraints.
    ///
    /// This is a conservative rule-based optimization: we only look for simple
    /// patterns like `length(p) >= 2` or `length(p) <= 5`.
    fn tighten_variable_length_bounds(&self, match_clause: &mut CypherMatchClause) {
        let filter = match &match_clause.filter {
            Some(f) => f.clone(),
            None => return,
        };

        let mut conjuncts = Vec::new();
        collect_and_conjuncts(&filter, &mut conjuncts);

        for conjunct in &conjuncts {
            if let Some((var, bound_kind, value)) = extract_length_constraint(conjunct) {
                // Find a variable-length relationship bound either directly
                // to the relationship variable, or conservatively through a
                // named path that contains exactly that one relationship.
                for pattern in &mut match_clause.patterns {
                    let single_rel_path_var_match = pattern.path_variable.as_deref()
                        == Some(var.as_str())
                        && pattern.relationships.len() == 1;
                    for rel in &mut pattern.relationships {
                        let is_var_length = rel.min_hops.is_some() || rel.max_hops.is_some();
                        if !is_var_length {
                            continue;
                        }

                        let rel_var = rel.variable.as_deref().unwrap_or("");
                        if rel_var != var && !single_rel_path_var_match {
                            continue;
                        }

                        match bound_kind {
                            BoundKind::MinInclusive => {
                                let current_min = rel.min_hops.unwrap_or(1);
                                if value > current_min {
                                    rel.min_hops = Some(value);
                                }
                            }
                            BoundKind::MaxInclusive => {
                                if let Some(current_max) = rel.max_hops {
                                    if value < current_max {
                                        rel.max_hops = Some(value);
                                    }
                                } else {
                                    rel.max_hops = Some(value);
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Helper types and functions
// ---------------------------------------------------------------------------

/// Kind of bound constraint extracted from a WHERE clause.
#[derive(Debug, Clone, Copy)]
enum BoundKind {
    MinInclusive,
    MaxInclusive,
}

/// Decompose an expression into AND conjuncts.
fn collect_and_conjuncts(expr: &TypedExpr, out: &mut Vec<TypedExpr>) {
    use aiondb_plan::TypedExprKind;
    let mut stack = vec![expr];
    while let Some(expr) = stack.pop() {
        match &expr.kind {
            TypedExprKind::LogicalAnd { left, right } => {
                stack.push(right);
                stack.push(left);
            }
            _ => {
                out.push(expr.clone());
            }
        }
    }
}

use crate::predicate_pushdown::combine_conjuncts;

/// Try to extract a `variable.property = literal_value` equality from an
/// expression. Returns `(variable_name, property_key, value_expr)` on success.
fn extract_property_equality(expr: &TypedExpr) -> Option<(String, String, TypedExpr)> {
    use aiondb_plan::TypedExprKind;

    match &expr.kind {
        TypedExprKind::BinaryEq { left, right } => {
            // Check left = property access, right = literal (or vice versa).
            if let Some((var, key)) = extract_property_access(left) {
                if is_literal_or_const(right) {
                    return Some((var, key, (**right).clone()));
                }
            }
            if let Some((var, key)) = extract_property_access(right) {
                if is_literal_or_const(left) {
                    return Some((var, key, (**left).clone()));
                }
            }
            None
        }
        _ => None,
    }
}

/// Try to extract `variable.property` from a typed expression.
///
/// Property access in the plan layer is represented as `ScalarFunction`
/// calls with name `"cypher_property_access"` or as `JsonGet`/`JsonGetText`
/// operations. We also support the `CypherPropertyAccess` kind if it exists.
fn extract_property_access(expr: &TypedExpr) -> Option<(String, String)> {
    use aiondb_plan::{ScalarFunction, TypedExprKind};

    match &expr.kind {
        TypedExprKind::ColumnRef { name, .. } | TypedExprKind::OuterColumnRef { name, .. } => {
            split_cypher_property_column_ref(name)
        }
        TypedExprKind::ScalarFunction { func, args } => {
            if let ScalarFunction::Generic(name) = func {
                if name == "cypher_property_access" && args.len() == 2 {
                    // args[0] = variable ref, args[1] = property key literal
                    let var_name = extract_variable_name(&args[0])?;
                    let prop_key = extract_string_literal(&args[1])?;
                    return Some((var_name, prop_key));
                }
            }
            None
        }
        TypedExprKind::JsonGetText { left, right } | TypedExprKind::JsonGet { left, right } => {
            let var_name = extract_variable_name(left)?;
            let prop_key = extract_string_literal(right)?;
            Some((var_name, prop_key))
        }
        _ => None,
    }
}

fn split_cypher_property_column_ref(name: &str) -> Option<(String, String)> {
    let (variable, property) = name.split_once('.').or_else(|| name.split_once('\0'))?;
    if variable.is_empty()
        || property.is_empty()
        || property.contains('.')
        || property.contains('\0')
    {
        return None;
    }
    Some((variable.to_owned(), property.to_owned()))
}

fn push_property_filter(properties: &mut Vec<CypherPropertyExpr>, key: String, value: TypedExpr) {
    if properties
        .iter()
        .any(|property| property.key.eq_ignore_ascii_case(&key) && property.value == value)
    {
        return;
    }
    properties.push(CypherPropertyExpr { key, value });
}

/// Extract a variable name from a typed expression (column reference or
/// direct variable reference).
fn extract_variable_name(expr: &TypedExpr) -> Option<String> {
    use aiondb_plan::{ScalarFunction, TypedExprKind};

    match &expr.kind {
        TypedExprKind::ColumnRef { name, .. } => Some(name.clone()),
        TypedExprKind::ScalarFunction { func, args } => {
            // Cypher variable reference encoded as a generic scalar function.
            if let ScalarFunction::Generic(fname) = func {
                if fname == "cypher_variable" && args.len() == 1 {
                    return extract_string_literal(&args[0]);
                }
            }
            None
        }
        _ => None,
    }
}

/// Extract a string literal value.
fn extract_string_literal(expr: &TypedExpr) -> Option<String> {
    use aiondb_core::Value;
    use aiondb_plan::TypedExprKind;

    match &expr.kind {
        TypedExprKind::Literal(Value::Text(s)) => Some(s.clone()),
        _ => None,
    }
}

/// Check if an expression is a constant / literal.
fn is_literal_or_const(expr: &TypedExpr) -> bool {
    use aiondb_plan::TypedExprKind;

    match &expr.kind {
        TypedExprKind::Literal(_) => true,
        TypedExprKind::Cast { expr, .. } => is_literal_or_const(expr),
        _ => false,
    }
}

/// Try to extract a `length(variable) >= N` or `length(variable) <= N`
/// constraint from an expression.
fn extract_length_constraint(expr: &TypedExpr) -> Option<(String, BoundKind, u32)> {
    use aiondb_plan::TypedExprKind;

    match &expr.kind {
        TypedExprKind::BinaryGe { left, right } => {
            // length(var) >= N
            if let Some(var) = extract_length_call(left) {
                if let Some(n) = extract_u32_literal(right) {
                    return Some((var, BoundKind::MinInclusive, n));
                }
            }
            // N >= length(var) => length(var) <= N
            if let Some(var) = extract_length_call(right) {
                if let Some(n) = extract_u32_literal(left) {
                    return Some((var, BoundKind::MaxInclusive, n));
                }
            }
            None
        }
        TypedExprKind::BinaryLe { left, right } => {
            // length(var) <= N
            if let Some(var) = extract_length_call(left) {
                if let Some(n) = extract_u32_literal(right) {
                    return Some((var, BoundKind::MaxInclusive, n));
                }
            }
            // N <= length(var) => length(var) >= N
            if let Some(var) = extract_length_call(right) {
                if let Some(n) = extract_u32_literal(left) {
                    return Some((var, BoundKind::MinInclusive, n));
                }
            }
            None
        }
        TypedExprKind::BinaryGt { left, right } => {
            // length(var) > N => min = N+1
            if let Some(var) = extract_length_call(left) {
                if let Some(n) = extract_u32_literal(right) {
                    return Some((var, BoundKind::MinInclusive, n.saturating_add(1)));
                }
            }
            None
        }
        TypedExprKind::BinaryLt { left, right } => {
            // length(var) < N => max = N-1
            if let Some(var) = extract_length_call(left) {
                if let Some(n) = extract_u32_literal(right) {
                    if n > 0 {
                        return Some((var, BoundKind::MaxInclusive, n - 1));
                    }
                }
            }
            None
        }
        _ => None,
    }
}

/// Extract the variable name from a `length(variable)` function call.
fn extract_length_call(expr: &TypedExpr) -> Option<String> {
    use aiondb_plan::{ScalarFunction, TypedExprKind};

    match &expr.kind {
        TypedExprKind::ScalarFunction { func, args } if args.len() == 1 => {
            let is_length = matches!(func, ScalarFunction::Length)
                || matches!(func, ScalarFunction::Generic(name) if name == "length" || name == "size");
            if is_length {
                extract_variable_name(&args[0])
            } else {
                None
            }
        }
        _ => None,
    }
}

/// Extract a non-negative u32 value from a literal integer expression.
fn extract_u32_literal(expr: &TypedExpr) -> Option<u32> {
    use aiondb_core::Value;
    use aiondb_plan::TypedExprKind;

    match &expr.kind {
        TypedExprKind::Literal(Value::Int(n)) => u32::try_from(*n).ok(),
        TypedExprKind::Literal(Value::BigInt(n)) => u32::try_from(*n).ok(),
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use aiondb_core::{DataType, Value};
    use aiondb_plan::{TypedExpr, TypedExprKind};

    fn make_node(var: Option<&str>, label: Option<&str>) -> CypherNodePattern {
        CypherNodePattern {
            variable: var.map(String::from),
            label: label.map(String::from),
            table_id: None,
            properties: Vec::new(),
            index_scan: None,
            range_pushdown: Vec::new(),
        }
    }

    fn make_rel(
        var: Option<&str>,
        rel_type: Option<&str>,
        direction: CypherRelDirection,
    ) -> CypherRelPattern {
        CypherRelPattern {
            variable: var.map(String::from),
            rel_type: rel_type.map(String::from),
            rel_type_alternatives: Vec::new(),
            table_id: None,
            direction,
            properties: Vec::new(),
            min_hops: None,
            max_hops: None,
            index_scan: None,
        }
    }

    fn make_pattern(nodes: Vec<CypherNodePattern>, rels: Vec<CypherRelPattern>) -> CypherPattern {
        CypherPattern {
            path_function: None,
            path_variable: None,
            nodes,
            relationships: rels,
        }
    }

    fn make_match(patterns: Vec<CypherPattern>) -> CypherMatchClause {
        CypherMatchClause {
            optional: false,
            patterns,
            filter: None,
        }
    }

    fn empty_plan() -> CypherQueryPlan {
        CypherQueryPlan {
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

    fn make_stats() -> GraphStats {
        let mut stats = GraphStats::default();
        stats.label_cardinality.insert("Person".into(), 1000);
        stats.label_cardinality.insert("Movie".into(), 100);
        stats.label_cardinality.insert("City".into(), 50);
        stats.edge_cardinality.insert("ACTED_IN".into(), 2000);
        stats.edge_cardinality.insert("KNOWS".into(), 5000);
        stats.edge_cardinality.insert("LIKES".into(), 7000);
        stats.avg_out_degree.insert("ACTED_IN".into(), 2.0);
        stats.avg_in_degree.insert("ACTED_IN".into(), 20.0);
        stats.avg_out_degree.insert("KNOWS".into(), 5.0);
        stats.avg_in_degree.insert("KNOWS".into(), 5.0);
        stats.avg_out_degree.insert("LIKES".into(), 7.0);
        stats.avg_in_degree.insert("LIKES".into(), 3.0);
        stats
    }

    #[test]
    fn pattern_reorder_selects_cheapest_first() {
        let stats = make_stats();
        let optimizer = GraphOptimizer::new(stats);

        // Pattern A: (p:Person)-[:KNOWS]->(q:Person) -- 1000 start nodes
        // Pattern B: (m:Movie) -- 100 start nodes
        let pattern_a = make_pattern(
            vec![
                make_node(Some("p"), Some("Person")),
                make_node(Some("q"), Some("Person")),
            ],
            vec![make_rel(None, Some("KNOWS"), CypherRelDirection::Outgoing)],
        );
        let pattern_b = make_pattern(vec![make_node(Some("m"), Some("Movie"))], vec![]);

        let mut match_clause = make_match(vec![pattern_a, pattern_b]);
        optimizer.reorder_patterns(&mut match_clause, &[]);

        // Pattern B (Movie, card=100) should come first.
        assert_eq!(
            match_clause.patterns[0].nodes[0].label.as_deref(),
            Some("Movie")
        );
    }

    #[test]
    fn pattern_reorder_prefers_pattern_that_reuses_bound_variable() {
        let stats = make_stats();
        let optimizer = GraphOptimizer::new(stats);

        let mut anchor = make_pattern(vec![make_node(Some("c"), Some("City"))], vec![]);
        anchor.nodes[0].properties.push(CypherPropertyExpr {
            key: "name".into(),
            value: TypedExpr::literal(Value::Text("Paris".into()), DataType::Text, false),
        });

        let dependent = make_pattern(
            vec![
                make_node(Some("c"), None),
                make_node(Some("m"), Some("Movie")),
            ],
            vec![make_rel(
                None,
                Some("ACTED_IN"),
                CypherRelDirection::Outgoing,
            )],
        );
        let independent = make_pattern(vec![make_node(Some("m2"), Some("Movie"))], vec![]);

        let mut match_clause = make_match(vec![anchor, independent, dependent]);
        optimizer.reorder_patterns(&mut match_clause, &[]);

        assert_eq!(
            match_clause.patterns[0].nodes[0].variable.as_deref(),
            Some("c")
        );
        assert_eq!(
            match_clause.patterns[1].nodes[0].variable.as_deref(),
            Some("c")
        );
        assert_eq!(
            match_clause.patterns[2].nodes[0].variable.as_deref(),
            Some("m2")
        );
    }

    #[test]
    fn optimize_start_reverses_for_selective_end() {
        let stats = make_stats();
        let optimizer = GraphOptimizer::new(stats);

        // (p:Person)-[:ACTED_IN]->(m:Movie)
        // Person=1000, Movie=100 => should NOT reverse since Movie is at end
        // But if we reverse the order: (m:Movie)-[:ACTED_IN]->(p:Person)
        // Then Person=1000, Movie=100 => Movie start is cheaper
        // Test with Movie at start and Person at end:
        let mut pattern = make_pattern(
            vec![
                make_node(Some("p"), Some("Person")),
                make_node(Some("m"), Some("City")),
            ],
            vec![make_rel(
                None,
                Some("ACTED_IN"),
                CypherRelDirection::Outgoing,
            )],
        );

        // Person(1000) -> City(50): City is < Person * 0.5, so should reverse
        optimizer.optimize_pattern_start(&mut pattern, &[], true);

        // After reversal, City should be first.
        assert_eq!(pattern.nodes[0].label.as_deref(), Some("City"));
        // Direction should be flipped.
        assert_eq!(
            pattern.relationships[0].direction,
            CypherRelDirection::Incoming
        );
    }

    #[test]
    fn optimize_start_preserves_named_path_orientation() {
        let stats = make_stats();
        let optimizer = GraphOptimizer::new(stats);

        let mut pattern = make_pattern(
            vec![
                make_node(Some("p"), Some("Person")),
                make_node(Some("c"), Some("City")),
            ],
            vec![make_rel(
                None,
                Some("ACTED_IN"),
                CypherRelDirection::Outgoing,
            )],
        );
        pattern.path_variable = Some("path".to_owned());

        optimizer.optimize_pattern_start(&mut pattern, &[], true);

        assert_eq!(pattern.nodes[0].label.as_deref(), Some("Person"));
        assert_eq!(
            pattern.relationships[0].direction,
            CypherRelDirection::Outgoing
        );
    }

    #[test]
    fn cbo_reorders_variable_length_walk_preserving_hop_bounds() {
        // Symmetric-degree KNOWS (so direction alone is cost-neutral) plus a
        // highly selective equality on the *far* node `b`: the cost-based
        // planner must seed `b` and reverse the `*1..2` walk. The reversal is
        // path-set-equivalent only if the hop bounds survive intact -- that is
        // the safety invariant variable-length wiring must hold.
        let mut stats = GraphStats::empty();
        stats.label_cardinality.insert("Person".to_owned(), 100_000);
        stats.edge_cardinality.insert("KNOWS".to_owned(), 500_000);
        stats.edge_endpoints.insert(
            "KNOWS".to_owned(),
            ("Person".to_owned(), "Person".to_owned()),
        );
        stats
            .distinct
            .insert(("Person".to_owned(), "email".to_owned()), 100_000.0);
        let optimizer = GraphOptimizer::new(stats);

        let mut rel = make_rel(None, Some("KNOWS"), CypherRelDirection::Outgoing);
        rel.min_hops = Some(1);
        rel.max_hops = Some(2);
        let pattern = make_pattern(
            vec![
                make_node(Some("a"), Some("Person")),
                make_node(Some("b"), Some("Person")),
            ],
            vec![rel],
        );
        let mut match_clause = make_match(vec![pattern]);
        match_clause.filter = Some(TypedExpr::binary_eq(
            TypedExpr::column_ref("b.email", 0, DataType::Text, false),
            TypedExpr::literal(Value::Text("x@example.com".into()), DataType::Text, false),
        ));

        optimizer.optimize_match_clause(&mut match_clause);
        let pattern = &match_clause.patterns[0];

        // Reordered to seed the selective end, direction flipped...
        assert_eq!(pattern.nodes[0].variable.as_deref(), Some("b"));
        assert_eq!(
            pattern.relationships[0].direction,
            CypherRelDirection::Incoming
        );
        // ...but the variable-length bounds must ride along unchanged.
        assert_eq!(pattern.relationships[0].min_hops, Some(1));
        assert_eq!(pattern.relationships[0].max_hops, Some(2));
    }

    #[test]
    fn undirected_relationship_direction_is_preserved_with_asymmetric_stats() {
        let stats = make_stats();
        let optimizer = GraphOptimizer::new(stats);

        // ACTED_IN: out_degree=2, in_degree=20
        let mut pattern = make_pattern(
            vec![make_node(Some("a"), None), make_node(Some("b"), None)],
            vec![make_rel(None, Some("ACTED_IN"), CypherRelDirection::Both)],
        );
        let mut match_clause = make_match(vec![pattern.clone()]);

        optimizer.optimize_match_clause(&mut match_clause);
        pattern = match_clause.patterns.remove(0);

        assert_eq!(pattern.relationships[0].direction, CypherRelDirection::Both);
    }

    #[test]
    fn undirected_relationship_direction_is_preserved_with_symmetric_stats() {
        let stats = make_stats();
        let optimizer = GraphOptimizer::new(stats);

        // KNOWS: out_degree=5, in_degree=5 (symmetric)
        let mut pattern = make_pattern(
            vec![make_node(Some("a"), None), make_node(Some("b"), None)],
            vec![make_rel(None, Some("KNOWS"), CypherRelDirection::Both)],
        );
        let mut match_clause = make_match(vec![pattern.clone()]);

        optimizer.optimize_match_clause(&mut match_clause);
        pattern = match_clause.patterns.remove(0);

        assert_eq!(pattern.relationships[0].direction, CypherRelDirection::Both);
    }

    #[test]
    fn cost_estimation_with_filters() {
        let stats = make_stats();

        let pattern = make_pattern(
            vec![
                make_node(Some("p"), Some("Person")),
                make_node(Some("m"), Some("Movie")),
            ],
            vec![make_rel(
                None,
                Some("ACTED_IN"),
                CypherRelDirection::Outgoing,
            )],
        );

        let cost_no_filter = estimate_pattern_cost(std::slice::from_ref(&pattern), &stats, &[]);
        assert!(cost_no_filter > 0.0);

        // A pattern with inline property filter on the start node should be cheaper.
        let mut filtered_pattern = pattern;
        filtered_pattern.nodes[0]
            .properties
            .push(CypherPropertyExpr {
                key: "name".into(),
                value: TypedExpr::literal(Value::Text("Alice".into()), DataType::Text, false),
            });

        let cost_with_filter = estimate_pattern_cost(&[filtered_pattern], &stats, &[]);
        assert!(cost_with_filter < cost_no_filter);
    }

    #[test]
    fn cost_estimation_sums_relationship_type_alternatives() {
        let stats = make_stats();
        let mut rel = make_rel(None, Some("KNOWS"), CypherRelDirection::Outgoing);
        rel.rel_type_alternatives.push("LIKES".to_owned());

        let degree = super::effective_degree(&rel, &stats);

        assert!((degree - 12.0).abs() < f64::EPSILON);
    }

    #[test]
    fn optimize_pattern_start_uses_where_range_selectivity() {
        let mut stats = GraphStats::empty();
        stats.label_cardinality.insert("Person".to_owned(), 100_000);
        stats.edge_cardinality.insert("KNOWS".to_owned(), 500_000);
        stats.avg_out_degree.insert("KNOWS".to_owned(), 5.0);
        stats.avg_in_degree.insert("KNOWS".to_owned(), 5.0);
        let optimizer = GraphOptimizer::new(stats);

        let pattern = make_pattern(
            vec![
                make_node(Some("a"), Some("Person")),
                make_node(Some("b"), Some("Person")),
            ],
            vec![make_rel(None, Some("KNOWS"), CypherRelDirection::Outgoing)],
        );
        let mut match_clause = make_match(vec![pattern]);
        match_clause.filter = Some(TypedExpr::binary_gt(
            TypedExpr::column_ref("b.number", 0, DataType::Int, false),
            TypedExpr::literal(Value::Int(20), DataType::Int, false),
        ));

        optimizer.optimize_match_clause(&mut match_clause);

        assert_eq!(
            match_clause.patterns[0].nodes[0].variable.as_deref(),
            Some("b")
        );
        assert_eq!(
            match_clause.patterns[0].nodes[1].variable.as_deref(),
            Some("a")
        );
        assert_eq!(
            match_clause.patterns[0].relationships[0].direction,
            CypherRelDirection::Incoming
        );
    }

    #[test]
    fn variable_length_expansion() {
        let _stats = make_stats();

        // Fixed length: cost = degree
        let fixed_rel = make_rel(None, Some("KNOWS"), CypherRelDirection::Outgoing);
        let expansion_fixed = super::variable_length_expansion(&fixed_rel, 5.0);
        assert_eq!(expansion_fixed, 5.0);

        // Variable length *1..3: cost = degree^1 + degree^2 + degree^3
        let mut var_rel = make_rel(None, Some("KNOWS"), CypherRelDirection::Outgoing);
        var_rel.min_hops = Some(1);
        var_rel.max_hops = Some(3);
        let expansion_var = super::variable_length_expansion(&var_rel, 5.0);
        // 5 + 25 + 125 = 155
        assert!((expansion_var - 155.0).abs() < 0.01);
    }

    #[test]
    fn named_path_length_constraint_tightens_single_variable_length_rel() {
        let stats = make_stats();
        let optimizer = GraphOptimizer::new(stats);

        let mut rel = make_rel(Some("r"), Some("KNOWS"), CypherRelDirection::Outgoing);
        rel.min_hops = Some(1);
        rel.max_hops = Some(10);
        let mut pattern = make_pattern(
            vec![
                make_node(Some("a"), Some("Person")),
                make_node(Some("b"), Some("Person")),
            ],
            vec![rel],
        );
        pattern.path_variable = Some("p".to_owned());

        let length_call = TypedExpr {
            kind: TypedExprKind::ScalarFunction {
                func: aiondb_plan::ScalarFunction::Length,
                args: vec![TypedExpr {
                    kind: TypedExprKind::ColumnRef {
                        name: "p".to_owned(),
                        ordinal: 0,
                    },
                    data_type: DataType::Text,
                    nullable: false,
                }],
            },
            data_type: DataType::Int,
            nullable: false,
        };
        let mut match_clause = make_match(vec![pattern]);
        match_clause.filter = Some(TypedExpr {
            kind: TypedExprKind::BinaryLe {
                left: Box::new(length_call),
                right: Box::new(TypedExpr::literal(Value::Int(3), DataType::Int, false)),
            },
            data_type: DataType::Boolean,
            nullable: false,
        });

        optimizer.optimize_match_clause(&mut match_clause);

        assert_eq!(match_clause.patterns[0].relationships[0].min_hops, Some(1));
        assert_eq!(match_clause.patterns[0].relationships[0].max_hops, Some(3));
    }

    #[test]
    fn graph_stats_defaults() {
        let stats = GraphStats::empty();
        assert_eq!(stats.node_count("Unknown"), DEFAULT_NODE_COUNT);
        assert_eq!(stats.edge_count("Unknown"), DEFAULT_EDGE_COUNT);
        assert!((stats.out_degree("Unknown") - DEFAULT_DEGREE).abs() < f64::EPSILON);
        assert!((stats.in_degree("Unknown") - DEFAULT_DEGREE).abs() < f64::EPSILON);
    }

    #[test]
    fn distinct_values_from_persisted_column_stats() {
        let mut stats = GraphStats::empty();
        stats
            .distinct
            .insert(("Person".to_owned(), "name".to_owned()), 950.0);

        assert_eq!(stats.distinct_values(Some("Person"), "name"), Some(950.0));
        // A property without collected stats falls back (None) so the cost
        // model uses its generic equality selectivity.
        assert_eq!(stats.distinct_values(Some("Person"), "missing"), None);
        // No label => cannot key into per-label stats.
        assert_eq!(stats.distinct_values(None, "name"), None);
    }

    #[test]
    fn triple_cardinality_uses_declared_endpoints() {
        let mut stats = GraphStats::empty();
        stats.edge_cardinality.insert("ACTED_IN".to_owned(), 5000);
        stats.edge_endpoints.insert(
            "ACTED_IN".to_owned(),
            ("Person".to_owned(), "Movie".to_owned()),
        );

        // Declared pair (matched case-insensitively) => full edge-table count.
        assert_eq!(
            stats.triple_cardinality(Some("person"), Some("ACTED_IN"), Some("movie")),
            Some(5000.0)
        );
        // Reversed/!declared pair => a label-backed edge type links exactly
        // one label pair, so this genuinely matches no edges.
        assert_eq!(
            stats.triple_cardinality(Some("Movie"), Some("ACTED_IN"), Some("Person")),
            Some(0.0)
        );
        // An unlabelled endpoint is not a typed triple => fall back so the
        // cost model uses the per-type total instead of assuming zero.
        assert_eq!(
            stats.triple_cardinality(None, Some("ACTED_IN"), Some("Movie")),
            None
        );
        // Unknown / un-analyzed edge type => fall back.
        assert_eq!(
            stats.triple_cardinality(Some("Person"), Some("KNOWS"), Some("Person")),
            None
        );
    }

    #[test]
    fn analyzed_distinct_count_makes_cbo_seed_the_selective_node() {
        // Two equally-sized labelled endpoints joined by one relationship,
        // with an equality predicate on the *second* node. Without real
        // distinct stats the planner has no reason to seed from `b`; with a
        // high distinct count the predicate becomes very selective, so the
        // cost-based planner must seed from `b` (forcing a reversal).
        let mut stats = GraphStats::empty();
        stats.label_cardinality.insert("Person".to_owned(), 100_000);
        stats.edge_cardinality.insert("KNOWS".to_owned(), 500_000);
        stats.edge_endpoints.insert(
            "KNOWS".to_owned(),
            ("Person".to_owned(), "Person".to_owned()),
        );
        stats
            .distinct
            .insert(("Person".to_owned(), "email".to_owned()), 100_000.0);
        let optimizer = GraphOptimizer::new(stats);

        let pattern = make_pattern(
            vec![
                make_node(Some("a"), Some("Person")),
                make_node(Some("b"), Some("Person")),
            ],
            vec![make_rel(None, Some("KNOWS"), CypherRelDirection::Outgoing)],
        );
        let mut match_clause = make_match(vec![pattern]);
        match_clause.filter = Some(TypedExpr::binary_eq(
            TypedExpr::column_ref("b.email", 0, DataType::Text, false),
            TypedExpr::literal(Value::Text("x@example.com".into()), DataType::Text, false),
        ));

        assert_eq!(
            optimizer.cbo_seed_index(&match_clause.patterns[0], match_clause.filter.as_slice()),
            Some(1)
        );
        optimizer.optimize_match_clause(&mut match_clause);

        // Seed flipped to the highly-selective `b`.
        assert_eq!(
            match_clause.patterns[0].nodes[0].variable.as_deref(),
            Some("b")
        );
        assert_eq!(
            match_clause.patterns[0].relationships[0].direction,
            CypherRelDirection::Incoming
        );
    }

    #[test]
    fn full_optimize_does_not_panic() {
        let stats = make_stats();
        let optimizer = GraphOptimizer::new(stats);

        let mut plan = empty_plan();
        plan.matches = vec![make_match(vec![
            make_pattern(
                vec![
                    make_node(Some("p"), Some("Person")),
                    make_node(Some("m"), Some("Movie")),
                ],
                vec![make_rel(
                    None,
                    Some("ACTED_IN"),
                    CypherRelDirection::Outgoing,
                )],
            ),
            make_pattern(vec![make_node(Some("c"), Some("City"))], vec![]),
        ])];

        let optimized = optimizer.optimize_cypher_query(plan);

        // City (card=50) should be first pattern after reordering.
        assert_eq!(
            optimized.matches[0].patterns[0].nodes[0].label.as_deref(),
            Some("City")
        );
    }

    #[test]
    fn full_optimize_recurses_into_call_subqueries_and_union() {
        let stats = make_stats();
        let optimizer = GraphOptimizer::new(stats);

        let mut subquery = empty_plan();
        subquery
            .pipeline
            .push(CypherPipelineOp::Match(make_match(vec![
                make_pattern(vec![make_node(Some("p"), Some("Person"))], vec![]),
                make_pattern(vec![make_node(Some("c"), Some("City"))], vec![]),
            ])));

        let mut union_right = empty_plan();
        union_right
            .pipeline
            .push(CypherPipelineOp::Match(make_match(vec![
                make_pattern(vec![make_node(Some("p"), Some("Person"))], vec![]),
                make_pattern(vec![make_node(Some("m"), Some("Movie"))], vec![]),
            ])));
        subquery.union = Some(Box::new(aiondb_plan::graph::CypherUnionPlan {
            all: false,
            right: union_right,
        }));

        let mut plan = empty_plan();
        plan.pipeline
            .push(CypherPipelineOp::CallSubquery(Box::new(subquery)));

        let optimized = optimizer.optimize_cypher_query(plan);
        let CypherPipelineOp::CallSubquery(subquery) = &optimized.pipeline[0] else {
            panic!("expected CALL subquery");
        };
        let CypherPipelineOp::Match(subquery_match) = &subquery.pipeline[0] else {
            panic!("expected optimized nested MATCH");
        };
        assert_eq!(
            subquery_match.patterns[0].nodes[0].label.as_deref(),
            Some("City")
        );

        let union_right = &subquery.union.as_ref().expect("expected UNION").right;
        let CypherPipelineOp::Match(union_match) = &union_right.pipeline[0] else {
            panic!("expected optimized UNION right MATCH");
        };
        assert_eq!(
            union_match.patterns[0].nodes[0].label.as_deref(),
            Some("Movie")
        );
    }

    #[test]
    fn predicate_pushdown_property_equality() {
        let stats = make_stats();
        let optimizer = GraphOptimizer::new(stats);

        // Build a WHERE filter: n.name = 'Alice' encoded as a property access.
        let name_access = TypedExpr {
            kind: TypedExprKind::ScalarFunction {
                func: aiondb_plan::ScalarFunction::Generic("cypher_property_access".into()),
                args: vec![
                    TypedExpr {
                        kind: TypedExprKind::ColumnRef {
                            ordinal: 0,
                            name: "n".into(),
                        },
                        data_type: DataType::Jsonb,
                        nullable: true,
                    },
                    TypedExpr::literal(Value::Text("name".into()), DataType::Text, false),
                ],
            },
            data_type: DataType::Text,
            nullable: true,
        };
        let filter = TypedExpr {
            kind: TypedExprKind::BinaryEq {
                left: Box::new(name_access),
                right: Box::new(TypedExpr::literal(
                    Value::Text("Alice".into()),
                    DataType::Text,
                    false,
                )),
            },
            data_type: DataType::Boolean,
            nullable: false,
        };

        let mut match_clause = make_match(vec![make_pattern(
            vec![make_node(Some("n"), Some("Person"))],
            vec![],
        )]);
        match_clause.filter = Some(filter);

        optimizer.push_predicates_into_patterns(&mut match_clause);

        // The filter should have been pushed into the node's inline properties.
        assert!(match_clause.filter.is_none());
        assert_eq!(match_clause.patterns[0].nodes[0].properties.len(), 1);
        assert_eq!(match_clause.patterns[0].nodes[0].properties[0].key, "name");
    }

    #[test]
    fn predicate_pushdown_dotted_cypher_column_ref_equality() {
        let optimizer = GraphOptimizer::new(make_stats());

        let filter = TypedExpr {
            kind: TypedExprKind::BinaryEq {
                left: Box::new(TypedExpr {
                    kind: TypedExprKind::ColumnRef {
                        name: "b.kind".to_owned(),
                        ordinal: 0,
                    },
                    data_type: DataType::Text,
                    nullable: true,
                }),
                right: Box::new(TypedExpr::literal(
                    Value::Text("dev".to_owned()),
                    DataType::Text,
                    false,
                )),
            },
            data_type: DataType::Boolean,
            nullable: false,
        };

        let mut match_clause = make_match(vec![make_pattern(
            vec![
                make_node(Some("a"), Some("Person")),
                make_node(Some("b"), Some("Person")),
            ],
            vec![make_rel(None, Some("KNOWS"), CypherRelDirection::Outgoing)],
        )]);
        match_clause.filter = Some(filter);

        optimizer.push_predicates_into_patterns(&mut match_clause);

        assert!(match_clause.filter.is_none());
        assert!(match_clause.patterns[0].nodes[0].properties.is_empty());
        assert_eq!(match_clause.patterns[0].nodes[1].properties.len(), 1);
        assert_eq!(match_clause.patterns[0].nodes[1].properties[0].key, "kind");
        assert_eq!(
            match_clause.patterns[0].nodes[1].properties[0].value.kind,
            TypedExprKind::Literal(Value::Text("dev".to_owned())),
        );
    }

    #[test]
    fn predicate_pushdown_keeps_casted_variable_comparison_as_filter() {
        let optimizer = GraphOptimizer::new(make_stats());

        let filter = TypedExpr {
            kind: TypedExprKind::BinaryEq {
                left: Box::new(TypedExpr {
                    kind: TypedExprKind::ColumnRef {
                        name: "n.age".to_owned(),
                        ordinal: 0,
                    },
                    data_type: DataType::Int,
                    nullable: true,
                }),
                right: Box::new(TypedExpr {
                    kind: TypedExprKind::Cast {
                        expr: Box::new(TypedExpr {
                            kind: TypedExprKind::ColumnRef {
                                name: "m.age".to_owned(),
                                ordinal: 1,
                            },
                            data_type: DataType::Text,
                            nullable: true,
                        }),
                        target_type: DataType::Int,
                    },
                    data_type: DataType::Int,
                    nullable: true,
                }),
            },
            data_type: DataType::Boolean,
            nullable: true,
        };

        let mut match_clause = make_match(vec![make_pattern(
            vec![
                make_node(Some("n"), Some("Person")),
                make_node(Some("m"), Some("Person")),
            ],
            vec![make_rel(None, Some("KNOWS"), CypherRelDirection::Outgoing)],
        )]);
        match_clause.filter = Some(filter);

        optimizer.push_predicates_into_patterns(&mut match_clause);

        assert!(match_clause.filter.is_some());
        assert!(match_clause.patterns[0].nodes[0].properties.is_empty());
        assert!(match_clause.patterns[0].nodes[1].properties.is_empty());
    }
}
