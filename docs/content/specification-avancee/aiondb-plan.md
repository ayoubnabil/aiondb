---
title: aiondb-plan
order: 22
---

# aiondb-plan

Plan IR shared by the planner, optimizer and executor. The crate has no logic of its own beyond construction helpers: it defines the `LogicalPlan` and `PhysicalPlan` enums, typed expressions, projections, sort keys, DML and Cypher plan nodes, and the distributed-fragment graph types. Every other SQL-pipeline crate depends on these definitions.

## cargo

```toml
[dependencies]
aiondb-plan = { path = "../aiondb-plan" }
```

## modules

| module | purpose |
|---|---|
| `logical` | `LogicalPlan` enum and SQL lock modes. |
| `physical` | `PhysicalPlan` enum, join types, aggregates, set operations, Cypher physical pipeline. |
| `expr` | Typed expression tree (`TypedExpr`, `TypedExprKind`, `ScalarFunction`, `WindowFunctionKind`). |
| `dml` | `MergePlan`, `UpdateAssignment`, `InsertOnConflict`, `MergeActionPlan`, `MutationTarget`. |
| `command` | Non-DML command plans (`CommandPlan`, `PgObjectAction`, `DiscardTarget`, ...). |
| `graph` | Cypher path functions and `IndexScanInfo`. |
| `metadata` | `PlanNodeId`, `ResultField` (output column descriptor). |
| `vector` | Vector-search plan (`VectorSearchPlan`, `VectorSearchAlgorithm`, `VectorSearchSpec`, `VectorDistanceMetric`). |
| `distributed` | Fragment graph (`DistributedPhysicalPlan`, `PlanFragment`, `ExchangeKind`, `FragmentEdge`, `FragmentTarget`, `FragmentPlacement`, `FragmentPartitionSpec`). |
| `shared` | Cross-module pieces: `ColumnPlan`, `IndexColumnPlan`, `ProjectionExpr`, `ForeignKeyPlan`, HNSW options. |

## key types

- `LogicalPlan` - logical algebra node produced by the planner: `ProjectTable`, `Join`, `Aggregate`, `Sort`, `Insert`, `Update`, `Delete`, `Merge`, `CypherQuery`, DDL variants, `InternalNoOp`, ...
- `PhysicalPlan` - physical operator emitted by the optimizer and executed by `aiondb-executor`.
- `TypedExpr` / `TypedExprKind` - expression with a `DataType` and nullability flag attached at every node.
- `ProjectionExpr`, `SortExpr`, `AggregateExpr` - projection, ordering, and aggregate computations.
- `JoinType`, `SetOperationType` - join kind and `UNION` / `INTERSECT` / `EXCEPT` flavors.
- `ScanAccessPath` - which physical access path a scan picked (sequential, index lookup, index range, ...).
- `MergePlan`, `MergeActionPlan`, `MergeWhenClausePlan`, `UpdateAssignment`, `InsertOnConflict`, `OnConflictActionPlan`, `MutationTarget` - DML.
- `CommandPlan`, `PgObjectAction`, `PgObjectKind`, `ConstraintTarget`, `DiscardTarget`, `ResetTarget`, `UnlistenTarget` - non-DML commands.
- `CypherQueryPlan`, `CypherPipelineOp`, `CypherMatchClause`, `CypherCreateClause`, `CypherMergeClause`, `CypherDeleteClause`, `CypherUnwindClause`, `CypherForeachPlan`, `CypherCallPlan`, `CypherWithClause`, `CypherUnionPlan`, `CypherSetItem`, `CypherPropertyExpr`, `CypherPattern`, `CypherNodePattern`, `CypherRelPattern`, `CypherRelDirection` - Cypher physical pipeline.
- `VectorSearchPlan`, `VectorSearchAlgorithm`, `VectorSearchSpec`, `VectorDistanceMetric` - kNN search spec.
- `DistributedPhysicalPlan`, `PlanFragment`, `FragmentEdge`, `ExchangeKind`, `FragmentTarget`, `FragmentPlacement`, `FragmentPartitionSpec` - distributed fragment graph.
- `PgLockMode` - Postgres table lock modes (mirrors the parser enum so planner and executor do not depend on the parser crate).
- `ColumnPlan`, `IndexColumnPlan`, `ForeignKeyPlan`, `HnswPlanOptions`, `HnswPlanDistanceMetric`, `HnswPlanQuantization` - DDL helpers.
- `ResultField` - one output column (name, type, nullability).
- `PlanNodeId` - stable plan-node identifier used by metrics and EXPLAIN.

## example

```rust
use aiondb_core::{DataType, Value};
use aiondb_plan::{LogicalPlan, PhysicalPlan, ProjectionExpr, ResultField, TypedExpr};

let lit = TypedExpr::literal(Value::Int(1), DataType::Int, false);
let proj = ProjectionExpr {
    field: ResultField {
        name: "one".to_string(),
        data_type: DataType::Int,
        nullable: false,
    },
    expr: lit,
};

let plan = PhysicalPlan::ProjectOnce {
    outputs: vec![proj],
    filter: None,
    order_by: Vec::new(),
    limit: None,
    offset: None,
};

let _ = plan;
let _ = LogicalPlan::Checkpoint;
```
