---
title: aiondb-eval
order: 25
---

# aiondb-eval

Expression evaluator and runtime helpers shared between the planner, optimizer and executor. Owns the scalar function registry, value coercions, hash-key construction, regex cache, async-notify queue, statement cancellation flag, and the per-thread session context (search path, datestyle, timezone, current database, etc.) that scalar functions read from.

## cargo

```toml
[dependencies]
aiondb-eval = { path = "../aiondb-eval" }
```

## modules

| module | purpose |
|---|---|
| `eval` | `ExpressionEvaluator` and the bulk of the evaluator (`mod`, `cast`, `operators`, `scalar_functions`, `domain_check`, `pg_format`, `session`, `temporal_precision`, `money`). |
| `coercions` | `coerce_value` - run-time value-to-type coercion. |
| `functions` | Scalar function registry (`FunctionRegistry`, `FunctionInfo`). |
| `hash_key` | `build_hash_key`, `ValueHashKey` - canonical hashing for grouping and joins. |
| `cancel` | Statement cancellation flag. |
| `async_notify` | Backing queue for `LISTEN` / `NOTIFY`. |

## key types

- `ExpressionEvaluator` - zero-sized struct with the evaluation entry points: `evaluate(&TypedExpr)`, `evaluate_with_row(&TypedExpr, &Row)`, plus resolver-driven variants for correlated subqueries.
- `EvalSessionContext`, `EvalTemporalSessionContext` - session-scoped state read by scalar functions.
- `FunctionRegistry`, `FunctionInfo` - scalar function dispatch.
- `ValueHashKey` - hashable wrapper over `Value` for hash joins and aggregation.
- `CompatCastContext`, `CompatCastMethod`, `CompatUserCast`, `CompatUserType`, `CompatUserTypeField`, `DomainConstraint`, `DomainDef` - PG-compatible cast / domain machinery.
- `ClusterDatabaseSummary` - snapshot of cluster-wide database state used by `pg_database` / `pg_stat_database` virtual tables.

## session and registry hooks

| item | role |
|---|---|
| `with_session_context(ctx, f)` | Run `f` with `ctx` installed as the current per-thread session context. |
| `current_session_context()` | Return the active context, if any. |
| `current_search_path_schemas()`, `current_database_name()`, `current_schema_name()`, `current_time_zone()`, `current_date_order()`, `current_interval_style()`, `current_lo_session_key()`, `current_temporal_session_context()` | Convenience accessors. |
| `set_extension_registry(reg)`, `with_extension_registry(reg, f)`, `extension_registry()` | Per-thread extension registry (`aiondb_extension::ExtensionRegistry`). |
| `enter_inlining_user_function(name)`, `is_inlining_user_function(name)` | Recursion guard used by the planner when inlining `CREATE CAST WITH FUNCTION` overrides. |
| `register_pg_statistics_objdef(oid, def)`, `lookup_pg_statistics_objdef(oid)` | `pg_statistic_ext` object-definition registry. |

## file-system scalar helpers

| function | role |
|---|---|
| `eval_pg_ls_dir_with_base_dir` | Backing for `pg_ls_dir`. |
| `eval_pg_read_file_with_base_dir` | Backing for `pg_read_file`. |
| `eval_pg_read_binary_file_with_base_dir` | Backing for `pg_read_binary_file`. |

All three accept the engine-supplied data directory so the evaluator never reads outside the cluster root.

## example

```rust
use aiondb_core::{DataType, Value};
use aiondb_eval::ExpressionEvaluator;
use aiondb_plan::TypedExpr;

let evaluator = ExpressionEvaluator;
let lit = TypedExpr::literal(Value::Int(7), DataType::Int, false);
let v = evaluator.evaluate(&lit).expect("evaluate");
assert_eq!(v, Value::Int(7));
```
