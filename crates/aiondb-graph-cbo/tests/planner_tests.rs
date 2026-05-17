//! Behavioural tests for the cost-based graph planner.
//!
//! These assert the planner makes the *decisions* a competitive optimizer must
//! make — selective-side seeding, index-aware starts, direction flipping,
//! cartesian for disconnected patterns, cycle handling, bounded var-length and
//! a safe greedy fallback — independent of any executor.

use aiondb_graph_cbo::{
    plan_query_graph, BaseStats, ExpansionPlan, GraphError, IndexKind, PhysicalOp, PlanError,
    PlannerConfig, PropertyPredicate, QueryGraph, QueryNode, QueryRel, RelDirection,
};

fn count_expands(p: &ExpansionPlan) -> usize {
    match &p.op {
        PhysicalOp::Expand { input, .. } => 1 + count_expands(input),
        PhysicalOp::CartesianProduct { left, right } | PhysicalOp::HashJoin { left, right, .. } => {
            count_expands(left) + count_expands(right)
        }
        _ => 0,
    }
}

/// The deepest (first-executed) leaf operator of a plan.
fn leaf(p: &ExpansionPlan) -> &PhysicalOp {
    match &p.op {
        PhysicalOp::Expand { input, .. } => leaf(input),
        PhysicalOp::CartesianProduct { left, .. } | PhysicalOp::HashJoin { left, .. } => leaf(left),
        op => op,
    }
}

fn any_into(p: &ExpansionPlan) -> bool {
    match &p.op {
        PhysicalOp::Expand { input, into, .. } => *into || any_into(input),
        PhysicalOp::CartesianProduct { left, right } | PhysicalOp::HashJoin { left, right, .. } => {
            any_into(left) || any_into(right)
        }
        _ => false,
    }
}

fn uses_hash_join(p: &ExpansionPlan) -> bool {
    match &p.op {
        PhysicalOp::HashJoin { .. } => true,
        PhysicalOp::Expand { input, .. } => uses_hash_join(input),
        PhysicalOp::CartesianProduct { left, right } => {
            uses_hash_join(left) || uses_hash_join(right)
        }
        _ => false,
    }
}

/// Mark every query node the plan binds (seeds + expand endpoints).
fn collect_nodes(p: &ExpansionPlan, seen: &mut [bool]) {
    match &p.op {
        PhysicalOp::AllNodesScan { node }
        | PhysicalOp::NodeByLabelScan { node, .. }
        | PhysicalOp::NodeIndexSeek { node, .. } => seen[node.0] = true,
        PhysicalOp::Expand {
            input, from, to, ..
        } => {
            collect_nodes(input, seen);
            seen[from.0] = true;
            seen[to.0] = true;
        }
        PhysicalOp::CartesianProduct { left, right } | PhysicalOp::HashJoin { left, right, .. } => {
            collect_nodes(left, seen);
            collect_nodes(right, seen);
        }
    }
}

#[test]
fn bushy_hash_join_beats_left_deep_through_a_hub() {
    // Two unique-id-seeded leaves meet at a popular hub. A left-deep plan
    // must enumerate the hub's whole neighbourhood to reach the second leaf;
    // a bushy NodeHashJoin expands each leaf→hub independently and joins on
    // the hub. This is the canonical pattern where Neo4j uses NodeHashJoin
    // and a left-deep-only planner loses badly.
    let stats = BaseStats::new()
        .with_label("L", 1_000_000)
        .with_label("H", 100_000)
        .with_edge("AT", 10_000_000)
        .with_triple("L", "AT", "H", 10_000_000);

    let mut g = QueryGraph::new();
    g.add_node(QueryNode::labelled(0, "L").with_index("id", IndexKind::Unique));
    g.add_node(QueryNode::labelled(1, "H"));
    g.add_node(QueryNode::labelled(2, "L").with_index("id", IndexKind::Unique));
    g.add_rel(QueryRel::new(0, 0, 1, Some("AT"), RelDirection::Outgoing));
    g.add_rel(QueryRel::new(1, 2, 1, Some("AT"), RelDirection::Outgoing));

    let bushy = plan_query_graph(&g, &stats, &PlannerConfig::default()).expect("plannable");
    let left_deep = plan_query_graph(
        &g,
        &stats,
        &PlannerConfig {
            bushy_max: 0,
            ..PlannerConfig::default()
        },
    )
    .expect("plannable");

    assert!(
        uses_hash_join(&bushy),
        "the optimal plan must be bushy here"
    );
    assert!(
        bushy.cost * 5.0 < left_deep.cost,
        "bushy ({:.0}) must decisively beat left-deep ({:.0})",
        bushy.cost,
        left_deep.cost
    );
    assert_eq!(count_expands(&bushy), 2);
}

#[test]
fn rejects_malformed_and_empty_graphs() {
    let cfg = PlannerConfig::default();
    let stats = BaseStats::new();

    let empty = QueryGraph::new();
    assert_eq!(
        plan_query_graph(&empty, &stats, &cfg),
        Err(PlanError::EmptyGraph)
    );

    let mut g = QueryGraph::new();
    g.add_node(QueryNode::anonymous(0));
    g.rels
        .push(QueryRel::new(0, 0, 9, None, RelDirection::Outgoing));
    assert_eq!(
        plan_query_graph(&g, &stats, &cfg),
        Err(PlanError::Invalid(GraphError::DanglingEndpoint(
            aiondb_graph_cbo::RelId(0)
        )))
    );
}

#[test]
fn single_label_node_uses_label_scan() {
    let cfg = PlannerConfig::default();
    let stats = BaseStats::new().with_label("Person", 500);
    let mut g = QueryGraph::new();
    g.add_node(QueryNode::labelled(0, "Person"));

    let plan = plan_query_graph(&g, &stats, &cfg).expect("plannable");
    match plan.op {
        PhysicalOp::NodeByLabelScan { ref label, .. } => assert_eq!(label, "Person"),
        other => panic!("expected label scan, got {other:?}"),
    }
    assert!((plan.rows - 500.0).abs() < 1e-6);
}

#[test]
fn unique_index_seed_beats_label_scan() {
    let cfg = PlannerConfig::default();
    let stats = BaseStats::new().with_label("Person", 1_000_000);

    let mut scan_only = QueryGraph::new();
    scan_only.add_node(QueryNode::labelled(0, "Person"));
    let scan = plan_query_graph(&scan_only, &stats, &cfg).expect("plannable");

    let mut indexed = QueryGraph::new();
    indexed.add_node(QueryNode::labelled(0, "Person").with_index("id", IndexKind::Unique));
    let seek = plan_query_graph(&indexed, &stats, &cfg).expect("plannable");

    assert!(matches!(
        seek.op,
        PhysicalOp::NodeIndexSeek { unique: true, .. }
    ));
    assert!(
        seek.cost < scan.cost,
        "index seek must be cheaper than full scan"
    );
    assert!(seek.rows <= 1.0);
}

#[test]
fn planner_seeds_the_selective_side_and_flips_direction() {
    // (a:Person)-[:LIVES_IN]->(b:City); City is tiny and uniquely indexed, so
    // the cheapest plan starts at City and expands *backwards* into Person.
    let cfg = PlannerConfig::default();
    let stats = BaseStats::new()
        .with_label("Person", 1_000_000)
        .with_label("City", 100)
        .with_edge("LIVES_IN", 1_000_000);

    let mut g = QueryGraph::new();
    g.add_node(QueryNode::labelled(0, "Person"));
    g.add_node(QueryNode::labelled(1, "City").with_index("name", IndexKind::Unique));
    g.add_rel(QueryRel::new(
        0,
        0,
        1,
        Some("LIVES_IN"),
        RelDirection::Outgoing,
    ));

    let plan = plan_query_graph(&g, &stats, &cfg).expect("plannable");
    match &plan.op {
        PhysicalOp::Expand {
            from,
            to,
            direction,
            into,
            ..
        } => {
            assert_eq!(from.0, 1, "must start from the selective City node");
            assert_eq!(to.0, 0);
            assert_eq!(*direction, RelDirection::Incoming, "direction must flip");
            assert!(!*into);
        }
        other => panic!("expected expand root, got {other:?}"),
    }
    assert!(matches!(
        leaf(&plan),
        PhysicalOp::NodeIndexSeek { node, .. } if node.0 == 1
    ));
}

#[test]
fn predicate_selectivity_changes_the_seed() {
    // Equal labels: the side carrying a selective equality predicate (few
    // distinct values known to stats) must be chosen as the seed.
    let cfg = PlannerConfig::default();
    let stats = BaseStats::new()
        .with_label("User", 100_000)
        .with_edge("FOLLOWS", 500_000)
        .with_distinct("User", "email", 100_000);

    let mut g = QueryGraph::new();
    g.add_node(QueryNode::labelled(0, "User"));
    g.add_node(QueryNode::labelled(1, "User").with_predicate(PropertyPredicate::equality("email")));
    g.add_rel(QueryRel::new(
        0,
        0,
        1,
        Some("FOLLOWS"),
        RelDirection::Outgoing,
    ));

    let plan = plan_query_graph(&g, &stats, &cfg).expect("plannable");
    let PhysicalOp::Expand { from, .. } = &plan.op else {
        panic!("expected expand root");
    };
    assert_eq!(
        from.0, 1,
        "seed must be the node with the selective predicate"
    );
}

#[test]
fn disconnected_pattern_becomes_cartesian_product() {
    let cfg = PlannerConfig::default();
    let stats = BaseStats::new()
        .with_label("A", 10)
        .with_label("B", 20)
        .with_edge("R", 50);

    let mut g = QueryGraph::new();
    for i in 0..4 {
        let label = if i < 2 { "A" } else { "B" };
        g.add_node(QueryNode::labelled(i, label));
    }
    g.add_rel(QueryRel::new(0, 0, 1, Some("R"), RelDirection::Outgoing));
    g.add_rel(QueryRel::new(1, 2, 3, Some("R"), RelDirection::Outgoing));

    let plan = plan_query_graph(&g, &stats, &cfg).expect("plannable");
    assert!(matches!(plan.op, PhysicalOp::CartesianProduct { .. }));
    assert_eq!(count_expands(&plan), 2);
    assert!(plan.cost.is_finite());
}

#[test]
fn triangle_pattern_is_closed_by_a_join_not_treed() {
    let stats = BaseStats::new()
        .with_label("N", 1_000)
        .with_edge("E", 5_000);

    let mut g = QueryGraph::new();
    for i in 0..3 {
        g.add_node(QueryNode::labelled(i, "N"));
    }
    g.add_rel(QueryRel::new(0, 0, 1, Some("E"), RelDirection::Outgoing));
    g.add_rel(QueryRel::new(1, 1, 2, Some("E"), RelDirection::Outgoing));
    g.add_rel(QueryRel::new(2, 2, 0, Some("E"), RelDirection::Outgoing));

    // Left-deep only: the cycle must be closed with an ExpandInto.
    let left_deep = plan_query_graph(
        &g,
        &stats,
        &PlannerConfig {
            bushy_max: 0,
            ..PlannerConfig::default()
        },
    )
    .expect("plannable");
    assert_eq!(count_expands(&left_deep), 3);
    assert!(
        any_into(&left_deep),
        "left-deep must close the cycle with ExpandInto"
    );

    // With bushy enabled the cycle may instead be closed by a HashJoin on the
    // shared node — also correct, and never costlier.
    let best = plan_query_graph(&g, &stats, &PlannerConfig::default()).expect("plannable");
    assert_eq!(count_expands(&best), 3);
    assert!(
        any_into(&best) || uses_hash_join(&best),
        "a cycle must be closed by a join (ExpandInto or HashJoin), not treed"
    );
    assert!(best.rows.is_finite() && best.cost.is_finite());
    assert!(best.cost <= left_deep.cost + 1e-9);
}

#[test]
fn unbounded_var_length_stays_finite() {
    let cfg = PlannerConfig::default();
    let stats = BaseStats::new()
        .with_label("N", 10_000)
        .with_edge("E", 100_000);

    let mut g = QueryGraph::new();
    g.add_node(QueryNode::labelled(0, "N").with_index("id", IndexKind::Unique));
    g.add_node(QueryNode::labelled(1, "N"));
    g.add_rel(QueryRel::new(0, 0, 1, Some("E"), RelDirection::Outgoing).with_var_length(1, None));

    let plan = plan_query_graph(&g, &stats, &cfg).expect("plannable");
    assert!(plan.rows.is_finite() && plan.rows <= 1e15);
    assert!(plan.cost.is_finite() && plan.cost <= 1e15);
}

#[test]
fn greedy_fallback_still_covers_every_relationship() {
    // Force greedy by capping DP below the relationship count; the plan must
    // still consume all relationships and stay finite.
    let cfg = PlannerConfig {
        max_dp_rels: 2,
        ..PlannerConfig::default()
    };
    let stats = BaseStats::new()
        .with_label("N", 1_000)
        .with_edge("E", 4_000);

    let mut g = QueryGraph::new();
    let n = 7;
    for i in 0..=n {
        g.add_node(QueryNode::labelled(i, "N"));
    }
    for i in 0..n {
        g.add_rel(QueryRel::new(
            i,
            i,
            i + 1,
            Some("E"),
            RelDirection::Outgoing,
        ));
    }

    let plan = plan_query_graph(&g, &stats, &cfg).expect("plannable");
    assert_eq!(
        count_expands(&plan),
        n,
        "every relationship must be expanded"
    );
    assert!(plan.cost.is_finite());
}

#[test]
fn dp_and_greedy_agree_on_a_simple_path() {
    let stats = BaseStats::new()
        .with_label("N", 1_000)
        .with_label("Hot", 5)
        .with_edge("E", 3_000);

    let mut g = QueryGraph::new();
    g.add_node(QueryNode::labelled(0, "N"));
    g.add_node(QueryNode::labelled(1, "N"));
    g.add_node(QueryNode::labelled(2, "Hot").with_index("k", IndexKind::Unique));
    g.add_rel(QueryRel::new(0, 0, 1, Some("E"), RelDirection::Outgoing));
    g.add_rel(QueryRel::new(1, 1, 2, Some("E"), RelDirection::Outgoing));

    let dp = plan_query_graph(&g, &stats, &PlannerConfig::default()).expect("dp");
    let greedy = plan_query_graph(
        &g,
        &stats,
        &PlannerConfig {
            max_dp_rels: 0,
            ..PlannerConfig::default()
        },
    )
    .expect("greedy");

    // Both strategies must seed the tiny "Hot" node (n2), whether via the
    // unique index or a 5-row label scan — either is optimal here; what
    // matters is not seeding a 1000-row N.
    let seed_node = |op: &PhysicalOp| match op {
        PhysicalOp::AllNodesScan { node }
        | PhysicalOp::NodeByLabelScan { node, .. }
        | PhysicalOp::NodeIndexSeek { node, .. } => Some(node.0),
        _ => None,
    };
    assert_eq!(seed_node(leaf(&dp)), Some(2));
    assert_eq!(seed_node(leaf(&greedy)), Some(2));
    assert!(
        dp.cost <= greedy.cost,
        "exhaustive DP is never worse than greedy"
    );
}

#[test]
fn planning_is_deterministic_and_explainable() {
    let cfg = PlannerConfig::default();
    let stats = BaseStats::new().with_label("N", 800).with_edge("E", 2_000);

    let mut g = QueryGraph::new();
    for i in 0..3 {
        g.add_node(QueryNode::labelled(i, "N"));
    }
    g.add_rel(QueryRel::new(0, 0, 1, Some("E"), RelDirection::Outgoing));
    g.add_rel(QueryRel::new(1, 1, 2, Some("E"), RelDirection::Both));

    let a = plan_query_graph(&g, &stats, &cfg).expect("plannable");
    let b = plan_query_graph(&g, &stats, &cfg).expect("plannable");
    assert_eq!(a, b, "the planner must be deterministic");

    let explained = a.explain();
    assert!(explained.contains("Expand"));
    assert!(explained.contains("rows"));
}

#[test]
fn typed_triple_statistic_sharpens_the_estimate() {
    // The same pattern, planned with only a per-type total vs. with the
    // typed-triple count, must yield a tighter row estimate — this is the
    // statistic that lets the planner match Neo4j on mixed schemas.
    let cfg = PlannerConfig::default();
    let coarse = BaseStats::new()
        .with_label("Person", 1_000)
        .with_label("City", 1_000)
        .with_edge("LIVES_IN", 50_000);
    let precise = coarse
        .clone()
        .with_triple("Person", "LIVES_IN", "City", 1_000);

    let mut g = QueryGraph::new();
    g.add_node(QueryNode::labelled(0, "Person").with_index("id", IndexKind::Unique));
    g.add_node(QueryNode::labelled(1, "City"));
    g.add_rel(QueryRel::new(
        0,
        0,
        1,
        Some("LIVES_IN"),
        RelDirection::Outgoing,
    ));

    let a = plan_query_graph(&g, &coarse, &cfg).expect("plannable");
    let b = plan_query_graph(&g, &precise, &cfg).expect("plannable");
    assert!(
        b.rows < a.rows,
        "triple stat ({} rows) must be tighter than per-type total ({} rows)",
        b.rows,
        a.rows
    );
}

#[test]
fn selective_relationship_predicate_reduces_output() {
    let cfg = PlannerConfig::default();
    let stats = BaseStats::new()
        .with_label("U", 10_000)
        .with_triple("U", "FOLLOWS", "U", 200_000);

    let build = |with_pred: bool| {
        let mut g = QueryGraph::new();
        g.add_node(QueryNode::labelled(0, "U").with_index("id", IndexKind::Unique));
        g.add_node(QueryNode::labelled(1, "U"));
        let mut rel = QueryRel::new(0, 0, 1, Some("FOLLOWS"), RelDirection::Outgoing);
        if with_pred {
            rel = rel.with_predicate(PropertyPredicate::equality("since"));
        }
        g.add_rel(rel);
        plan_query_graph(&g, &stats, &cfg).expect("plannable")
    };

    assert!(
        build(true).rows < build(false).rows,
        "an equality predicate on the relationship must shrink the estimate"
    );
}

#[test]
fn errors_implement_display_and_source() {
    let stats = BaseStats::new();
    let cfg = PlannerConfig::default();
    let err = plan_query_graph(&QueryGraph::new(), &stats, &cfg).unwrap_err();
    assert_eq!(err.to_string(), "query graph has no nodes");

    let mut g = QueryGraph::new();
    g.add_node(QueryNode::anonymous(0));
    g.rels
        .push(QueryRel::new(0, 0, 5, None, RelDirection::Outgoing));
    let err = plan_query_graph(&g, &stats, &cfg).unwrap_err();
    assert!(err.to_string().starts_with("invalid query graph:"));
    assert!(std::error::Error::source(&err).is_some());
}

/// Deterministic, dependency-free xorshift64* PRNG so the stress test is
/// reproducible across machines and CI runs.
struct Rng(u64);

impl Rng {
    fn next_u64(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }
    fn below(&mut self, n: usize) -> usize {
        (self.next_u64() % n as u64) as usize
    }
    fn chance(&mut self, percent: u64) -> bool {
        self.next_u64() % 100 < percent
    }
}

/// Property/fuzz harness: over many bounded random patterns the planner must
/// stay total (never panic), produce finite estimates, expand every
/// relationship exactly once, be deterministic, and never let the greedy
/// fallback beat exhaustive DP. This is the invariant set a planner must hold
/// to be trustworthy in production — stronger guarantees than a hand-picked
/// suite, and what keeps it from silently regressing below Neo4j.
#[test]
fn randomized_patterns_uphold_planner_invariants() {
    let labels = [None, Some("A"), Some("B"), Some("C")];
    let rel_types = [None, Some("R1"), Some("R2")];
    let dirs = [
        RelDirection::Outgoing,
        RelDirection::Incoming,
        RelDirection::Both,
    ];

    for seed in 1..=300u64 {
        let mut rng = Rng(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1);

        let n_nodes = 2 + rng.below(5);
        let mut g = QueryGraph::new();
        for i in 0..n_nodes {
            let mut node = match labels[rng.below(labels.len())] {
                Some(l) => QueryNode::labelled(i, l),
                None => QueryNode::anonymous(i),
            };
            if rng.chance(15) {
                node = node.with_index("id", IndexKind::Unique);
            }
            if rng.chance(25) {
                node = node.with_predicate(PropertyPredicate::equality("p"));
            }
            g.add_node(node);
        }
        let n_rels = rng.below(9);
        for r in 0..n_rels {
            let from = rng.below(n_nodes);
            let to = rng.below(n_nodes);
            let mut rel = QueryRel::new(
                r,
                from,
                to,
                rel_types[rng.below(rel_types.len())],
                dirs[rng.below(dirs.len())],
            );
            if rng.chance(15) {
                rel = rel.with_var_length(1, Some(1 + rng.below(4) as u32));
            }
            if rng.chance(20) {
                rel = rel.with_predicate(PropertyPredicate::equality("w"));
            }
            g.add_rel(rel);
        }

        let stats = BaseStats::new()
            .with_label("A", 1 + rng.below(100_000) as u64)
            .with_label("B", 1 + rng.below(50_000) as u64)
            .with_label("C", 1 + rng.below(1_000) as u64)
            .with_edge("R1", 1 + rng.below(500_000) as u64)
            .with_edge("R2", 1 + rng.below(10_000) as u64)
            .with_triple("A", "R1", "B", 1 + rng.below(20_000) as u64);

        let cfg = PlannerConfig::default();
        let plan = plan_query_graph(&g, &stats, &cfg)
            .unwrap_or_else(|e| panic!("seed {seed}: planner must be total, got {e}"));

        assert!(
            plan.rows.is_finite() && plan.rows >= 0.0 && plan.rows <= 1e15,
            "seed {seed}: rows out of range: {}",
            plan.rows
        );
        assert!(
            plan.cost.is_finite() && plan.cost >= 0.0 && plan.cost <= 1e15,
            "seed {seed}: cost out of range: {}",
            plan.cost
        );
        assert_eq!(
            count_expands(&plan),
            g.rels.len(),
            "seed {seed}: every relationship must be expanded exactly once"
        );
        let mut seen = vec![false; g.nodes.len()];
        collect_nodes(&plan, &mut seen);
        assert!(
            seen.iter().all(|&b| b),
            "seed {seed}: every query node must be bound by the plan"
        );

        let again = plan_query_graph(&g, &stats, &cfg).expect("plannable");
        assert_eq!(plan, again, "seed {seed}: planner must be deterministic");

        let greedy = plan_query_graph(
            &g,
            &stats,
            &PlannerConfig {
                max_dp_rels: 0,
                ..PlannerConfig::default()
            },
        )
        .expect("plannable");
        assert!(
            plan.cost <= greedy.cost + 1e-6 + greedy.cost.abs() * 1e-9,
            "seed {seed}: exhaustive DP ({}) must not be beaten by greedy ({})",
            plan.cost,
            greedy.cost
        );

        // Enabling bushy joins explores a strict superset of plans, so it can
        // never produce a worse plan than the left-deep-only search.
        let left_deep_only = plan_query_graph(
            &g,
            &stats,
            &PlannerConfig {
                bushy_max: 0,
                ..PlannerConfig::default()
            },
        )
        .expect("plannable");
        assert!(
            plan.cost <= left_deep_only.cost + 1e-6 + left_deep_only.cost.abs() * 1e-9,
            "seed {seed}: bushy ({}) must never be worse than left-deep ({})",
            plan.cost,
            left_deep_only.cost
        );

        // A tiny frontier cap must stay total/finite/covering, and a larger
        // cap must never produce a worse plan (more retained candidates can
        // only help): the cap is a safe, monotone approximation knob.
        let tight = plan_query_graph(
            &g,
            &stats,
            &PlannerConfig {
                frontier_cap: 2,
                ..PlannerConfig::default()
            },
        )
        .expect("plannable");
        assert!(tight.rows.is_finite() && tight.cost.is_finite());
        assert_eq!(count_expands(&tight), g.rels.len());
        let mut seen2 = vec![false; g.nodes.len()];
        collect_nodes(&tight, &mut seen2);
        assert!(
            seen2.iter().all(|&b| b),
            "seed {seed}: tight cap drops a node"
        );
        assert!(
            plan.cost <= tight.cost + 1e-6 + tight.cost.abs() * 1e-9,
            "seed {seed}: larger frontier cap must never be worse ({} vs {})",
            plan.cost,
            tight.cost
        );
    }
}
