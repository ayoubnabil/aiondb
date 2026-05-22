---
title: Observability
order: 61
---

# Observability

AionDB starts an HTTP observability server for local health and metrics.

> New in v0.3: AionDB now presents SQL, graph, and vector retrieval as one engine story, backed by pgvector-style SQL, HNSW, IVF-flat, Qdrant-style filters, and reproducible vector benchmarks. See [What's New in v0.3](/documentation/project/whats-new-v0-3.html).

## Defaults

```bash
AIONDB_OBSERVABILITY_BIND=127.0.0.1
AIONDB_OBSERVABILITY_PORT=9187
```

Endpoints:

- `/livez`
- `/healthz`
- `/readyz`
- `/metrics`
- `/info`

## Local checks

```bash
curl http://127.0.0.1:9187/livez
curl http://127.0.0.1:9187/healthz
curl http://127.0.0.1:9187/readyz
curl http://127.0.0.1:9187/info
curl http://127.0.0.1:9187/metrics
```

## Startup behavior

By default, the server can continue in degraded mode if observability startup fails. To fail startup instead:

```bash
AIONDB_OBSERVABILITY_FAIL_FAST=true
```

## Security posture

The server treats public observability exposure as unsafe in v0.1. Keep observability on loopback unless the environment is explicitly secured.

## What to check first

Use `/livez` to check whether the observability HTTP process is alive. Use `/readyz` for supervisors and load balancers that need a pgwire readiness gate. `/healthz` is kept as a compatibility alias for readiness. Use `/info` to inspect basic runtime identity and configuration. Use `/metrics` for counters and gauges that are useful during local evaluation.

## Exposed metric families

`/metrics` emits Prometheus-compatible plain-text counters and gauges. The metric names below are stable enough to be used by local evaluation dashboards, but the exact set may grow between releases.

| Family | Names |
| --- | --- |
| Query lifecycle | `aiondb_queries_total`, `aiondb_queries_failed_total`, `aiondb_rows_returned_total`, `aiondb_rows_affected_total` |
| Query latency | `aiondb_query_duration_micros_total`, `aiondb_query_duration_micros_bucket{le="..."}`, `aiondb_query_duration_micros_sum`, `aiondb_query_duration_micros_count`, `aiondb_query_duration_micros_p50`, `aiondb_query_duration_micros_p95`, `aiondb_query_duration_micros_p99` |
| Concurrency | `aiondb_query_queue_depth_current`, `aiondb_query_queue_depth_peak`, `aiondb_session_lock_wait_total`, `aiondb_session_lock_wait_micros_total`, `aiondb_session_lock_wait_micros_max` |
| Graph DDL | `aiondb_graph_ddl_operations_total` |
| Distributed execution | `aiondb_distributed_fragments_total`, `aiondb_distributed_fragment_errors_total` |
| pgwire listener | `aiondb_pgwire_connections_total`, `aiondb_pgwire_connections_active`, `aiondb_pgwire_queries_total`, `aiondb_pgwire_successful_startups_total`, `aiondb_pgwire_failed_startups_total`, `aiondb_pgwire_authentication_failures_total` |
| Product contract | `aiondb_product_single_node_mode`, `aiondb_product_clustering_supported`, `aiondb_product_encryption_at_rest_supported`, `aiondb_product_backup_restore_supported` |
| Distributed topology | `aiondb_distributed_remote_nodes_total`, `aiondb_distributed_remote_nodes_available`, `aiondb_distributed_remote_circuits_open`, `aiondb_distributed_remote_circuits_half_open`, `aiondb_distributed_remote_node_available{node=...}`, `aiondb_distributed_remote_node_circuit_state{node=...}`, `aiondb_distributed_remote_node_consecutive_failures{node=...}` |
| Control plane | `aiondb_distributed_control_plane_nodes_total`, `aiondb_distributed_control_plane_nodes_live`, `aiondb_distributed_control_plane_node_live{node=...}`, `aiondb_distributed_control_plane_shards_total`, `aiondb_distributed_control_plane_placement_epoch` |
| Distributed replication | `aiondb_distributed_replication_shards_total`, `aiondb_distributed_replication_shards_with_live_quorum`, `aiondb_distributed_replication_shards_without_live_quorum`, `aiondb_distributed_replication_under_replicated_shards`, `aiondb_distributed_replication_shards_with_down_voters`, `aiondb_distributed_replication_shards_with_learners`, `aiondb_distributed_replication_learner_replicas`, `aiondb_distributed_replication_shard_live_quorum{shard_id=...}`, `aiondb_distributed_replication_node_leaders{node_id=...}`, `aiondb_distributed_replication_node_voters{node_id=...}`, `aiondb_distributed_replication_node_learners{node_id=...}` |
| Replica runtime | `aiondb_replica_runtime_sessions_started`, `aiondb_replica_runtime_sessions_succeeded`, `aiondb_replica_runtime_sessions_failed`, `aiondb_replica_runtime_reconnects`, `aiondb_replica_runtime_wal_bytes_received`, `aiondb_replica_runtime_standby_status_updates_sent`, `aiondb_replica_runtime_last_session_started_at_us` |
| Replica WAL receiver | `aiondb_replica_wal_receiver_write_lsn`, `aiondb_replica_wal_receiver_flush_lsn`, `aiondb_replica_wal_receiver_apply_lsn`, `aiondb_replica_wal_receiver_write_apply_lag_lsn`, `aiondb_replica_wal_receiver_flush_apply_lag_lsn` |

The `aiondb_product_*` gauges are dimensional booleans that describe what the running binary actually supports. They are useful for dashboards that need to refuse production-readiness claims a build cannot back.

During a benchmark or compatibility run, record:

- AionDB commit hash;
- server command and environment variables;
- observability bind address;
- start time;
- benchmark command;
- raw benchmark output.

That information makes a performance or reliability report useful after the machine has been shut down.

## Local debugging pattern

Start the server in one terminal:

```bash
AIONDB_BOOTSTRAP_USER=dev \
AIONDB_BOOTSTRAP_PASSWORD='ReplaceWithLongUniquePassword42!' \
cargo run -p aiondb-server --bin aiondb -- --ephemeral
```

Check health from another terminal:

```bash
curl -s http://127.0.0.1:9187/livez
curl -s http://127.0.0.1:9187/healthz
curl -s http://127.0.0.1:9187/readyz
curl -s http://127.0.0.1:9187/info
```

If the database accepts client connections but observability does not respond, check the bind address, port, and whether another process already owns the port.

## Graph `EXPLAIN` JSON

AionDB also exposes a structured graph observability payload through SQL `EXPLAIN`.

For the full JSON contract, field list, examples, and engine helper API, use [Explain JSON](/documentation/manage/explain-json.html).

Supported forms:

```sql
EXPLAIN (FORMAT JSON)
MATCH (a)-[:KNOWS]->(b)
RETURN b.id;

EXPLAIN (ANALYZE, FORMAT JSON)
MATCH (a)-[:KNOWS]->(b)
RETURN b.id;
```

`FORMAT JSON` returns a single-row JSON payload instead of the usual multi-line text plan. `ANALYZE` keeps the same JSON shape and adds runtime fields such as actual rows, actual selectivity, clause input/output rows, and lightweight timings.

### Contract

The payload is versioned:

- `schema_version = 1`
- `format_kind = "aiondb.explain_json"`

Top-level fields:

| Field | Meaning |
| --- | --- |
| `query_plan_lines` | Full text `EXPLAIN` output preserved as an array of lines. |
| `plan_lines` | Non-graph `EXPLAIN` lines. |
| `structural_plan_lines` | `plan_lines` without runtime summary lines such as `Execution:` or `Rows Returned:`. |
| `graph_lines` | Human-readable graph observability lines. |
| `plan_overview` | Stable summary of the non-graph plan root and primary operator. |
| `graph_summary` | Stable machine-readable summary of graph risk, pivots, joins, and drift. |
| `graph_detail` | Clause-level and pattern-level graph details. |
| `execution_summary` | Runtime summary when `ANALYZE` is used. |

### `plan_overview`

`plan_overview` is meant to be a small stable entry point for UI and automation.

Fields:

- `root_line`
- `root_kind`
- `primary_operator_line`
- `primary_operator_kind`
- `plan_category`
- `plan_subcategory`
- `line_count`
- `structural_line_count`
- `graph_line_count`

Current `plan_category` values include:

- `join`
- `scan`
- `sort`
- `aggregate`
- `limit`
- `project`
- `other`

Current `plan_subcategory` values include:

- `nested_loop`
- `hash_join`
- `merge_join`
- `index_scan`
- `seq_scan`
- `sort`
- `aggregate`
- `limit`
- `project`
- `query_wrapper`
- `other`

### `graph_summary`

`graph_summary` is the compact machine-readable graph health block.

Important fields include:

- `severity`
- `pivotable_patterns`
- `fragile_pivots`
- `blocked_pivots`
- `selected_non_leftmost`
- `selected_non_leftmost_source`
- `pivot_driver_metrics_source`
- `multi_pattern_clauses`
- `correlated_clauses`
- `shared_anchor_clauses`
- `correlated_shared_anchor`
- `correlated_non_shared`
- `shared_anchor_uncorrelated`
- `independent_multi_scan`
- `drift_patterns`
- `high_drift_patterns`
- `drift_metrics_source`
- `risky_join_clauses`
- `high_risk_join_clauses`
- `join_risk_metrics_source`
- `max_fanout`

Current `severity` values are:

- `ok`
- `watch`
- `risk`

### `graph_detail`

`graph_detail` contains:

- `summary`
- `clauses[]`

Each clause can expose:

- `kind`
- `clause_index`
- `optional`
- `patterns`
- `actual_input_rows`
- `actual_output_rows`
- `actual_selectivity`
- `actual_time_ms`
- `join_risk`
- `pattern_details[]`

`join_risk` can expose:

- `severity`
- `fanout`
- `basis`
- `join_risk_source`
- `correlated`
- `correlated_source`
- `shared_anchor`
- `shared_anchor_source`
- `join_shape`
- `join_shape_source`
- `patterns`

Each pattern detail can expose:

- `estimated_rows`
- `actual_rows`
- `estimate_error_ratio`
- `estimated_selectivity`
- `actual_selectivity`
- `actual_time_ms`
- `seed`
- `seed_mode`
- `seed_mode_source`
- `seed_binding_state`
- `seed_binding_state_source`
- `correlated_vars`
- `correlated_vars_source`
 - `seed_constraints`
 - `seed_constraints_source`
- `pattern_runtime_strategy`
- `pattern_runtime_strategy_source`
- `pattern_runtime_reason`
- `pattern_runtime_reason_source`
- `pivot_driver`
- `pivot_driver_source`
- `pivot_reason`
- `pivot_reason_source`
- `pivot_decision`
- `pivot_decision_source`
- `pivot_margin`
- `pivot_competition`
- `pivot_scores`
 - `first_rel`
 - `first_rel_source`
 - `first_rel_mode`
 - `first_rel_mode_source`
 - `first_rel_constraints`
 - `first_rel_constraints_source`
 - `bound_vars`
 - `bound_vars_source`
- `shape`
- `shape_source`
- `flags`
- `flags_source`
- `warning_severity`

### Provenance and trust

The graph payload distinguishes between values that were observed at runtime and values that were inferred from static plan shape.

Typical provenance fields include:

- `query_runtime_source`
- `selected_non_leftmost_source`
- `pivot_driver_metrics_source`
- `drift_metrics_source`
- `join_risk_metrics_source`
- `graph_detail.clauses[*].join_risk.join_risk_source`
- `graph_detail.clauses[*].join_risk.correlated_source`
- `graph_detail.clauses[*].join_risk.shared_anchor_source`
- `graph_detail.clauses[*].join_risk.join_shape_source`
- `runtime_strategy_source`
- `pattern_runtime_strategy_source`
- `pattern_runtime_reason_source`
- `seed_mode_source`
- `seed_binding_state_source`
- `correlated_vars_source`
- `seed_constraints_source`
- `pivot_driver_source`
- `pivot_reason_source`
- `pivot_decision_source`
- `first_rel_source`
- `first_rel_mode_source`
- `first_rel_constraints_source`
- `bound_vars_source`
- `flags_source`
- `shape_source`

Current values are:

- `observed`
- `inferred`
- `mixed`
- `unavailable`

Under plain `EXPLAIN (FORMAT JSON)`, most runtime-facing fields are `inferred` or `unavailable`.

Under `EXPLAIN (ANALYZE, FORMAT JSON)`, the payload can carry `observed` or `mixed` values when the engine has real runtime evidence.

### Text `EXPLAIN` lines

The plain text graph lines also carry provenance on the main summaries and warnings.

Examples:

- `Graph Summary Severity: ... source=inferred|observed|mixed`
- `Graph Planner Warning: ... source=inferred|observed`
- `Graph Pivot Hint: ... source=inferred|observed`
- `Graph Join Hint: ... source=inferred`
- `Graph Access Summary: ... source=inferred`
- `Graph Drift Summary: ... source=observed`
- `Graph Join Fanout Summary: ... source=observed`

This is mainly for operator readability. Product logic should still prefer the JSON payload.

### `execution_summary`

`execution_summary` is present in both modes, but runtime values are only populated under `ANALYZE`.

Fields:

- `kind`
- `rows_returned`
- `memory_used_bytes`

Under plain `EXPLAIN (FORMAT JSON)`, these runtime fields can be `null`.

### Example

Abbreviated payload:

```json
{
  "schema_version": 1,
  "format_kind": "aiondb.explain_json",
  "plan_overview": {
    "root_kind": "Cypher Query",
    "primary_operator_kind": "Nested Loop",
    "plan_category": "join",
    "plan_subcategory": "nested_loop"
  },
  "graph_summary": {
    "severity": "watch",
    "fragile_pivots": 1,
    "risky_join_clauses": 0,
    "max_fanout": null
  },
  "graph_detail": {
    "summary": {
      "severity": "watch"
    },
    "clauses": [
      {
        "kind": "PipelineMatch",
        "pattern_details": [
          {
            "seed_mode": "label_scan",
            "pivot_decision": "retained_leftmost"
          }
        ]
      }
    ]
  },
  "execution_summary": {
    "kind": "Query",
    "rows_returned": 1,
    "memory_used_bytes": 5283
  }
}
```

The contract is intended for local tooling, UI work, and future planner feedback loops. Keep clients tolerant to additive fields and reject only on incompatible `schema_version` or `format_kind`.

### Consuming the payload

From a SQL client such as `psql`, `FORMAT JSON` returns a single text cell that contains the JSON document:

```sql
EXPLAIN (FORMAT JSON)
MATCH (a)-[:KNOWS]->(b)
RETURN b.id;
```

That is the right path for ad hoc inspection, shell tooling, and compatibility with existing SQL clients.

Inside the engine, prefer the structured helpers instead of reparsing text output:

- `QueryEngine::execute_explain_graph_summary_json(session, sql, analyze)`
- `QueryEngine::execute_explain_graph_detail_json(session, sql, analyze)`

Those helpers:

- prepend `EXPLAIN` or `EXPLAIN ANALYZE`;
- execute the statement;
- extract the structured graph payload;
- return `serde_json::Value`.

Minimal Rust sketch:

```rust
use aiondb_engine::engine::api::QueryEngine;

fn load_graph_summary(
    engine: &dyn QueryEngine,
    session: &aiondb_engine::session::SessionHandle,
) -> aiondb_core::error::DbResult<serde_json::Value> {
    engine.execute_explain_graph_summary_json(
        session,
        "MATCH (a)-[:KNOWS]->(b) RETURN b.id",
        true,
    )
}
```

For UI or telemetry work:

- use `graph_summary` for badges, coarse severity, and top-level warnings;
- use `graph_detail` for clause and pattern drill-down;
- use `plan_overview` for quick SQL plan labeling;
- keep `query_plan_lines` only for raw rendering or debugging.

## Production-style guidance

For v0.1, do not expose observability directly to the public internet. Put it behind local networking, firewall rules, or a trusted collection agent. Treat metrics as operational data: they may reveal database names, runtime shape, workload volume, or error patterns.

## What is not covered yet

The v0.1 observability story is intentionally small. A mature deployment story would also need structured tracing, stable metric names, documented alert thresholds, log redaction policy, dashboard examples, and integration tests for degraded observability startup.
