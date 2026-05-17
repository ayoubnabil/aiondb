# aiondb-graph-cbo

Cost-based optimizer for graph (Cypher) traversal. This is the planning brain
that lets AionDB choose a good join/expansion order **without** relying on
hand-written executor fast paths — the same family of algorithm Neo4j uses for
Cypher (Iterative Dynamic Programming) driven by a real cardinality/cost model
fed from catalog statistics.

## What it does

```
QueryGraph  +  GraphStatistics   ──▶   plan_query_graph   ──▶   ExpansionPlan
(validated pattern)  (catalog stats)      (this crate)        (annotated, explainable)
```

* **Index-aware seeding** — picks the cheapest entry point (unique seek vs.
  index range vs. label scan vs. all-nodes scan), including the realistic
  crossover where scanning a tiny label beats an index seek.
* **Cost-based order & direction** — searches seed choice, expansion order and
  traversal direction; flips a relationship when starting from the more
  selective endpoint is cheaper.
* **Typed-triple cardinality** — fan-out prefers `count((:A)-[:T]->(:B))`, the
  statistic Neo4j's planner relies on, falling back to per-type totals.
* **Cycles & disconnected patterns** — closes cycles with `ExpandInto`;
  combines independent components with a cartesian product.

## Algorithm

Per connected component, bounded dynamic programming over the relationship set
(Selinger / Neo4j-IDP style): the cheapest plan for every reachable
sub-pattern is built bottom-up by extending one adjacent relationship at a
time **and** by joining two node-sharing sub-patterns with a bushy
`HashJoin` (subset DP — Neo4j's `NodeHashJoin`, decisive on hub patterns
where left-deep loses). Because each expansion's cost and output cardinality
depend on the sub-plan's *size*, keeping a single cheapest plan per set is
**not** Bellman-optimal, so a **Pareto frontier** of `(cost, rows)`-
nondominated sub-plans is kept — and carried *through* the cartesian product,
since a costlier-but-smaller component plan can be far cheaper once
multiplied. The global optimum is chosen only at the root.

Above a safe size the search degrades to a deterministic, still cost-driven
greedy strategy; the DP table is hard-capped so it can never exhaust memory.

## Safety & performance

* No `unsafe`; passes `clippy` at the workspace's `pedantic = deny`,
  `warnings = deny` level; `rustfmt`-clean.
* Total: malformed patterns are rejected by `QueryGraph::validate`; the
  planner never panics, and all arithmetic is saturated/finite (no
  `NaN`/`inf`, cardinalities capped).
* Plan nodes share structure via `Rc`, so the DP's constant sub-plan cloning
  is O(1) rather than a deep tree copy.
* Dependency-free and pure → unit-testable in isolation
  (`cargo test -p aiondb-graph-cbo`).

## Testing

Behavioural tests pin the decisions a competitive planner must make, plus a
deterministic, zero-dependency **fuzz harness** (300 seeded random patterns)
asserting the invariants — totality, finiteness, full relationship coverage,
determinism, and *exhaustive DP is never beaten by greedy*. The harness has
already caught two real soundness defects (see git history).

## Status & limitations

* The output `ExpansionPlan` is consumed by the engine to order the existing
  Cypher executor — see [`INTEGRATION.md`](./INTEGRATION.md) for the (purely
  mechanical) wiring.
* Both left-deep expansion and bushy hash-join plans are enumerated. The
  bushy subset-DP is gated by `PlannerConfig::bushy_max` (hard-capped at 10)
  so the `O(3^k)` search can never blow up; very large components fall back
  to left-deep DP and then greedy.

## Usage

```rust
use aiondb_graph_cbo::*;

let stats = BaseStats::new()
    .with_label("Person", 1_000_000)
    .with_label("City", 100)
    .with_triple("Person", "LIVES_IN", "City", 1_000_000);

let mut g = QueryGraph::new();
g.add_node(QueryNode::labelled(0, "Person"));
g.add_node(QueryNode::labelled(1, "City").with_index("name", IndexKind::Unique));
g.add_rel(QueryRel::new(0, 0, 1, Some("LIVES_IN"), RelDirection::Outgoing));

let plan = plan_query_graph(&g, &stats, &PlannerConfig::default()).unwrap();
println!("{}", plan.explain());
```
