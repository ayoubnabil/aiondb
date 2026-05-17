# Wiring `aiondb-graph-cbo` into the engine

This crate is the **cost-based graph planner brain**. It is intentionally
pure and dependency-free so it builds and tests in isolation
(`cargo test -p aiondb-graph-cbo`) without compiling the large
`aiondb-optimizer` / `aiondb-executor` crates.

The remaining work is purely *mechanical glue* (no algorithm changes). It is
documented here rather than applied, because compiling `aiondb-optimizer`
(~26k LOC) is a heavy build that was deliberately out of scope.

## Goal

Make the **general traversal path competitive with Neo4j without relying on
executor fast paths**. Today `crates/aiondb-executor/.../graph_plans/mod.rs`
has ~22 hand-written fast paths plus a crude 3-level compile-time pivot
heuristic. This crate replaces the heuristic with a real IDP cost search.

## Step 1 — statistics adapter (trivial: field-name compatible)

`aiondb_optimizer::graph_optimizer::GraphStats` already stores
`label_cardinality` and `edge_cardinality`. Implement `GraphStatistics` for it:

```rust
impl aiondb_graph_cbo::GraphStatistics for GraphStats {
    fn total_nodes(&self) -> f64 { self.label_cardinality.values().sum::<u64>() as f64 }
    fn label_cardinality(&self, l: Option<&str>) -> f64 {
        l.map_or_else(|| self.total_nodes(),
                      |l| self.node_count(l) as f64)
    }
    fn relationship_cardinality(&self, t: Option<&str>) -> f64 {
        t.map_or(5_000.0, |t| self.edge_count(t) as f64)
    }
    fn distinct_values(&self, _l: Option<&str>, _p: &str) -> Option<f64> { None }
}
```

`distinct_values` can later be sourced from
`CatalogReader::get_statistics(...).column_stats[*].ndistinct` for sharper
equality selectivity.

## Step 2 — lower `CypherPattern` → `QueryGraph`

In `graph_optimizer.rs`, before the existing greedy `reorder_patterns`, build a
`QueryGraph`: one `QueryNode` per `CypherNodePattern` (label, inline property
predicates → `PropertyPredicate`, `IndexScanInfo` → `with_index`), one
`QueryRel` per `CypherRelPattern` (type, `CypherRelDirection` →
`RelDirection`, `min_hops/max_hops` → `with_var_length`).

## Step 3 — call the planner, project the order back

`plan_query_graph(&qg, &stats, &PlannerConfig::default())` returns an
`ExpansionPlan`. Walk it to recover the seed node and the ordered expand
sequence (with chosen directions / expand-into), then reorder
`CypherPattern.nodes` / `.relationships` to match and set
`CypherRelDirection` from the plan. The executor's existing left-to-right
runner then follows the optimal order — no executor changes required.

## Step 4 — retire fast paths incrementally

With the CBO driving order, fast paths become redundant. Gate them behind a
flag and A/B against the planner per shape; delete once parity holds. The
planner is deterministic, so plan-shape regression tests are stable.
