---
title: aiondb-optimizer
order: 24
---

# aiondb-optimizer

Lowers a `LogicalPlan` into a `PhysicalPlan`. Applies predicate pushdown, projection pruning, transitive-predicate inference, outer-join simplification, join reordering, access-path selection, and HNSW / vector-search planning. Costs are computed against catalog statistics fetched through a `CatalogReader`.

## cargo

```toml
[dependencies]
aiondb-optimizer = { path = "../aiondb-optimizer" }
```

## modules

| module | purpose |
|---|---|
| `physical_builder` | Drives the logical-to-physical lowering and emits `PhysicalPlan` nodes. |
| `access_path` | Picks an access path for each scan (sequential, index lookup, index range, vector search). |
| `cost` | `PlanCost` and per-operator costing helpers. |
| `rules` | Logical-rewrite rules dispatched from the top of `optimize`. |
| `predicate_pushdown` | Pushes filters past joins and projections. |
| `projection_pruning` | Drops columns that downstream operators do not need. |
| `transitive_predicates` | Derives `a = b && b = c => a = c` style predicates to enable extra index lookups. |
| `outer_join_simplify` | Converts outer joins to inner joins where the outer side is provably non-null. |
| `join_reorder` | Reorders inner-join chains by estimated row count. |
| `graph_optimizer` | Cypher-specific rewrites. |
| `distributed` | Lowering for `DistributedPhysicalPlan` fragments. |

## key types

- `Optimizer` - top-level optimizer, built with `Optimizer::new(catalog_reader)`.
- `OptimizeRequest` - input bundle: `logical_plan` and `txn_id`.
- `PlanCost` (in `cost`) - rows + cpu + io estimate attached to candidate plans.
- `PhysicalBuilder` (in `physical_builder`) - re-exported builder that walks a `LogicalPlan` and produces `PhysicalPlan` nodes.

## entry points

| item | role |
|---|---|
| `Optimizer::new(catalog)` | Build an optimizer over a `CatalogReader`. |
| `Optimizer::optimize(req)` | Lower a `LogicalPlan` to `PhysicalPlan`, returning a `DbResult<PhysicalPlan>`. |
| `Optimizer::optimize_cypher_with_stats(...)` | Cypher-specific optimization path that accepts external statistics. |

## example

```rust
use std::sync::Arc;
use aiondb_catalog::CatalogReader;
use aiondb_core::TxnId;
use aiondb_optimizer::{OptimizeRequest, Optimizer};
use aiondb_plan::LogicalPlan;

fn lower(catalog: Arc<dyn CatalogReader>, logical: LogicalPlan, txn: TxnId) {
    let optimizer = Optimizer::new(catalog);
    let req = OptimizeRequest {
        logical_plan: logical,
        txn_id: txn,
    };
    let physical = optimizer.optimize(req).expect("optimize");
    let _ = physical;
}
```
