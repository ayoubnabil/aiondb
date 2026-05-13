---
title: Benchmark Reproducibility
order: 72
---

# Benchmark Reproducibility

Benchmark numbers without context are not useful. This page defines the minimum information needed for an AionDB benchmark claim.

Use this page as the checklist before publishing a performance comparison. It should be possible for another person to rerun the same command on a similar machine and understand why their number differs.

## Required metadata

Record:

- AionDB commit hash.
- Build command.
- Benchmark command.
- Dataset size.
- Hardware: CPU, memory, disk.
- OS and kernel.
- Filesystem and encryption status.
- Durability settings.
- Resource limits.
- Number of clients and threads.
- Raw output.

Keep metadata next to the result. A separate chat message or memory of the machine is not enough when someone tries to reproduce the run later.

## Example pgbench run

```bash
cargo build --release -p aiondb-server --bin aiondb

PGBENCH_SCALE=1 \
PGBENCH_CLIENTS=1 \
PGBENCH_DURATION=10 \
benchmarks/run.sh pgbench
```

The pgbench harness writes a tab-separated report under the benchmark state directory.

## Suggested report template

```text
Title:
AionDB commit:
Comparison engine and version:
Build command:
Benchmark command:
Dataset scale:
Clients:
Threads:
Duration:
Durability:
Limits:
Hardware:
OS/kernel:
Filesystem:
Protocol path:
Raw output:
Notes:
```

Use the same template for positive and negative results. Slow results are often more useful than fast results because they show where the engine needs work.

## Engine selection

Run both engines:

```bash
BENCH_ENGINES="aiondb pg" benchmarks/run.sh pgbench
```

Run only AionDB:

```bash
BENCH_ENGINES=aiondb benchmarks/run.sh pgbench
```

Run the SurrealDB 3 article-style comparison:

```bash
SURREAL_SUITE_ENGINES="surrealdb aiondb pgstack" \
SURREAL_SUITE_ITERATIONS=1 \
SURREAL_SUITE_DURATION_SECONDS=20 \
SURREAL_SUITE_ROWS=2000 \
benchmarks/run.sh surreal-suite
```

Keep the entire `benchmarks/.state/surreal-suite/<run-id>/` directory with the result. It contains per-engine warmup logs, measured iteration logs, run metadata, raw CSV, and summaries.

## Durability disclosure

Always disclose:

```bash
AIONDB_STORAGE_DURABLE_WAL_COMMIT_POLICY=always
AIONDB_PERSIST_PAGED_STATE_ON_COMMIT=0
```

`AIONDB_STORAGE_DURABLE_WAL_COMMIT_POLICY=always` keeps the WAL commit path on the safest benchmark setting. `AIONDB_PERSIST_PAGED_STATE_ON_COMMIT=0` means the benchmark does not force a paged-state refresh after every commit; recovery relies on WAL replay and later snapshot/checkpoint publication. If either setting changes, say so in the result. Relaxed durability or extra synchronous persistence can change write throughput dramatically.

## Protocol disclosure

Disclose how the query reached the engine:

- PostgreSQL wire through `psql`;
- PostgreSQL wire through a driver;
- embedded Rust API;
- benchmark harness wrapper;
- another protocol path.

Protocol overhead can dominate tiny queries. A fair comparison should measure comparable paths or clearly explain why the paths differ.

For `surreal-suite`, disclose that SurrealDB uses WebSocket JSON-RPC, AionDB uses PostgreSQL wire, and PostgreSQL stack uses PostgreSQL wire with `pgvector` and Apache AGE when those extensions are installed.

## Comparison rule

When comparing AionDB with another database, disclose the driver and protocol path. For example, PostgreSQL wire over a local TCP connection is not the same measurement as an in-process embedded API. HTTP, WebSocket, and native drivers can also change latency enough to dominate small queries.

For join-heavy tests, publish the full query text and schema. Optimizer behavior depends on indexes, constraints, table sizes, and data distribution; the query name alone is not enough.

## Result rule

A benchmark result should include the command and raw output. A summary without reproduction details should be treated as anecdotal.

## Regression tracking

For performance regressions, include before and after:

```text
good commit:
bad commit:
same benchmark command:
raw output at good commit:
raw output at bad commit:
configuration diff:
```

Without a known-good and known-bad pair, a regression report becomes guesswork.
