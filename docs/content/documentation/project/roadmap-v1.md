---
title: Roadmap to v1
order: 90
---

# Roadmap to v1.0

This roadmap is the feature plan from `v0.1 alpha` to `v1.0 GA`. Each milestone is a product challenge with a clear competitive target, not a parking lot for small tasks. The numbering is a direction, not a date promise, and this page frames milestones by scope, ambition, and evidence.

## Ambition model

Each minor milestone should move AionDB closer to a defensible position against specialized systems:

- graph workloads should be benchmarked against graph databases such as Neo4j;
- relational joins and SQL planning should be benchmarked against PostgreSQL;
- distributed SQL behavior should be benchmarked against systems such as CockroachDB or YugabyteDB;
- vector and hybrid retrieval should be benchmarked against pgvector and dedicated vector engines where the workload fits;
- operational readiness should be validated through failure drills, not prose.

These are targets for engineering direction and public evidence. A release should not claim to beat another system unless the benchmark setup, data shape, product versions, hardware, configuration, and failure cases are published.

Patch releases, documentation updates, security fixes, and narrowly scoped compatibility improvements may ship between milestones. Minor milestones are reserved for coherent capability changes that can be explained, reproduced, tested, and operated.

## High-level milestone map

| Tag | Theme | Big objective | Required evidence |
| --- | --- | --- | --- |
| `v0.2` | Credibility challenge | Prove AionDB is a real PostgreSQL-wire database foundation: stable storage, WAL contracts, real drivers, honest type behavior, and reproducible evaluation. | Storage contract tests, WAL contract tests, upgrade path, type matrix, driver smoke results, baseline benchmark reports. |
| `v0.3` | Graph engine challenge | Build a serious graph engine: core graph algorithms, path queries, graph indexes, and public benchmarks against Neo4j-class workloads. | Graph algorithm suite, Cypher/SQL-PGQ compatibility notes, traversal benchmarks, memory profiles, correctness corpus. |
| `v0.4` | PostgreSQL-class query engine | Push joins, planning, execution, and EXPLAIN toward PostgreSQL-class behavior on pinned SQL workloads. | Join benchmark corpus, optimizer regression suite, statistics fixtures, EXPLAIN JSON snapshots, spill and memory tests. |
| `v0.5` | Distributed SQL challenge | Move from single-node credibility to distributed SQL: sharding, distributed transactions, replica placement, and correctness drills against CockroachDB/Yugabyte-style expectations. | Jepsen-style scenarios, distributed transaction tests, shard rebalancing drills, replication lag metrics, failure recovery reports. |
| `v0.6` | Hybrid graph/vector/SQL | Make hybrid queries the differentiator: relational filters, graph traversal, and vector scoring in one planner and one dataset. | Hybrid query corpus, recall/latency reports, graph semantics tests, vector benchmark matrix, planner explainability docs. |
| `v0.7` | Durability and recovery | Make storage failures boring: checksums, crash recovery, physical backup, WAL archive, PITR, and verified restore. | Failpoint matrix, recovery invariants, checksum corruption tests, backup/restore rehearsals, PITR transcripts. |
| `v0.8` | High availability | Turn distributed primitives into operator-facing HA: automatic failover, fencing, read replicas, and cluster procedures. | Failure-injection suite, fencing validation, cluster operation runbooks, client reconnect guidance, promotion tests. |
| `v0.9` | Security and operations | Make the system operable by default: TLS, SCRAM, RBAC, RLS, audit, observability, containers, and Kubernetes. | Security matrix, auth interop tests, audit coverage map, metrics and tracing dashboards, hardened packaging checks. |
| `v1.0-rc` | Stabilization | Frozen public API, supported upgrade matrix, signed artifacts. | Upgrade matrix, API freeze ledger, signed release artifacts, compatibility reports. |
| `v1.0` | GA | LTS line, public CVE process, runbooks. | Support policy, CVE workflow, production runbooks, migration importers, release governance. |

## Release quality gates

Every milestone should satisfy the same basic bar before its tag is credible:

- The public documentation must match the code paths that ship.
- The release notes must separate implemented behavior from roadmap intent.
- New user-facing behavior must have reduced tests that fail without the implementation.
- Compatibility claims must name exact clients, driver versions, query shapes, and known exclusions.
- Storage, WAL, wire protocol, and API changes must include upgrade or migration notes.
- Operational features must have a command, a config surface, an observable signal, and a failure-mode note.
- Benchmarks must be reproducible from a fresh checkout or explicitly marked as exploratory.
- Unsupported behavior must fail clearly or be documented as undefined for that release.

## v0.2 — Credibility challenge

The v0.2 target is to prove that AionDB is not a prototype hidden behind a demo query. A technical evaluator should be able to clone the project, run the server, connect through real PostgreSQL clients, inspect the storage contract, exercise WAL behavior, and understand exactly which parts are stable or unsupported.

### Storage format

- Frozen v1 page layout: header, checksum slot, slot directory, tuple format.
- Magic number and format version on every stable file under the data directory.
- Storage manifest with readable major/minor version and release line.
- `aiondb doctor` reports the format version, stable artifacts, experimental artifacts, and corruption findings.
- `aiondb doctor` refuses unknown future majors instead of silently opening unsafe data.
- `aiondb upgrade` covers v0.1 → v0.2 and backs up the previous data directory state.
- Documentation names which file kinds are stable and which remain experimental.

### WAL contract

- Record types frozen for the v0.2 line.
- WAL segment naming and rotation documented.
- LSN semantics documented.
- Idempotent recovery.
- WAL segment header documented with magic, format version, LSN mode, system identifier, and timeline.
- Frozen WAL tag table asserted by tests so renumbering or reordering records breaks loudly.
- Replay from the same start LSN produces the same entry sequence across reopen.
- Corrupt tails are handled as recovery boundaries rather than undefined behavior.

### Type system

- Published type mapping matrix (generated, not prose).
- Per PostgreSQL OID: text format, binary format, accepted casts, overflow error class, NULL handling.
- All unsupported types fail with `SQLSTATE 0A000` and a clear error message.
- Numeric overflow, invalid casts, NULL handling, and unsupported binary formats are reduced into compatibility tests.
- The docs distinguish "implemented", "accepted but lossy", "text-only", "binary-supported", and "unsupported".
- Type behavior is tested through pgwire clients, not only through internal Rust APIs.

### Driver compatibility surface

- Smoke matrix across `libpq`/`psql`, `psycopg`, `asyncpg`, `pgx`, `node-postgres`, `npgsql`, `JDBC`.
- Connect, simple query, prepared query, transaction, error recovery, COPY in/out.
- Public compatibility matrix page.
- Each driver result pins driver version, server flags, protocol path, and failing query shape.
- Prepared statement behavior documents parameter inference, unknown type handling, and plan invalidation limits.
- Unsupported extended-protocol paths return precise errors rather than hanging or returning misleading row descriptions.

### Catalog primer

- `pg_catalog`: `pg_class`, `pg_attribute`, `pg_type`, `pg_namespace`, `pg_index`, `pg_constraint`, `pg_proc`, `pg_database`, `pg_roles`.
- `information_schema.tables`, `columns`, `key_column_usage`, `table_constraints`.
- Enough catalog behavior for drivers and simple tools to introspect tables, columns, indexes, constraints, roles, and database names.
- Unsupported catalog rows are omitted or explicitly marked rather than fabricated.

### Baseline benchmark target

- Public baseline scripts for startup, simple inserts, point reads, range scans, WAL append, checkpoint, and restart recovery.
- Comparisons against PostgreSQL are allowed only as baseline context, not as a claim that AionDB is faster at this stage.
- Metrics: startup time, first connection latency, rows inserted per second, point read latency, WAL bytes written, recovery time, and memory footprint.
- Benchmarks run from a fresh checkout with documented hardware, config, dataset size, and command lines.
- The report should explain bottlenecks that later milestones are designed to attack.

### Documentation bar

- Storage format and WAL contract pages are treated as release artifacts.
- Limitations are documented before release notes make broad claims.
- The roadmap links ambitious future work to concrete prerequisites already delivered in v0.2.
- Examples must be runnable with normal PostgreSQL clients where possible.

### Exit criteria

- A fresh user can create a data directory, inspect it, upgrade it, and understand which artifacts are stable.
- A maintainer can change storage or WAL code and immediately see contract tests fail when a frozen byte layout drifts.
- The docs explain which PostgreSQL types, casts, binary formats, and wire behaviors are supported.
- Driver smoke tests produce a public matrix with exact versions and reduced failing cases for unsupported behavior.
- The release avoids broad "PostgreSQL compatible" language unless the claim is backed by a named compatibility suite.
- Baseline performance and recovery reports are reproducible and honest about where AionDB is still behind.
- The release gives later graph, query-engine, distributed, hybrid, and HA milestones a stable base instead of moving the lower-level contract every time.

## v0.3 — Graph engine challenge

The v0.3 target is to make graph a first-class reason to use AionDB. The goal is not merely to expose graph syntax over tables; it is to build a graph execution engine with enough algorithm coverage and performance evidence to be compared against Neo4j-class workloads.

### Graph model and syntax

- SQL/PGQ alignment for `MATCH ... PATTERN` over relational tables.
- Cypher-inspired compatibility subset documented where it improves migration from existing graph workloads.
- Explicit node labels, edge labels, edge direction, endpoint nullability, and property mapping.
- Graph catalog views that explain how graph labels map to underlying tables and indexes.
- Deterministic semantics for duplicate edges, null endpoints, self loops, and multi-label entities.

### Algorithm coverage

- Breadth-first search and depth-first traversal with bounded depth.
- Shortest path: unweighted BFS, bidirectional BFS, and weighted Dijkstra.
- All-shortest-path variants where the result cardinality is bounded by explicit limits.
- Connected components, weakly connected components, strongly connected components.
- PageRank, degree centrality, closeness centrality, betweenness centrality with documented approximation modes.
- Triangle counting, community detection baseline, top-k neighbors, k-hop neighborhood expansion.
- Cycle detection and path uniqueness modes.

### Graph performance work

- Adjacency storage tuned for hot traversal paths.
- Compressed adjacency lists for high-degree nodes.
- Direction-specific indexes for outgoing, incoming, and bidirectional traversals.
- Batch traversal execution that avoids per-edge executor overhead.
- Cost model for choosing index traversal, adjacency scan, or relational prefiltering.
- Memory budget controls for path expansion, centrality, and all-shortest-path workloads.

### Neo4j-class benchmark target

- Public benchmark suite with pinned Neo4j and AionDB versions.
- Workloads covering shortest path, k-hop traversal, centrality, community detection, write-heavy edge ingestion, and mixed property filters.
- Datasets that include sparse graphs, dense hubs, social-network shapes, knowledge-graph shapes, and weighted road-network shapes.
- Metrics: latency distribution, throughput, memory use, load time, index build time, result correctness, and failure modes under limits.
- No claim of beating Neo4j unless the published benchmark supports it for a named workload.

### Driver and tool surface

- Graph queries remain accessible through normal PostgreSQL clients.
- Result shapes are stable enough for application code and benchmarks.
- Error messages distinguish unsupported syntax, unsupported algorithm modes, and resource-limit aborts.
- Documentation includes migration examples from table-only schemas and graph-native schemas.

### Exit criteria

- AionDB can run a serious graph benchmark suite from a fresh checkout.
- Core graph algorithms have correctness fixtures and resource-limit tests.
- Graph performance reports compare against pinned Neo4j-class baselines without hiding dataset or configuration details.
- The planner can explain traversal order, index choices, and memory budget decisions.
- Unsupported graph features are explicit; the release does not imply full Cypher or full SQL/PGQ compatibility without evidence.

## v0.4 — PostgreSQL-class query engine

The v0.4 target is to make relational execution credible against PostgreSQL on pinned SQL workloads. The ambition is to push joins, statistics, planning, execution, and `EXPLAIN` close enough to PostgreSQL-class behavior that performance gaps are measurable and explainable, not architectural mysteries.

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

### PostgreSQL-class join target

- Benchmark nested-loop, hash join, sort-merge join, semi join, anti join, lateral joins, and multi-way joins against pinned PostgreSQL versions.
- Cover OLTP point lookups, star-schema joins, selective joins, low-selectivity joins, skewed joins, and join-plus-aggregation workloads.
- Track planning time, execution time, memory, spill volume, row estimate error, and plan stability.
- Target PostgreSQL-class behavior on supported query shapes; when AionDB is slower, publish why.
- Avoid broad "as fast as PostgreSQL" claims without workload names and reproducible scripts.

### SQL compatibility pressure

- Extended query parity: Parse, Bind, Describe, Execute, Sync, Flush, Close.
- Named prepared statements and named portals.
- `COPY FROM STDIN` and `COPY TO STDOUT`, text and binary.
- Cursor support: `DECLARE CURSOR`, `FETCH`, `MOVE`, `CLOSE`.
- ORM-generated CRUD from SQLAlchemy, Django ORM, Prisma, Drizzle, Diesel, ActiveRecord, Hibernate.
- Migration-tool workflows from `sqlx`, Alembic, Django migrate, Prisma migrate, Flyway, and Liquibase.

### Exit criteria

- `ANALYZE` produces statistics that visibly affect plan selection on representative queries.
- Planner regressions are tested with stable logical expectations and stable JSON explain snapshots where appropriate.
- Operators that can spill to disk expose memory accounting and bounded failure modes.
- Parallel execution has deterministic correctness tests and documented performance caveats.
- Index selection can be explained from available statistics rather than hidden heuristics.
- PostgreSQL comparison reports are generated from benchmark scripts, not manually edited tables.

## v0.5 — Distributed SQL challenge

The v0.5 target is to make AionDB a credible distributed SQL system, not only a local engine with networking. The benchmark and correctness pressure should be CockroachDB/Yugabyte-style: serializable behavior, shard movement, replica failure, node loss, rejoin, and observable recovery.

### Sharding and placement

- Range or hash sharding with documented key selection and split behavior.
- Replica placement rules with explicit zone/rack/node labels.
- Online shard movement with backpressure and progress visibility.
- Hot-shard detection and rebalance recommendations.
- Metadata catalog for shard ownership, leaseholder/leader state, and replica health.

### Distributed transactions

- Serializable transaction contract for supported distributed writes.
- Deadlock detection or prevention with clear retry semantics.
- Read timestamps, write intents, conflict handling, and transaction retry errors.
- Cross-shard commit protocol with durable decision records.
- SQLSTATE mapping for retryable and non-retryable distributed transaction failures.

### Replication protocol

- WAL streaming over the replication protocol.
- Per-shard replication streams and retention.
- Physical read replicas for supported shard sets.
- Synchronous and asynchronous modes with documented commit semantics.
- Lag metrics by shard, replica, and transaction class.

### CockroachDB/Yugabyte-style benchmark target

- Public correctness scenarios inspired by distributed SQL failure modes: partition, clock skew, slow disk, node restart, replica lag, and rebalancing during writes.
- Comparative benchmark scripts against pinned distributed SQL baselines where the data model and consistency level are comparable.
- Workloads: TPC-C-like transactions, multi-region read/write patterns, range scans, secondary-index writes, and hotspot contention.
- Metrics: commit latency, abort/retry rate, failover impact, recovery time, consistency violations, storage amplification, and operator-visible state.
- Claims must name the exact workload where AionDB is better, comparable, or behind.

### Exit criteria

- Distributed writes survive process crashes, node loss, network partitions, and replica rejoin in reduced tests.
- Operators can see shard placement, replication lag, transaction retries, and unsafe states from documented commands or views.
- The system rejects unsupported distributed modes clearly rather than silently weakening consistency.
- Benchmark and correctness reports pin product versions, topology, hardware, and configuration.
- Distributed behavior is usable from normal SQL clients without bespoke application protocols.

## v0.6 — Hybrid graph/vector/SQL

The v0.6 target is to make the hybrid model the product differentiator. AionDB should not behave like three disconnected engines bolted together; SQL filters, graph traversals, and vector ranking should share one optimizer, one storage model, and one explanation surface.

### Vector

- Distance functions: L2, cosine, inner product, Hamming.
- Index types: HNSW (existing), IVF-Flat, IVF-PQ, optional DiskANN for larger-than-memory sets.
- Filtered ANN: predicate pushdown into the index scan with a documented recall/latency tradeoff knob.
- Multi-vector columns per table.
- Quantization: scalar and product quantization, opt-in per index.

### Hybrid execution

- Query shapes that combine relational predicates, graph path predicates, vector similarity, ranking, and pagination.
- Predicate pushdown across relational storage, graph adjacency indexes, and ANN indexes.
- Re-ranking pipeline for approximate vector candidates filtered by SQL and graph constraints.
- Shared memory budget across vector search, graph traversal, joins, and sort.
- Stable result semantics when approximate search and deterministic filters are combined.

### Hybrid planner

- Single optimizer pass for queries mixing relational filters, graph traversal, and vector scoring.
- Cost model accounts for index selectivity from all three families.

### Vector benchmark target

- Public benchmarks against pgvector and selected dedicated vector engines where the workload is comparable.
- Workloads covering filtered ANN, hybrid metadata filters, multi-vector rows, graph-neighborhood-limited vector search, and exact fallback.
- Metrics: recall, latency distribution, memory use, build time, update cost, filter selectivity, and result correctness after recheck.
- Claims must distinguish pure vector search from hybrid search where AionDB's architecture is supposed to matter.

### Exit criteria

- Hybrid SQL, graph, and vector queries run against one shared schema without application-side duplication.
- Vector recall and latency reports name dataset shape, index parameters, filter selectivity, and comparison baseline.
- Graph path semantics document null endpoints, cycle handling, path bounds, and deterministic tie behavior.
- The planner can explain why it chose relational filtering before graph traversal or vector scoring, or the reverse.
- Heavy graph algorithms and large-scale vector serving are included only where the tests, benchmarks, and docs make the contract clear.

## v0.7 — Durability and recovery

The v0.7 target is to make failures uneventful. After the graph, SQL, distributed, and hybrid ambitions are in place, the storage layer must prove that crashes, corruption, backup, restore, and replay do not turn ambitious features into fragile demos.

### Crash recovery

- Failpoints in WAL append, page write, fsync, segment rotation, checkpoint, shard movement, and index build.
- After any crash, replayed state equals the last committed transaction state for supported single-node and distributed modes.
- Idempotent recovery across repeated restart cycles.
- Recovery invariants for SQL rows, graph adjacency state, vector indexes, and distributed metadata.

### Page integrity

- Checksum on every stable data page.
- Checksums verified on read; corrupted pages produce `SQLSTATE XX001` with file/offset/expected/actual.
- `aiondb doctor` walks the data directory and reports all checksum failures.
- Corruption handling distinguishes data pages, WAL segments, catalog snapshots, graph indexes, and vector indexes.

### Backup and restore

- `aiondb basebackup` produces a consistent physical snapshot online.
- WAL archive command and restore command.
- Point-in-time recovery using base backup plus WAL.
- Logical `aiondb dump` and `aiondb restore` cover schema, data, indexes, sequences, vector indexes, graph labels, and distributed metadata where supported.
- Backup validation command that proves a backup can restore before operators trust it.

### WAL and storage operations

- WAL streaming over the replication protocol.
- WAL archive integrity verification.
- Full-page writes or an explicitly documented atomic page write strategy.
- Documented fsync points: WAL flush on commit, checkpoint, segment rotation, archive publish, and backup boundary.
- Configuration knobs: `synchronous_commit`, `fsync`, `wal_sync_method`, archive mode, and restore mode.

### Exit criteria

- Crash tests cover WAL append, commit flush, page write, checkpoint publish, segment rotation, backup boundary, and index rebuild failures.
- Recovery can be repeated and compared against the same committed logical state without diverging.
- Page corruption is detected with enough path, offset, expected, and actual metadata to support operator action.
- Backup and restore are exercised from command-line workflows, not only from internal crate tests.
- PITR behavior is documented with clear stop targets, archive requirements, and unsupported edge cases.

## v0.8 — High availability

The v0.8 target is to turn the distributed engine into an operator-facing HA system. The goal is not just to have replicas; it is to survive leader loss, stale primaries, client reconnects, rolling maintenance, and replica promotion with a documented contract.

### Replication

- Physical replication with read-only replicas serving PostgreSQL traffic.
- Cascading replication where it does not weaken the recovery contract.
- Logical replication with at least one provided output plugin.
- Per-table publication and subscription.
- Replication slots with retention pressure and pruning safety.

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
- Rolling restart and rolling upgrade procedure for supported cluster shapes.
- Backup from replica and restore with primary failure plus replica promotion.

### HA benchmark target

- Failure drills covering primary loss, replica loss, network partition, slow disk, delayed WAL, stale leader, and operator restart.
- Comparative behavior notes against distributed SQL systems where the topology and consistency model are comparable.
- Metrics: failover impact, write unavailability window, read availability, data loss boundary, client reconnect behavior, and recovery progress.
- No HA claim without a runbook and a reduced failure test.

### Exit criteria

- A replica can be initialized, stream WAL, serve read-only traffic, disconnect, reconnect, and catch up.
- Replication slots prevent unsafe WAL pruning and expose retention pressure in operator-visible metrics.
- Failover has documented detection, election, fencing, and client reconnection behavior.
- Split-brain prevention is tested with network partitions, delayed messages, stale leaders, and disk stalls.
- Operators can inspect cluster state without reading internal metadata files.
- Promotion and restore flows are tested with real files and documented as operator procedures.

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

### Exit criteria

- TLS and authentication behavior are tested with real clients, expired certificates, invalid credentials, and downgrade attempts.
- Authorization rules cover object ownership, inherited roles, grants, revokes, default privileges, and row-level policies.
- Audit records are structured, append-only, documented, and tied to specific security-sensitive actions.
- Metrics, logs, and traces are sufficient to debug slow queries, WAL pressure, replication lag, authentication failures, and storage errors.
- Packaging defaults are conservative: non-root containers, explicit writable paths, documented secrets handling, and no insecure auth by default.

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

### Exit criteria

- Every public API, protocol behavior, CLI flag, config key, metric name, and log field is listed in a freeze ledger.
- The upgrade matrix is exercised from every supported pre-1.0 line to the release-candidate line.
- Breaking changes require migration notes and must be rejected unless explicitly accepted in the freeze ledger.
- Release artifacts are signed, reproducible enough to audit, and accompanied by SBOM and provenance metadata.
- Compatibility reports are regenerated from tests rather than manually edited.

## v1.0 — GA, production ready

### Release contents

- Signed release archives for Linux x86_64 and arm64, macOS arm64 (developer-only).
- Signed container images for the server and Studio.
- Signed Helm chart and operator image.
- SBOM and SLSA attestations per artifact.

### Support commitments

- v1.0 is the first long-term support line.
- LTS support stages documented with clear maintenance and security-fix rules.
- Public CVE process under `SECURITY.md` with private disclosure address, response SLA, credit policy.
- Public release policy for v1.x minors.

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

### GA bar

- The project has a public support policy, security disclosure process, and release governance model.
- Operators can install, configure, back up, restore, upgrade, monitor, and troubleshoot without crate-level knowledge.
- Users can evaluate SQL, graph, vector, replication, and HA behavior from documented examples and reproducible tests.
- Known limitations are explicit enough that production users can decide whether AionDB fits their workload.
- The release is boring to operate for the supported single-node and cluster contracts it claims.
