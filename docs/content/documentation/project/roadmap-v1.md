---
title: Roadmap to v1
order: 90
---

# Roadmap to v1.0

This roadmap is the feature plan from `v0.1 alpha` to `v1.0 GA`. Each milestone lists what is added or solidified in that release. The numbering is a direction, not a date promise.

## High-level milestone map

| Tag | Theme | Single sentence |
| --- | --- | --- |
| `v0.2` | Foundations | Stable on-disk format, driver coverage, honest type system. |
| `v0.3` | Drivers and ORMs | Extended protocol parity, ORM-generated SQL works for CRUD. |
| `v0.4` | Durability | Crash recovery, page checksums, physical backup, PITR. |
| `v0.5` | Query engine | Cost-based optimizer with statistics, parallel scans, real EXPLAIN. |
| `v0.6` | Graph and vector | Hybrid planner, filtered ANN, variable-length paths. |
| `v0.7` | Replication | Streaming physical replication, logical decoding, read replicas. |
| `v0.8` | High availability | Automatic failover with consensus. |
| `v0.9` | Security and operations | TLS, SCRAM, RBAC, row-level security, audit, full observability. |
| `v1.0-rc` | Stabilization | Frozen public API, supported upgrade matrix, signed artifacts. |
| `v1.0` | GA | LTS line, public CVE process, runbooks. |

## v0.2 — Foundations

### Storage format

- Frozen v1 page layout: header, checksum slot, slot directory, tuple format.
- Magic number and format version on every file under the data directory.
- `aiondb doctor` reports the format version and refuses unknown versions.
- `aiondb upgrade` covers v0.1 → v0.2.

### WAL contract

- Record types frozen for the v0.2 line.
- WAL segment naming and rotation documented.
- LSN semantics documented.
- Idempotent recovery.

### Type system

- Published type mapping matrix (generated, not prose).
- Per PostgreSQL OID: text format, binary format, accepted casts, overflow error class, NULL handling.
- All unsupported types fail with `SQLSTATE 0A000` and a clear error message.

### Driver compatibility surface

- Smoke matrix across `libpq`/`psql`, `psycopg`, `asyncpg`, `pgx`, `node-postgres`, `npgsql`, `JDBC`.
- Connect, simple query, prepared query, transaction, error recovery, COPY in/out.
- Public compatibility matrix page.

### Catalog primer

- `pg_catalog`: `pg_class`, `pg_attribute`, `pg_type`, `pg_namespace`, `pg_index`, `pg_constraint`, `pg_proc`, `pg_database`, `pg_roles`.
- `information_schema.tables`, `columns`, `key_column_usage`, `table_constraints`.

## v0.3 — Drivers and ORMs

### Wire protocol

- Extended query parity: Parse, Bind, Describe, Execute, Sync, Flush, Close, command tags, row descriptions in text and binary.
- Named prepared statements and named portals.
- `COPY FROM STDIN` and `COPY TO STDOUT`, text and binary.
- Cursor support: `DECLARE CURSOR`, `FETCH`, `MOVE`, `CLOSE`.
- Cancel protocol (out-of-band cancellation).
- Notice and notification frames: `NOTICE`, `RAISE`, `LISTEN`/`NOTIFY` (with documented contract).

### Authentication

- `SCRAM-SHA-256` as default mechanism.
- `pg_hba`-style configuration file with method per host/user/database.
- Cleartext only behind a non-default flag with a loud warning.

### Prepared statements

- Plan cache keyed by SQL text and parameter type oids.
- Plan invalidation on DDL.
- Documented parameter inference and unknown type behavior.

### ORM compatibility

- SQLAlchemy, Django ORM, Prisma, Drizzle, Diesel, ActiveRecord, Hibernate.
- Schema introspection, table CRUD, basic relations, transaction commit and rollback, error mapping.
- Public compatibility report per ORM with version pinned.

### Migration tools

- `sqlx`, `Alembic`, `Django migrate`, `Prisma migrate`, `Flyway`, `Liquibase`.
- Create table, alter column, add index, drop column, foreign key, generated column where supported.

### Error reference

- SQLSTATE coverage extended to all paths exercised by the ORM suite.
- Error reference page generated from source.

## v0.4 — Durability and recovery

### Crash recovery

- Failpoints in WAL append, page write, fsync, segment rotation, checkpoint.
- After any crash, replayed state equals the last committed transaction state.

### Page integrity

- Checksum on every data page (CRC32C or xxh3).
- Checksums verified on read; corrupted pages produce `SQLSTATE XX001` with file/offset/expected/actual.
- `aiondb doctor` walks the data directory and reports all checksum failures.

### Torn writes

- Full-page writes in WAL or atomic page write strategy.

### fsync semantics

- Documented fsync points: WAL flush on commit, checkpoint, segment rotation.
- Configuration knobs: `synchronous_commit`, `fsync`, `wal_sync_method`.

### Backup and restore

- `aiondb basebackup` produces a consistent physical snapshot online.
- WAL archive command and restore command.
- Point-in-time recovery using base backup plus WAL.
- Logical `aiondb dump` and `aiondb restore` cover schema, data, indexes, sequences, vector indexes, graph labels.

## v0.5 — Query engine

### Statistics

- `ANALYZE` populates per-column histograms, most common values, null fraction, ndistinct.
- `pg_stats` and `pg_statistic` views reflect gathered statistics.
- Auto-analyze daemon with configurable thresholds.

### Optimizer

- Cost model with documented IO and CPU coefficients.
- Plan space: nested loop, hash join, sort-merge join, hash aggregate, sort aggregate, index scan, bitmap scan, parallel sequential scan.
- Join ordering by dynamic programming up to a configurable join count, then greedy.

### Execution

- Vectorized batches for hot operators: scan, filter, project, hash join build, aggregation.
- Parallel sequential scan with worker count knob.
- Memory accounting per operator with spill to disk for hash join, aggregation, sort.

### EXPLAIN

- `EXPLAIN (ANALYZE, BUFFERS, FORMAT JSON)` with stable output: time per node, rows per node, planned vs actual, buffer hits, operator-specific stats.
- `auto_explain`-style logging for slow queries.

### Index types

- B-tree (existing), hash index, partial index, expression index.

## v0.6 — Graph and vector

### Graph

- SQL/PGQ alignment: `MATCH ... PATTERN` syntax stabilized.
- Variable-length paths with explicit min and max bounds.
- Cycle detection in path queries.
- Shortest path operator with cost-based selection between BFS, bidirectional BFS, and Dijkstra (weighted).
- Edge endpoint nullability part of the label DDL.

### Vector

- Distance functions: L2, cosine, inner product, Hamming.
- Index types: HNSW (existing), IVF-Flat, IVF-PQ, optional DiskANN for larger-than-memory sets.
- Filtered ANN: predicate pushdown into the index scan with a documented recall/latency tradeoff knob.
- Multi-vector columns per table.
- Quantization: scalar and product quantization, opt-in per index.

### Hybrid planner

- Single optimizer pass for queries mixing relational filters, graph traversal, and vector scoring.
- Cost model accounts for index selectivity from all three families.

## v0.7 — Replication

### Physical replication

- WAL streaming over the replication protocol.
- Replica is read-only and serves `psql` traffic.
- Replication slots with retention.
- Cascading replication.
- Synchronous and asynchronous modes with documented commit semantics.

### Logical replication

- Output plugin model with at least one provided plugin (JSON change events).
- Per-table publication and subscription.
- Conflict policy documentation for asymmetric topologies.

### Monitoring views

- `pg_stat_replication`-like views with lag in bytes and seconds.

### Backups in a replicated setup

- Base backup taken from a replica.
- Restore exercised with primary failure plus replica promotion.

## v0.8 — High availability

### Consensus

- Raft (or equivalent) for leader election and metadata changes.
- Quorum commit option with documented latency tradeoff.
- Consensus-replicated cluster membership changes.

### Failover

- Automatic failover within a documented detection window.
- Fencing of the previous primary.
- Client reconnect guidance per supported driver.

### Cluster operations

- `aiondb cluster init`, `aiondb cluster join`, `aiondb cluster status`, `aiondb cluster step-down`.
- Documented procedure for adding and removing nodes safely.

### Sharding

- Sharding stays optional and behind a feature flag, with its own consistency documentation. If not ready, v1.0 ships the single-cluster contract and defers sharding to v1.x.

## v0.9 — Security and operations

### Transport security

- TLS 1.2+ on pgwire and HTTP endpoints.
- Server-side and client-side certificate verification.
- mTLS option.
- Cipher suite policy aligned with current Mozilla intermediate guidance.

### Authentication

- SCRAM-SHA-256 (from v0.3), certificate auth, optional LDAP, optional OIDC for the management surface.
- Pluggable auth interface.

### Authorization

- Roles, groups, GRANT, REVOKE, DEFAULT PRIVILEGES, role inheritance.
- Row-level security policies.
- Column-level grants.
- Object ownership and ownership transfer.

### Audit

- Append-only audit log with documented format.
- Coverage: connection events, DDL, privileged DML, role changes, configuration changes, failed authentications.
- Rotation and shipping documented.

### Observability

- Prometheus metrics: connection count, transaction rate, WAL bytes, replication lag, page reads/writes, cache hit ratio, vector index recall sample, graph traversal depth distribution.
- OpenTelemetry traces with span per operator.
- Structured logs with correlation IDs.
- Grafana dashboard JSON shipped in `integrations/grafana/`.

### Connection management

- Built-in connection pool with documented sizing guidance, or first-class PgBouncer guidance.
- `idle_in_transaction_session_timeout`, `statement_timeout`, `lock_timeout` honored.

### Kubernetes

- Helm chart in `packaging/helm/` with values for storage class, replica count, TLS, backup destination.
- Kubernetes operator in `packaging/operator/` with reconcile logic for cluster, backup, restore, upgrade.

### Container hardening

- Distroless or minimal base image.
- Non-root user.
- Read-only filesystem with explicit writable volumes.

### Configuration

- Every value has a default, a description, and a category in `manage/configuration.md`.
- Reload behavior (restart, SIGHUP, online) documented per parameter.

## v1.0-rc — Stabilization

### Freeze

- Public APIs frozen: pgwire features, SQL surface, HTTP control endpoints, configuration keys, embedded Rust API, CLI flags, environment variables, metric names, log fields.
- A breaking change between RC and GA requires a documented justification and a migration note.

### Upgrade matrix

- In-place upgrade from every minor on the v0.x line.
- Downgrade is not supported and is documented as such.

### Release engineering

- All artifacts (binaries, archives, container images, Helm chart, operator image) signed with cosign.
- SBOM generated per artifact.
- SLSA build provenance attestations published.
- Reproducible build documented for the server binary.

## v1.0 — GA, production ready

### Release contents

- Signed release archives for Linux x86_64 and arm64, macOS arm64 (developer-only).
- Signed container images for the server and Studio.
- Signed Helm chart and operator image.
- SBOM and SLSA attestations per artifact.

### Support commitments

- v1.0 is the first long-term support line.
- LTS window documented (for example 18 months of fixes and 6 months of security-only).
- Public CVE process under `SECURITY.md` with private disclosure address, response SLA, credit policy.
- Public release cadence for v1.x minors.

### Runbooks

Documented under `docs/content/documentation/manage/runbooks/`:

- cold start;
- planned restart;
- primary failure and failover;
- corrupted page on a replica;
- WAL archive backlog;
- backup verification;
- upgrade from v0.9 and from v1.0 minors;
- rollback strategy (restore from backup, not in-place downgrade);
- capacity planning.

### Migration importers

- PostgreSQL: schema and data via logical dump.
- pgvector: vector columns.
- Neo4j: label-property graphs via Cypher subset.
