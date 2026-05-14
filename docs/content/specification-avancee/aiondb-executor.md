---
title: aiondb-executor
order: 26
---

# aiondb-executor

Runs a `PhysicalPlan` against the catalog, storage, and sequence backends. The crate ties together every other SQL-pipeline crate: it consumes plans from `aiondb-plan`, dispatches expressions through `aiondb-eval`, drives mutations through `aiondb-catalog` and the storage traits, and runs PL/pgSQL programs through `aiondb-plpgsql`.

## cargo

```toml
[dependencies]
aiondb-executor = { path = "../aiondb-executor" }
```

## modules

| module | purpose |
|---|---|
| `context` | `ExecutionContext`, `SessionSettings`, `SequenceSessionState`. |
| `executor` | The `Executor` struct and every per-operator implementation (joins, aggregates, DML, DDL, COPY, triggers, FK / unique / check enforcement, window functions, vacuum, analyze, distributed fragments, Cypher, PL/pgSQL bridge). |
| `result` | `ExecutionResult` and `ResultChunk` - what `execute` returns to the engine. |
| `row_stream` | Row iterator used between operators. |
| `distributed_runtime` | `DistributedQueryRuntime`, the engine-level coordinator that owns fragment dispatchers. |
| `spill` | On-disk spill files for hash joins / aggregates that exceed the work-memory budget. |

## key types

- `Executor` - top-level executor. Constructed with `Executor::new(catalog_reader, catalog_writer, catalog_txn, sequence_manager, storage_ddl, storage_dml, storage_txn, logical_plan_compiler)` and optionally `Executor::with_fragment_dispatcher(dispatcher)` for distributed mode.
- `ExecutionContext` - per-statement state: txn id, session settings, sequence session state, parameter values, cancellation flag.
- `SessionSettings`, `SequenceSessionState` - session-scoped state passed through the context.
- `ExecutionResult`, `ResultChunk` - rows + side effects produced by a statement.
- `DistributedQueryRuntime` - owns remote fragment handlers and dispatches them.
- `DistributedFragment`, `FragmentDispatcher`, `FragmentPartition`, `FragmentTarget`, `RegisteredRemoteFragmentDispatcher`, `RemoteFragmentHandler` - distributed-execution surface re-exported from `executor`.

## entry points

| item | role |
|---|---|
| `Executor::execute(plan, ctx)` | Run a single `PhysicalPlan` and return an `ExecutionResult`. |
| `Executor::execute_distributed_fragments(plans, ctx)` | Run a slice of fragments locally. |
| `Executor::execute_distributed_fragments_targeted(fragments, ctx)` | Run pre-targeted distributed fragments through the configured dispatcher. |
| `assign_distributed_fragment_targets`, `distributed_fragment_target_for_index`, `format_fragment_target` | Helpers used to build / inspect fragment targets. |
| `hash_partition_for_row` | Hash-partitioning helper used by exchange operators. |
| `parse_copy_text_value` | Re-exported from the `COPY` plan path so the engine can parse a single text value the same way `COPY FROM` does. |
| `node_registry` | Module exposing the per-operator entry points used by the engine and tests. |

## example

```rust
use std::sync::Arc;

use aiondb_catalog::{CatalogReader, CatalogTxnParticipant, CatalogWriter, SequenceManager};
use aiondb_executor::{ExecutionContext, Executor};
use aiondb_plan::PhysicalPlan;

fn run(
    catalog_reader: Arc<dyn CatalogReader>,
    catalog_writer: Arc<dyn CatalogWriter>,
    catalog_txn: Arc<dyn CatalogTxnParticipant>,
    sequence_manager: Arc<dyn SequenceManager>,
    storage_ddl: Arc<dyn aiondb_executor::executor::StorageDDL>,
    storage_dml: Arc<dyn aiondb_executor::executor::StorageDML>,
    storage_txn: Arc<dyn aiondb_executor::executor::StorageTxnParticipant>,
    logical_plan_compiler: Arc<aiondb_executor::executor::LogicalPlanCompiler>,
    plan: &PhysicalPlan,
    ctx: &ExecutionContext,
) {
    let executor = Executor::new(
        catalog_reader,
        catalog_writer,
        catalog_txn,
        sequence_manager,
        storage_ddl,
        storage_dml,
        storage_txn,
        logical_plan_compiler,
    );
    let result = executor.execute(plan, ctx).expect("execute");
    let _ = result;
}
```

The exact set of storage / compiler traits is defined in the `executor` module and consumed by the engine; the snippet above is illustrative of the construction shape.
