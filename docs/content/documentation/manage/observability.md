---
title: Observability
order: 61
---

# Observability

AionDB starts an HTTP observability server for local health and metrics.

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
AIONDB_BOOTSTRAP_PASSWORD='DevPassword42!' \
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

## Production-style guidance

For v0.1, do not expose observability directly to the public internet. Put it behind local networking, firewall rules, or a trusted collection agent. Treat metrics as operational data: they may reveal database names, runtime shape, workload volume, or error patterns.

## What is not covered yet

The v0.1 observability story is intentionally small. A mature deployment story would also need structured tracing, stable metric names, documented alert thresholds, log redaction policy, dashboard examples, and integration tests for degraded observability startup.
