//! The cost-based search itself.
//!
//! Each connected component is planned by bounded dynamic programming over its
//! relationships — Selinger / Neo4j-IDP style: build the cheapest plan for
//! every reachable sub-pattern bottom-up, extending one adjacent relationship
//! at a time. Above a safe size it degrades to a deterministic, still
//! cost-driven greedy search so a pathological MATCH can never exhaust memory
//! or time. Disconnected components are combined by cartesian product,
//! cheapest first.

use std::rc::Rc;

use crate::cost::{CostModel, ExpandInput};
use crate::plan::{ExpansionPlan, PhysicalOp};
use crate::query_graph::{GraphError, NodeId, QueryGraph, QueryNode, RelDirection, RelId};
use crate::stats::GraphStatistics;

/// Why planning could not produce a plan.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PlanError {
    /// The pattern has no nodes.
    EmptyGraph,
    /// The pattern failed structural validation.
    Invalid(GraphError),
}

impl std::fmt::Display for PlanError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::EmptyGraph => write!(f, "query graph has no nodes"),
            Self::Invalid(e) => write!(f, "invalid query graph: {e}"),
        }
    }
}

impl std::error::Error for PlanError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Invalid(e) => Some(e),
            Self::EmptyGraph => None,
        }
    }
}

/// Planner tuning knobs.
#[derive(Clone, Copy, Debug)]
pub struct PlannerConfig {
    /// Largest component relationship count for which exhaustive DP is used.
    /// Above this the greedy strategy takes over. Hard-capped at 14 internally
    /// so `2^k * frontier_cap` sub-plans can never exhaust memory regardless
    /// of the configured value.
    pub max_dp_rels: usize,
    /// Largest component for which bushy `HashJoin` plans are also enumerated
    /// (subset DP, `O(3^k)`). Hard-capped at 10 internally so the extra search
    /// can never blow up. Set to 0 to disable bushy planning.
    pub bushy_max: usize,
    /// Number of `(cost, rows)`-nondominated sub-plans retained per relation
    /// set. Pareto pruning is exact; the *cap* is the only approximation in
    /// the search — when a set has more nondominated plans than this, the
    /// cheapest `cap` plus the global minimum-rows plan are kept (so
    /// `DP ≤ greedy` and `bushy ≤ left-deep` still hold, but the true global
    /// optimum is only guaranteed when frontiers stay within the cap). Larger
    /// = closer to optimal, more memory/time. Clamped to `2..=32`.
    pub frontier_cap: usize,
    /// Cost model used for all estimates.
    pub cost: CostModel,
}

impl Default for PlannerConfig {
    fn default() -> Self {
        Self {
            max_dp_rels: 12,
            bushy_max: 8,
            frontier_cap: 16,
            cost: CostModel::default(),
        }
    }
}

/// Plan `graph` against `stats`, returning the cheapest traversal plan.
pub fn plan_query_graph(
    graph: &QueryGraph,
    stats: &dyn GraphStatistics,
    cfg: &PlannerConfig,
) -> Result<ExpansionPlan, PlanError> {
    graph.validate().map_err(PlanError::Invalid)?;
    if graph.nodes.is_empty() {
        return Err(PlanError::EmptyGraph);
    }

    // Union-find over nodes to discover connected components.
    let mut parent: Vec<usize> = (0..graph.nodes.len()).collect();
    for r in &graph.rels {
        union(&mut parent, r.from.0, r.to.0);
    }
    let mut comp_rels: Vec<Vec<RelId>> = vec![Vec::new(); graph.nodes.len()];
    for r in &graph.rels {
        let root = find(&mut parent, r.from.0);
        comp_rels[root].push(r.id);
    }
    let mut comp_nodes: Vec<Vec<NodeId>> = vec![Vec::new(); graph.nodes.len()];
    for n in &graph.nodes {
        let root = find(&mut parent, n.id.0);
        comp_nodes[root].push(n.id);
    }

    // Plan every non-empty component, keeping its full Pareto frontier rather
    // than collapsing to one plan: a costlier-but-smaller component plan can be
    // far cheaper once multiplied into a cartesian product, so the global
    // optimum is only known after components are combined.
    // `2^dp_cap * frontier_cap` sub-plans is the hard memory bound; 14 keeps
    // the table comfortably small whatever `max_dp_rels` is configured to.
    let dp_cap = cfg.max_dp_rels.min(14);
    let cap = cfg.frontier_cap.clamp(2, 32);
    let mut acc: Option<Vec<ExpansionPlan>> = None;
    for (root, nodes) in comp_nodes.iter().enumerate() {
        if nodes.is_empty() {
            continue;
        }
        let rels = &comp_rels[root];
        let frontier = if rels.is_empty() {
            vec![seed_plan(&graph.nodes[nodes[0].0], stats, cfg)]
        } else if rels.len() <= dp_cap {
            plan_component_dp(graph, rels, stats, cfg, cap)
        } else {
            vec![plan_component_greedy(graph, rels, stats, cfg)]
        };
        acc = Some(match acc {
            None => frontier,
            Some(left) => cartesian_frontier(&left, &frontier, cap),
        });
    }

    acc.and_then(|f| f.into_iter().min_by(|a, b| cmp_plan(a, b)))
        .ok_or(PlanError::EmptyGraph)
}

/// Combine two component frontiers by cartesian product, keeping the Pareto
/// frontier of the results so the globally cheapest combination survives.
fn cartesian_frontier(
    left: &[ExpansionPlan],
    right: &[ExpansionPlan],
    cap: usize,
) -> Vec<ExpansionPlan> {
    let mut out: Vec<ExpansionPlan> = Vec::new();
    for a in left {
        for b in right {
            let rows = clamp(a.rows * b.rows);
            let cost = clamp(a.cost + b.cost + a.rows * b.rows);
            add_to_frontier(
                &mut out,
                ExpansionPlan {
                    op: PhysicalOp::CartesianProduct {
                        left: Rc::new(a.clone()),
                        right: Rc::new(b.clone()),
                    },
                    rows,
                    cost,
                },
                cap,
            );
        }
    }
    out
}

// --- Seeds ---

fn seed_plan(node: &QueryNode, stats: &dyn GraphStatistics, cfg: &PlannerConfig) -> ExpansionPlan {
    use crate::cost::SeedOpKind as K;
    let s = cfg.cost.seed(node, stats);
    let op = match s.kind {
        K::AllNodes => PhysicalOp::AllNodesScan { node: node.id },
        K::Label => PhysicalOp::NodeByLabelScan {
            node: node.id,
            label: node.label.clone().unwrap_or_default(),
        },
        K::IndexUnique | K::IndexEq | K::IndexRange => PhysicalOp::NodeIndexSeek {
            node: node.id,
            property: node
                .index
                .as_ref()
                .map(|i| i.property.clone())
                .unwrap_or_default(),
            unique: s.kind == K::IndexUnique,
            range: s.kind == K::IndexRange,
        },
    };
    ExpansionPlan::leaf(op, s.rows, s.cost)
}

// --- Expansion step shared by DP and greedy ---

/// Extend `input` by relationship `rid`, starting from whichever endpoint is
/// already bound. `bound` reports nodes the input plan already produces.
fn expand_step(
    graph: &QueryGraph,
    input: &ExpansionPlan,
    rid: RelId,
    bound: &[bool],
    stats: &dyn GraphStatistics,
    cfg: &PlannerConfig,
) -> Option<ExpansionPlan> {
    let rel = &graph.rels[rid.0];
    let from_bound = bound[rel.from.0];
    let to_bound = bound[rel.to.0];
    let (start, end, direction) = if from_bound {
        (rel.from, rel.to, rel.direction)
    } else if to_bound {
        (rel.to, rel.from, rel.direction.reversed())
    } else {
        return None; // not adjacent to the bound set
    };
    let into = bound[end.0];
    let end_node = &graph.nodes[end.0];
    let dst_sel = if into {
        1.0
    } else {
        CostModel::node_selectivity(end_node, stats)
    };
    let ei = ExpandInput {
        in_rows: input.rows,
        rel_type: rel.rel_type.as_deref(),
        schema_from: graph.nodes[rel.from.0].label.as_deref(),
        schema_to: graph.nodes[rel.to.0].label.as_deref(),
        start_is_from: start == rel.from,
        dst_sel,
        rel_sel: CostModel::predicates_selectivity(&rel.predicates, None, None, stats),
        both: matches!(rel.direction, RelDirection::Both),
        var: rel.var_length.map(|v| (v.min, v.max)),
        into,
    };
    let (rows, ecost) = cfg.cost.expand(&ei, stats);
    Some(ExpansionPlan {
        op: PhysicalOp::Expand {
            input: Rc::new(input.clone()),
            rel: rid,
            from: start,
            to: end,
            direction,
            into,
            var_length: rel.var_length,
        },
        rows,
        cost: clamp(input.cost + ecost),
    })
}

// --- Exhaustive DP (IDP-1: extend one adjacent edge at a time) ---
//
// Soundness note: per relation-set the model's incremental expand cost and
// output rows both depend on the *cardinality* of the sub-plan, not just its
// accumulated cost. Keeping only the single cheapest sub-plan per set would
// therefore violate Bellman optimality (a costlier-but-smaller sub-plan can
// extend more cheaply). We instead keep the Pareto frontier of
// `(cost, rows)`-nondominated sub-plans, which preserves optimality because
// every future expand is monotone in both quantities. The frontier is capped
// so the table size stays bounded.

fn plan_component_dp(
    graph: &QueryGraph,
    rels: &[RelId],
    stats: &dyn GraphStatistics,
    cfg: &PlannerConfig,
    cap: usize,
) -> Vec<ExpansionPlan> {
    let k = rels.len();
    let full = (1u32 << k) - 1;
    let mut dp: Vec<Vec<ExpansionPlan>> = vec![Vec::new(); 1usize << k];
    // Memoize the bound-node set of every relation subset once, instead of
    // recomputing it in the O(3^k) bushy loop and every extension step.
    let bsets: Vec<Vec<bool>> = (0..1usize << k)
        .map(|m| bound_set(graph, rels, m as u32))
        .collect();

    // Bases: each relationship seeded from each of its two endpoints.
    for (bit, rid) in rels.iter().enumerate() {
        let rel = &graph.rels[rid.0];
        for &seed_node in &[rel.from, rel.to] {
            let seed = seed_plan(&graph.nodes[seed_node.0], stats, cfg);
            let mut bound = vec![false; graph.nodes.len()];
            bound[seed_node.0] = true;
            if let Some(p) = expand_step(graph, &seed, *rid, &bound, stats, cfg) {
                add_to_frontier(&mut dp[1usize << bit], p, cap);
            }
        }
    }

    let bushy_cap = cfg.bushy_max.min(10);

    // Predecessor mask < successor mask ⇒ ascending order is a valid schedule:
    // when we reach `mask`, every proper submask is already final, so both the
    // bushy subset-joins and the left-deep extensions below see complete data.
    for mask in 1u32..=full {
        // Bushy: join two independently-planned, node-sharing sub-patterns.
        if k <= bushy_cap && mask.count_ones() >= 2 {
            let mut sub = (mask - 1) & mask;
            while sub != 0 {
                let other = mask ^ sub;
                if sub < other && !dp[sub as usize].is_empty() && !dp[other as usize].is_empty() {
                    let shared = shared_nodes(&bsets[sub as usize], &bsets[other as usize]);
                    if !shared.is_empty() {
                        let (lhs, rhs) = (dp[sub as usize].clone(), dp[other as usize].clone());
                        for a in &lhs {
                            for b in &rhs {
                                let p = hash_join_plan(a, b, &shared, graph, stats);
                                add_to_frontier(&mut dp[mask as usize], p, cap);
                            }
                        }
                    }
                }
                sub = (sub - 1) & mask;
            }
        }

        if dp[mask as usize].is_empty() {
            continue;
        }
        let bound = &bsets[mask as usize];
        let plans = dp[mask as usize].clone();
        for plan in &plans {
            for (bit, rid) in rels.iter().enumerate() {
                let bitmask = 1u32 << bit;
                if mask & bitmask != 0 {
                    continue;
                }
                if let Some(p) = expand_step(graph, plan, *rid, &bound, stats, cfg) {
                    add_to_frontier(&mut dp[(mask | bitmask) as usize], p, cap);
                }
            }
        }
    }

    // Return the whole frontier; the caller picks the global optimum after
    // combining components. Empty only if the component is degenerate, in
    // which case the deterministic greedy plan keeps the planner total.
    let frontier = std::mem::take(&mut dp[full as usize]);
    if frontier.is_empty() {
        vec![plan_component_greedy(graph, rels, stats, cfg)]
    } else {
        frontier
    }
}

/// Nodes bound by *both* relationship subsets, ascending — the equi-join keys
/// of a candidate hash join. Empty ⇒ the two sides are independent (a
/// cartesian, handled elsewhere), so no hash join is formed.
fn shared_nodes(a: &[bool], b: &[bool]) -> Vec<NodeId> {
    (0..a.len()).filter(|&n| a[n] && b[n]).map(NodeId).collect()
}

/// Build a `HashJoin` of two node-sharing sub-plans. The shared nodes are an
/// equi-join on identity, so output is the product divided by the shared
/// labels' cardinalities; the smaller side is hashed (build), the larger
/// probed.
fn hash_join_plan(
    a: &ExpansionPlan,
    b: &ExpansionPlan,
    shared: &[NodeId],
    graph: &QueryGraph,
    stats: &dyn GraphStatistics,
) -> ExpansionPlan {
    let mut denom = 1.0_f64;
    for s in shared {
        denom *= stats
            .label_cardinality(graph.nodes[s.0].label.as_deref())
            .max(1.0);
    }
    let rows = clamp(a.rows * b.rows / denom);
    // Hash the smaller side. Building the hash table (insert + memory) is
    // dearer per row than probing, so a large build side is penalised — this
    // is what makes the planner prefer a genuinely small build, exactly like
    // Postgres/Neo4j hash-join costing.
    let (build, probe) = if a.rows <= b.rows { (a, b) } else { (b, a) };
    const HASH_BUILD: f64 = 1.5;
    const HASH_PROBE: f64 = 1.0;
    let cost =
        clamp(build.cost + probe.cost + HASH_BUILD * build.rows + HASH_PROBE * probe.rows + rows);
    ExpansionPlan {
        op: PhysicalOp::HashJoin {
            left: Rc::new(build.clone()),
            right: Rc::new(probe.clone()),
            on: shared[0],
        },
        rows,
        cost,
    }
}

/// Nodes bound by the relationships selected in `mask`.
fn bound_set(graph: &QueryGraph, rels: &[RelId], mask: u32) -> Vec<bool> {
    let mut bound = vec![false; graph.nodes.len()];
    for (bit, rid) in rels.iter().enumerate() {
        if mask & (1u32 << bit) != 0 {
            let r = &graph.rels[rid.0];
            bound[r.from.0] = true;
            bound[r.to.0] = true;
        }
    }
    bound
}

// --- Greedy fallback (deterministic, still cost-driven) ---

fn plan_component_greedy(
    graph: &QueryGraph,
    rels: &[RelId],
    stats: &dyn GraphStatistics,
    cfg: &PlannerConfig,
) -> ExpansionPlan {
    // Start from the cheapest seed reachable in this component.
    let mut node_ids: Vec<NodeId> = Vec::new();
    for rid in rels {
        let r = &graph.rels[rid.0];
        for n in [r.from, r.to] {
            if !node_ids.contains(&n) {
                node_ids.push(n);
            }
        }
    }
    let mut best_seed: Option<(NodeId, ExpansionPlan)> = None;
    for n in &node_ids {
        let p = seed_plan(&graph.nodes[n.0], stats, cfg);
        if best_seed
            .as_ref()
            .map_or(true, |(_, bp)| cmp_plan(&p, bp).is_lt())
        {
            best_seed = Some((*n, p));
        }
    }
    // `node_ids` is non-empty here; the fallback only keeps the fn total.
    let (seed_node, mut plan) = best_seed.unwrap_or_else(|| {
        let n = graph.nodes[0].id;
        (n, seed_plan(&graph.nodes[0], stats, cfg))
    });
    let mut bound = vec![false; graph.nodes.len()];
    bound[seed_node.0] = true;
    let mut remaining: Vec<RelId> = rels.to_vec();

    while !remaining.is_empty() {
        let mut pick: Option<(usize, ExpansionPlan)> = None;
        for (idx, rid) in remaining.iter().enumerate() {
            if let Some(p) = expand_step(graph, &plan, *rid, &bound, stats, cfg) {
                if pick
                    .as_ref()
                    .map_or(true, |(_, bp)| cmp_plan(&p, bp).is_lt())
                {
                    pick = Some((idx, p));
                }
            }
        }
        let (idx, p) = match pick {
            Some(v) => v,
            // Unreachable for a well-formed component; degrade safely.
            None => break,
        };
        let rid = remaining.remove(idx);
        let r = &graph.rels[rid.0];
        bound[r.from.0] = true;
        bound[r.to.0] = true;
        plan = p;
    }
    plan
}

// --- Helpers ---

fn find(parent: &mut [usize], x: usize) -> usize {
    let mut x = x;
    while parent[x] != x {
        parent[x] = parent[parent[x]];
        x = parent[x];
    }
    x
}

fn union(parent: &mut [usize], a: usize, b: usize) {
    let ra = find(parent, a);
    let rb = find(parent, b);
    if ra != rb {
        parent[ra] = rb;
    }
}

/// Deterministic plan ordering: cheaper cost wins; ties broken by fewer rows
/// then by structural shape so the planner is stable across runs.
fn cmp_plan(a: &ExpansionPlan, b: &ExpansionPlan) -> std::cmp::Ordering {
    use std::cmp::Ordering::Equal;
    a.cost
        .partial_cmp(&b.cost)
        .unwrap_or(Equal)
        .then(a.rows.partial_cmp(&b.rows).unwrap_or(Equal))
}

/// Insert `cand` into a relation set's Pareto frontier, dropping plans it
/// dominates and itself if dominated. The frontier is capped, but the
/// globally smallest-rows plan is always retained because it is the one that
/// extends most cheaply downstream — losing it is what would let greedy beat
/// the supposedly exhaustive search.
fn add_to_frontier(frontier: &mut Vec<ExpansionPlan>, cand: ExpansionPlan, cap: usize) {
    if frontier
        .iter()
        .any(|e| e.cost <= cand.cost && e.rows <= cand.rows)
    {
        return;
    }
    frontier.retain(|e| !(cand.cost <= e.cost && cand.rows <= e.rows));
    frontier.push(cand);
    if frontier.len() > cap {
        frontier.sort_by(|a, b| cmp_plan(a, b));
        let min_rows = frontier
            .iter()
            .enumerate()
            .min_by(|(_, a), (_, b)| {
                a.rows
                    .partial_cmp(&b.rows)
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
            .map(|(i, _)| i)
            .unwrap_or(0);
        if min_rows >= cap {
            let keep = frontier.remove(min_rows);
            frontier.truncate(cap - 1);
            frontier.push(keep);
        } else {
            frontier.truncate(cap);
        }
    }
}

fn clamp(v: f64) -> f64 {
    if !v.is_finite() || v < 0.0 {
        0.0
    } else if v > 1e15 {
        1e15
    } else {
        v
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::query_graph::{IndexKind, QueryRel};
    use crate::stats::BaseStats;

    fn leaf(p: &ExpansionPlan) -> &PhysicalOp {
        match &p.op {
            PhysicalOp::Expand { input, .. } => leaf(input),
            PhysicalOp::CartesianProduct { left, .. } | PhysicalOp::HashJoin { left, .. } => {
                leaf(left)
            }
            op => op,
        }
    }

    /// The headline guarantee: on a pattern where the naive "start at the
    /// first MATCH node and expand left-to-right" order (what an engine with
    /// no/poor statistics, e.g. an unconfigured Neo4j, effectively does)
    /// blows up intermediate cardinality, the cost-based planner must produce
    /// a substantially cheaper plan by seeding the selective endpoint. (The
    /// margin is bounded by the pattern's intrinsic result size, so we assert
    /// a robust multiple rather than an inflated "orders of magnitude".)
    #[test]
    fn cost_based_plan_crushes_naive_left_to_right_order() {
        let cfg = PlannerConfig::default();
        let stats = BaseStats::new()
            .with_label("Big", 1_000_000)
            .with_label("Tiny", 8)
            .with_edge("E", 5_000_000);

        // (n0)-[E]->(n1:Big)-[E]->(n2:Big)-[E]->(n3:Tiny, unique index)
        let mut g = QueryGraph::new();
        g.add_node(QueryNode::anonymous(0));
        g.add_node(QueryNode::labelled(1, "Big"));
        g.add_node(QueryNode::labelled(2, "Big"));
        g.add_node(QueryNode::labelled(3, "Tiny").with_index("id", IndexKind::Unique));
        for r in 0..3 {
            g.add_rel(QueryRel::new(
                r,
                r,
                r + 1,
                Some("E"),
                RelDirection::Outgoing,
            ));
        }

        // Naive: seed the first node, expand relationships in textual order.
        let mut naive = seed_plan(&g.nodes[0], &stats, &cfg);
        let mut bound = vec![false; g.nodes.len()];
        bound[0] = true;
        for r in 0..g.rels.len() {
            naive = expand_step(&g, &naive, RelId(r), &bound, &stats, &cfg)
                .expect("path is connected in order");
            bound[g.rels[r].from.0] = true;
            bound[g.rels[r].to.0] = true;
        }

        let optimal = plan_query_graph(&g, &stats, &cfg).expect("plannable");

        assert!(
            optimal.cost * 5.0 < naive.cost,
            "cost-based ({:.0}) must decisively beat naive ({:.0})",
            optimal.cost,
            naive.cost
        );
        // And it gets there by seeding the selective indexed endpoint.
        assert!(matches!(
            leaf(&optimal),
            PhysicalOp::NodeIndexSeek { node, .. } if node.0 == 3
        ));
    }
}
