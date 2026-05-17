//! Cardinality and cost model.
//!
//! Estimates derive from catalog facts ([`crate::stats`]): per-row scan cost,
//! index-seek startup + per-row cost, and per-edge expansion cost, with
//! selectivities defaulted from predicate shape when no histogram exists. Edge
//! fan-out prefers the *typed-triple* statistic `count((:A)-[:T]->(:B))` — the
//! same statistic Neo4j's planner relies on — and only falls back to per-type
//! totals when it is unavailable. All arithmetic is saturated and finite, so a
//! degenerate or hostile pattern can never produce `NaN`/`inf`.

use crate::query_graph::{IndexKind, PredicateOp, PropertyPredicate, QueryNode};
use crate::stats::GraphStatistics;

/// How a node will be seeded (the planner maps this to a physical op).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SeedOpKind {
    AllNodes,
    Label,
    IndexUnique,
    IndexEq,
    IndexRange,
}

/// A seed candidate with its estimated output rows and cost.
#[derive(Clone, Copy, Debug)]
pub(crate) struct SeedChoice {
    pub kind: SeedOpKind,
    pub rows: f64,
    pub cost: f64,
}

/// Inputs describing the relationship being expanded.
pub(crate) struct ExpandInput<'a> {
    pub in_rows: f64,
    pub rel_type: Option<&'a str>,
    /// Label of the relationship's schema `from` endpoint.
    pub schema_from: Option<&'a str>,
    /// Label of the relationship's schema `to` endpoint.
    pub schema_to: Option<&'a str>,
    /// `true` when traversal starts at the schema `from` endpoint.
    pub start_is_from: bool,
    /// Combined selectivity of the destination node's predicates.
    pub dst_sel: f64,
    /// Combined selectivity of the relationship's own predicates.
    pub rel_sel: f64,
    /// Direction is `Both` (traversable either way).
    pub both: bool,
    /// Variable-length `(min, max)`; `max == None` ⇒ unbounded (cost-capped).
    pub var: Option<(u32, Option<u32>)>,
    /// The far endpoint is already bound (cycle close): expand is a filter.
    pub into: bool,
}

/// Tunable cost constants. Units are relative; only ratios matter. Defaults
/// make an index seek beat a label scan beat an all-nodes scan, and a
/// selective expand beat a fan-out expand.
#[derive(Clone, Copy, Debug)]
pub struct CostModel {
    all_nodes_scan_per_row: f64,
    label_scan_per_row: f64,
    index_seek_startup: f64,
    index_seek_per_row: f64,
    expand_per_edge: f64,
    /// Hop cap applied to open-ended variable-length relationships.
    var_length_cap: u32,
}

impl Default for CostModel {
    fn default() -> Self {
        Self {
            all_nodes_scan_per_row: 1.2,
            label_scan_per_row: 1.0,
            index_seek_startup: 2.0,
            index_seek_per_row: 0.25,
            expand_per_edge: 1.5,
            var_length_cap: 8,
        }
    }
}

/// Largest cardinality the model ever reports (prevents overflow to `inf`).
const CARD_CAP: f64 = 1e15;
const GENERIC_EQ_SEL: f64 = 0.1;
const RANGE_SEL: f64 = 0.3;
const PREFIX_SEL: f64 = 0.1;
const OTHER_SEL: f64 = 0.25;

#[inline]
fn clamp_card(v: f64) -> f64 {
    if !v.is_finite() || v < 0.0 {
        0.0
    } else if v > CARD_CAP {
        CARD_CAP
    } else {
        v
    }
}

#[inline]
fn pos(v: f64) -> f64 {
    if v.is_finite() && v > 1.0 {
        v
    } else {
        1.0
    }
}

impl CostModel {
    fn predicate_selectivity(
        op: PredicateOp,
        label: Option<&str>,
        property: &str,
        stats: &dyn GraphStatistics,
    ) -> f64 {
        match op {
            PredicateOp::Equality => match stats.distinct_values(label, property) {
                Some(d) if d >= 1.0 => 1.0 / d,
                _ => GENERIC_EQ_SEL,
            },
            PredicateOp::Range => RANGE_SEL,
            PredicateOp::Prefix => PREFIX_SEL,
            PredicateOp::Other => OTHER_SEL,
        }
    }

    /// Combined selectivity of `preds`, skipping the property already covered
    /// by an index seed. Predicates are assumed independent (Neo4j does too).
    pub(crate) fn predicates_selectivity(
        preds: &[PropertyPredicate],
        label: Option<&str>,
        skip_property: Option<&str>,
        stats: &dyn GraphStatistics,
    ) -> f64 {
        let mut sel = 1.0_f64;
        for p in preds {
            if skip_property == Some(p.property.as_str()) {
                continue;
            }
            sel *= Self::predicate_selectivity(p.op, label, &p.property, stats);
        }
        sel.clamp(0.0, 1.0)
    }

    /// Combined selectivity of a node's predicates (used by the planner).
    pub(crate) fn node_selectivity(node: &QueryNode, stats: &dyn GraphStatistics) -> f64 {
        Self::predicates_selectivity(&node.predicates, node.label.as_deref(), None, stats)
    }

    /// Cheapest way to materialize `node` standalone: an index seek if present
    /// and competitive, else a label or all-nodes scan.
    pub(crate) fn seed(&self, node: &QueryNode, stats: &dyn GraphStatistics) -> SeedChoice {
        let label = node.label.as_deref();
        let base = stats.label_cardinality(label);
        let scan_kind = if label.is_some() {
            SeedOpKind::Label
        } else {
            SeedOpKind::AllNodes
        };
        let scan_per_row = if label.is_some() {
            self.label_scan_per_row
        } else {
            self.all_nodes_scan_per_row
        };
        let scan_sel = Self::predicates_selectivity(&node.predicates, label, None, stats);
        let mut best = SeedChoice {
            kind: scan_kind,
            rows: clamp_card(base * scan_sel),
            cost: clamp_card(base * scan_per_row),
        };

        if let Some(idx) = &node.index {
            let (kind, seek_base) = match idx.kind {
                IndexKind::Unique => (SeedOpKind::IndexUnique, 1.0),
                IndexKind::NonUnique => (
                    SeedOpKind::IndexEq,
                    base * Self::predicate_selectivity(
                        PredicateOp::Equality,
                        label,
                        &idx.property,
                        stats,
                    ),
                ),
                IndexKind::Range => (SeedOpKind::IndexRange, base * RANGE_SEL),
            };
            let residual =
                Self::predicates_selectivity(&node.predicates, label, Some(&idx.property), stats);
            let rows = clamp_card(seek_base * residual);
            // Startup + B-tree descent (≈ log2 of the indexed cardinality) +
            // per-row fetch of the matched range.
            let descent = (base + 2.0).log2().max(1.0);
            let cost =
                clamp_card(self.index_seek_startup + descent + seek_base * self.index_seek_per_row);
            if cost < best.cost {
                best = SeedChoice { kind, rows, cost };
            }
        }
        best
    }

    /// Average fan-out of one hop, preferring the typed-triple statistic.
    fn degree(i: &ExpandInput, stats: &dyn GraphStatistics) -> f64 {
        let edges = stats
            .triple_cardinality(i.schema_from, i.rel_type, i.schema_to)
            .unwrap_or_else(|| stats.relationship_cardinality(i.rel_type));
        let (start_label, other_label) = if i.start_is_from {
            (i.schema_from, i.schema_to)
        } else {
            (i.schema_to, i.schema_from)
        };
        let primary = edges / pos(stats.label_cardinality(start_label));
        if i.both {
            primary + edges / pos(stats.label_cardinality(other_label))
        } else {
            primary
        }
    }

    /// Effective fan-out over a (possibly variable-length) hop: the geometric
    /// series of per-hop degree with a hard hop cap.
    fn hop_multiplier(&self, degree: f64, min: u32, max: Option<u32>) -> f64 {
        let d = pos(degree);
        let lo = min.max(1);
        let hi = max
            .unwrap_or(self.var_length_cap)
            .min(self.var_length_cap)
            .max(lo);
        let mut term = d.powi(lo as i32);
        let mut acc = term;
        for _ in (lo + 1)..=hi {
            term *= d;
            acc += term;
            if acc > CARD_CAP {
                break;
            }
        }
        clamp_card(acc)
    }

    /// Output rows and incremental cost of one expansion.
    pub(crate) fn expand(&self, i: &ExpandInput, stats: &dyn GraphStatistics) -> (f64, f64) {
        let degree = Self::degree(i, stats);
        let mult = match i.var {
            Some((min, max)) => self.hop_multiplier(degree, min, max),
            None => clamp_card(degree),
        };
        let edges_touched = clamp_card(i.in_rows * mult);
        let sel = i.dst_sel.clamp(0.0, 1.0) * i.rel_sel.clamp(0.0, 1.0);
        let out = if i.into {
            let end_label = if i.start_is_from {
                i.schema_to
            } else {
                i.schema_from
            };
            let end_card = pos(stats.label_cardinality(end_label));
            clamp_card(i.in_rows * (mult / end_card) * sel)
        } else {
            clamp_card(i.in_rows * mult * sel)
        };
        let cost = clamp_card(edges_touched * self.expand_per_edge + out);
        (out, cost)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::stats::BaseStats;

    fn input<'a>(stats_both: bool) -> ExpandInput<'a> {
        ExpandInput {
            in_rows: 100.0,
            rel_type: Some("KNOWS"),
            schema_from: Some("Person"),
            schema_to: Some("Person"),
            start_is_from: true,
            dst_sel: 1.0,
            rel_sel: 1.0,
            both: stats_both,
            var: None,
            into: false,
        }
    }

    #[test]
    fn equality_selectivity_uses_distinct_count() {
        let stats = BaseStats::new().with_distinct("P", "k", 50);
        assert!(
            (CostModel::predicate_selectivity(PredicateOp::Equality, Some("P"), "k", &stats)
                - 0.02)
                .abs()
                < 1e-9
        );
        // Unknown distinct ⇒ conservative default.
        assert!(
            (CostModel::predicate_selectivity(PredicateOp::Equality, Some("P"), "x", &stats)
                - GENERIC_EQ_SEL)
                .abs()
                < 1e-9
        );
    }

    #[test]
    fn triple_statistic_overrides_total_for_degree() {
        let cm = CostModel::default();
        let coarse = BaseStats::new()
            .with_label("Person", 1_000)
            .with_edge("KNOWS", 50_000);
        let precise = coarse
            .clone()
            .with_triple("Person", "KNOWS", "Person", 2_000);
        let (coarse_rows, _) = cm.expand(&input(false), &coarse);
        let (precise_rows, _) = cm.expand(&input(false), &precise);
        // Coarse degree = 50000/1000 = 50; precise = 2000/1000 = 2.
        assert!((coarse_rows - 5_000.0).abs() < 1.0);
        assert!((precise_rows - 200.0).abs() < 1.0);
        assert!(precise_rows < coarse_rows);
    }

    #[test]
    fn both_direction_sums_in_and_out_degree() {
        let cm = CostModel::default();
        let stats = BaseStats::new()
            .with_label("Person", 1_000)
            .with_triple("Person", "KNOWS", "Person", 4_000);
        let (one, _) = cm.expand(&input(false), &stats);
        let (both, _) = cm.expand(&input(true), &stats);
        assert!(both > one, "Both must traverse at least as many edges");
    }

    #[test]
    fn expand_is_monotone_in_input_rows_and_finite() {
        let cm = CostModel::default();
        let stats = BaseStats::new().with_label("N", 100).with_edge("E", 10_000);
        let mut a = input(false);
        a.schema_from = Some("N");
        a.schema_to = Some("N");
        a.rel_type = Some("E");
        let (r1, c1) = cm.expand(&a, &stats);
        a.in_rows = 1_000.0;
        let (r2, c2) = cm.expand(&a, &stats);
        assert!(r2 > r1 && c2 > c1);
        assert!(r2.is_finite() && c2.is_finite());
    }

    #[test]
    fn index_seek_scan_crossover_is_realistic() {
        use crate::query_graph::{IndexKind, QueryNode};
        let cm = CostModel::default();
        // A tiny label: a full scan must beat an index seek (seek startup +
        // B-tree descent outweighs touching a handful of rows).
        let tiny = BaseStats::new().with_label("T", 2);
        let n = QueryNode::labelled(0, "T").with_index("id", IndexKind::Unique);
        assert_eq!(cm.seed(&n, &tiny).kind, SeedOpKind::Label);
        // A large label: the index seek must win decisively.
        let big = BaseStats::new().with_label("T", 5_000_000);
        assert_eq!(cm.seed(&n, &big).kind, SeedOpKind::IndexUnique);
    }

    #[test]
    fn unbounded_var_length_is_capped() {
        let cm = CostModel::default();
        let stats = BaseStats::new()
            .with_label("N", 10)
            .with_edge("E", 1_000_000);
        let mut a = input(false);
        a.schema_from = Some("N");
        a.schema_to = Some("N");
        a.rel_type = Some("E");
        a.var = Some((1, None));
        let (rows, cost) = cm.expand(&a, &stats);
        assert!(rows.is_finite() && rows <= CARD_CAP);
        assert!(cost.is_finite() && cost <= CARD_CAP);
    }
}
