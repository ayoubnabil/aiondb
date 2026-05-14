---
title: Control Plane
order: 74
---

# Control Plane

AionDB v0.1 does not include a managed cloud service. The current control-plane work is a local operator surface: configuration, health, metrics, storage doctor, backup/restore, dashboard metadata, and pgAdmin integration.

## Current Operator Surfaces

| Surface | Command or endpoint | Purpose |
| --- | --- | --- |
| Server config | CLI flags and `AIONDB_*` env vars | repeatable startup |
| Liveness | `GET /livez` | observability HTTP process liveness |
| Health | `GET /healthz` | compatibility alias for pgwire readiness |
| Readiness | `GET /readyz` | load-balancer or supervisor readiness |
| Metrics | `GET /metrics` | Prometheus-compatible counters and gauges |
| Product info | `GET /info` | release-line contract and support boundaries |
| Storage doctor | `aiondb doctor --data-dir <path>` | offline data-dir inspection |
| Storage upgrade | `aiondb upgrade --data-dir <path>` | storage v1 manifest upgrade |
| Logical backup | `aiondb dump` / `aiondb restore` | canonical v0.1 recovery path |
| pgAdmin profile | `make dashboard-pgadmin` | SQL administration through pgwire |
| Replication role | `AIONDB_REPLICATION_ROLE=primary\|replica` | warm-standby driver and apply tracker; reconnects with exponential backoff and resumes at the local `flush_lsn` |
| HA gating | `AIONDB_HA_*` env vars | heartbeat / election / fencing knobs for the embedded HA layer |

When a replica participates in distributed repair, include
`application_name=<NodeId>` in `AIONDB_REPLICATION_PRIMARY_CONNINFO` so
primary-side WAL progress can be mapped back to the learner placement.
With HA and distributed auto-rebalance enabled, the HA tick runs that
mapping on the current primary.

## Managed Cloud Gap

The following are not v0.1 product claims:

- tenant provisioning API;
- billing and metering;
- managed backups;
- region placement;
- automated failover;
- online point-in-time recovery;
- hosted console.

These are cloud product features, not database kernel features. They should not be implied until there is a working service boundary and operator test suite.

## Near-Term Control-Plane Backlog

1. Add a machine-readable readiness endpoint that reports storage format, backup support, topology, and compatibility level.
2. Add dashboard views for storage doctor output and backup/restore status.
3. Add release artifacts with checksums and signed provenance.
4. Add a tenant/project metadata model only after single-node local operations are reliable.
5. Add managed backup scheduling only after the logical backup path has restore drills in CI.

## Evaluation Guidance

For v0.1, evaluate AionDB as a local single-node database with PostgreSQL-facing connectivity. If the target business requires hosted multi-region operations immediately, record that as a product gap rather than a database-engine bug.
