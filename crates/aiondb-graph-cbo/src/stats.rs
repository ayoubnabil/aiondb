//! Graph statistics consumed by the cost model.
//!
//! The planner needs only a handful of catalog facts; everything else
//! (degree, fan-out, selectivity) is derived in [`crate::cost`]. [`BaseStats`]
//! mirrors the field names of the existing
//! `aiondb_optimizer::graph_optimizer::GraphStats` so a future adapter is a
//! direct field copy, not a rewrite.

use std::collections::HashMap;

/// Default node count for an un-analyzed label (matches the existing optimizer).
pub const DEFAULT_NODE_COUNT: f64 = 1_000.0;
/// Default relationship count for an un-analyzed type.
pub const DEFAULT_EDGE_COUNT: f64 = 5_000.0;

/// Statistics source for cost estimation.
pub trait GraphStatistics {
    /// Total node count across all labels.
    fn total_nodes(&self) -> f64;
    /// Nodes carrying `label` (or [`total_nodes`](Self::total_nodes) if `None`).
    fn label_cardinality(&self, label: Option<&str>) -> f64;
    /// Relationships of `rel_type` (or all relationships if `None`).
    fn relationship_cardinality(&self, rel_type: Option<&str>) -> f64;
    /// Distinct value count of `label.property`, if analyzed.
    fn distinct_values(&self, label: Option<&str>, property: &str) -> Option<f64>;
    /// Relationships of `rel_type` between a `from`-labelled and a
    /// `to`-labelled node — the typed-triple statistic Neo4j's planner relies
    /// on for accurate fan-out. `None` ⇒ the cost model falls back to
    /// [`relationship_cardinality`](Self::relationship_cardinality).
    fn triple_cardinality(
        &self,
        from: Option<&str>,
        rel_type: Option<&str>,
        to: Option<&str>,
    ) -> Option<f64> {
        let (_, _, _) = (from, rel_type, to);
        None
    }
}

/// In-memory [`GraphStatistics`] implementation.
#[derive(Clone, Debug, Default)]
pub struct BaseStats {
    /// Nodes per label.
    pub label_cardinality: HashMap<String, u64>,
    /// Relationships per type.
    pub edge_cardinality: HashMap<String, u64>,
    /// Distinct values keyed by `(label, property)`.
    pub distinct: HashMap<(String, String), u64>,
    /// Typed-triple edge counts keyed by `(from_label, rel_type, to_label)`.
    pub triple: HashMap<(String, String, String), u64>,
}

impl BaseStats {
    /// Empty statistics (every lookup returns its default).
    pub fn new() -> Self {
        Self::default()
    }
    /// Record a label node count (builder style).
    pub fn with_label(mut self, label: impl Into<String>, count: u64) -> Self {
        self.label_cardinality.insert(label.into(), count);
        self
    }
    /// Record a relationship-type count (builder style).
    pub fn with_edge(mut self, rel_type: impl Into<String>, count: u64) -> Self {
        self.edge_cardinality.insert(rel_type.into(), count);
        self
    }
    /// Record a distinct value count for `label.property` (builder style).
    pub fn with_distinct(
        mut self,
        label: impl Into<String>,
        property: impl Into<String>,
        ndistinct: u64,
    ) -> Self {
        self.distinct
            .insert((label.into(), property.into()), ndistinct);
        self
    }
    /// Record a typed-triple edge count `(from)-[type]->(to)` (builder style).
    pub fn with_triple(
        mut self,
        from: impl Into<String>,
        rel_type: impl Into<String>,
        to: impl Into<String>,
        count: u64,
    ) -> Self {
        self.triple
            .insert((from.into(), rel_type.into(), to.into()), count);
        self
    }
}

impl GraphStatistics for BaseStats {
    fn total_nodes(&self) -> f64 {
        let sum: u64 = self.label_cardinality.values().copied().sum();
        if sum == 0 {
            DEFAULT_NODE_COUNT
        } else {
            sum as f64
        }
    }

    fn label_cardinality(&self, label: Option<&str>) -> f64 {
        match label {
            None => self.total_nodes(),
            Some(l) => self
                .label_cardinality
                .get(l)
                .map(|c| *c as f64)
                .unwrap_or(DEFAULT_NODE_COUNT),
        }
    }

    fn relationship_cardinality(&self, rel_type: Option<&str>) -> f64 {
        match rel_type {
            None => {
                let sum: u64 = self.edge_cardinality.values().copied().sum();
                if sum == 0 {
                    DEFAULT_EDGE_COUNT
                } else {
                    sum as f64
                }
            }
            Some(t) => self
                .edge_cardinality
                .get(t)
                .map(|c| *c as f64)
                .unwrap_or(DEFAULT_EDGE_COUNT),
        }
    }

    fn distinct_values(&self, label: Option<&str>, property: &str) -> Option<f64> {
        let label = label?;
        self.distinct
            .get(&(label.to_owned(), property.to_owned()))
            .map(|d| *d as f64)
    }

    fn triple_cardinality(
        &self,
        from: Option<&str>,
        rel_type: Option<&str>,
        to: Option<&str>,
    ) -> Option<f64> {
        let (f, t, d) = (from?, rel_type?, to?);
        self.triple
            .get(&(f.to_owned(), t.to_owned(), d.to_owned()))
            .map(|c| *c as f64)
    }
}
