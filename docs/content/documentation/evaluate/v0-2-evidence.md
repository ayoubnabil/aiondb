---
title: v0.2 Evidence
order: 71
---

# v0.2 Evidence

v0.2 is the credibility milestone. The release should prove that AionDB is a real PostgreSQL-wire database foundation, not only a demo path, while also making the graph surface credible against Neo4j-class expectations. This page lists the evidence that should exist before broad claims are made.

The goal is not to claim production completeness. The goal is to make storage, WAL, driver behavior, type behavior, graph behavior, and baseline evaluation inspectable from a fresh checkout.

## Evidence matrix

| Area | Evidence | Source |
| --- | --- | --- |
| Storage format | Frozen page layout, manifest version, doctor behavior, upgrade path. | [Storage Format](/documentation/learn/storage-format.html), [Storage Compatibility](/documentation/manage/storage-compatibility.html) |
| WAL contract | Segment header, LSN modes, frozen record tags, idempotent replay. | [WAL Contract](/documentation/learn/wal-contract.html) |
| Type system | PG-facing names, OIDs, null behavior, text/binary expectations. | [Data Types](/documentation/query/data-types.html) |
| Driver behavior | Connect, simple query, prepared query, transactions, error recovery. | [Client Drivers](/documentation/connect/client-drivers.html), [PostgreSQL Compatibility](/documentation/connect/postgresql-compatibility.html) |
| Catalog surface | Catalog rows needed by drivers and introspection tools. | [System Catalogs](/documentation/query/system-catalogs.html) |
| Graph maturity | Neo4j-class modeling, algorithms, benchmark targets, migration guidance, and operations evidence. | [Graph and Vector](/documentation/query/graph-and-vector.html), [Graph Reference](/documentation/query/graph-reference.html) |
| Baseline performance | Startup, simple writes, point reads, WAL append, recovery. | [Benchmarks](/documentation/evaluate/benchmarks.html), [Benchmark Reproducibility](/documentation/evaluate/benchmark-reproducibility.html) |
| Limitations | Unsupported behavior and alpha boundaries. | [Limitations](/documentation/evaluate/limitations.html) |

## Required local checks

Run the contract checks before treating v0.2 as credible:

```bash
cargo test -p aiondb-buffer-pool frozen_layout_v1
cargo test -p aiondb-storage-engine storage_compat
cargo test -p aiondb-wal frozen_wal
cargo test -p aiondb-wal reader_replay_is_idempotent
```

Run the compatibility matrix when the required client tools are installed:

```bash
cargo xtask ecosystem-compat --list
cargo xtask ecosystem-compat --report tmp/clean_prod/ecosystem_compat.json
```

Run the graph correctness checks before making graph maturity claims:

```bash
cargo test -p aiondb-graph-api -p aiondb-graph-projection -p aiondb-graph
cargo test -p aiondb-storage-engine adjacency
cargo test -p aiondb-executor cypher_graph
cargo test -p aiondb-engine graph
```

Run a small baseline benchmark only after correctness checks pass:

```bash
cargo build --release -p aiondb-server --bin aiondb

BENCH_ENGINES=aiondb \
PGBENCH_SCALE=1 \
PGBENCH_CLIENTS=1 \
PGBENCH_DURATION=10 \
benchmarks/run.sh pgbench
```

The benchmark output is not a claim unless it includes the command, commit hash, durability settings, hardware, raw output, and correctness checks.

## Storage and WAL acceptance

v0.2 should not move forward unless:

- every stable file kind has documented magic bytes or a documented manifest entry;
- unknown future storage majors are refused;
- v0.1 data directories have a tested upgrade path;
- the WAL tag table is dense, frozen, and asserted by tests;
- replay from the same start LSN is deterministic across reopens;
- corrupt WAL tails stop recovery cleanly instead of producing undefined state.

## Driver and type acceptance

Driver evidence should name exact versions and protocol paths:

| Driver or tool | Required v0.2 path |
| --- | --- |
| `psql` / `libpq` | connect, simple query, prepared statement, transaction rollback, SQLSTATE smoke |
| `psycopg` | parameter binding, rollback semantics, error class propagation |
| `SQLAlchemy` | reflection, bound parameters, simple CRUD |
| `Django` | migrations, introspection, constraints, rollback |
| `node-postgres` | prepared parameters, rollback semantics, introspection |
| `Prisma` | schema introspection against a live AionDB schema |
| `Diesel` | `PgConnection`, bind parameters, rollback, introspection, error class |

Type evidence should include:

- PG name and OID reported through catalog and row descriptions;
- text format round-trip;
- binary format status: supported, rejected, or not requested by the driver;
- null round-trip;
- cast behavior and overflow error class;
- unsupported types returning `SQLSTATE 0A000` or another documented error class.

## Graph acceptance

v0.2 should treat graph as a serious product surface, not an isolated demo. The goal is Neo4j-class maturity as an engineering target, with evidence for what is already comparable and what is still behind.

Required graph evidence:

- stable node, edge, label, property, path, and projection semantics;
- documented behavior for duplicate edges, null endpoints, self loops, multi-label entities, and path uniqueness;
- SQL/PGQ and Cypher-compatible subsets named precisely;
- algorithm correctness fixtures for traversal, shortest path, connected components, centrality, and community-style workloads where implemented;
- resource controls for heavy algorithms: memory budget, timeout, result limit, and abort behavior;
- migration notes for Neo4j label-property graphs, including unsupported Cypher features;
- graph-specific metrics for traversal depth, frontier size, index hit rate, memory use, timeout aborts, and result truncation.

Required Neo4j-class benchmark fields:

| Field | Required |
| --- | --- |
| AionDB commit | yes |
| Neo4j version | yes |
| Dataset shape | yes |
| Query text | yes |
| Algorithm or traversal mode | yes |
| Index definitions | yes |
| Hardware and OS | yes |
| Result correctness check | yes |
| Raw output path | yes |

Allowed graph claim:

```text
On this pinned workload, AionDB is faster/comparable/slower than Neo4j, with these query shapes and this raw output.
```

Disallowed graph claim:

```text
AionDB is mature as Neo4j.
```

The second form is too broad unless every relevant surface is backed by benchmark, compatibility, migration, and operations evidence.

## Baseline benchmark acceptance

The v0.2 benchmark baseline is meant to expose bottlenecks for later milestones. It should not claim that AionDB is faster than PostgreSQL, Neo4j, or distributed SQL systems.

Required baseline shapes:

- startup and first connection;
- simple insert path;
- point read by primary key;
- range scan over an indexed column;
- WAL append and flush path;
- checkpoint or snapshot publication where implemented;
- restart recovery after committed writes.

Required report fields:

| Field | Required |
| --- | --- |
| AionDB commit | yes |
| Build command | yes |
| Benchmark command | yes |
| Dataset size | yes |
| Durability settings | yes |
| Protocol path | yes |
| Hardware and OS | yes |
| Correctness check | yes |
| Raw output path | yes |

## Claim language

Use precise v0.2 language:

- "AionDB speaks PostgreSQL wire protocol."
- "This driver path has been tested against these operations."
- "This storage/WAL behavior is part of the v0.2 contract."
- "This graph workload has been compared with a pinned Neo4j version and raw output."
- "This benchmark is a baseline for a named commit and configuration."

Avoid broad language:

- "PostgreSQL compatible" without a compatibility matrix.
- "Faster than PostgreSQL" without a named benchmark and raw output.
- "Production ready" before the v1.0 support, security, backup, HA, and operations bars are met.
- "Supports Neo4j/CockroachDB-style workloads" before the later milestone evidence exists.

## Exit checklist

Before tagging v0.2, the release should have:

- storage and WAL docs aligned with code;
- storage doctor and upgrade tests passing;
- WAL frozen tag tests passing;
- graph semantics, algorithm, benchmark, migration, and operations evidence recorded for supported Neo4j-class claims;
- at least one public driver matrix report;
- type mapping page reviewed against catalog output;
- benchmark baseline commands and raw output retained;
- limitations page updated for unsupported PostgreSQL, storage, graph, vector, and operations behavior;
- release notes that separate implemented behavior from roadmap intent.
