---
title: Benchmarks
order: 70
---

# Benchmarks

AionDB includes local benchmark harnesses under `benchmarks/`. They are intended to make performance claims reproducible and tied to a specific commit, dataset, and machine.

The benchmark docs are deliberately conservative. A fast number without the command, dataset size, durability mode, hardware, and raw output should not be treated as a product claim.

For the short product-facing read on the current v0.2 snapshots, use
[v0.2 Performance Snapshot](/documentation/evaluate/v0-2-performance.html).

## Available harnesses

```bash
benchmarks/run.sh --help
```

Current benchmark families:

- `pgbench` for OLTP microbenchmarks.
- `surreal-suite` for SurrealDB 3 article-style CRUD, scan, graph, index, full-text, and vector tests against SurrealDB WS, AionDB pgwire, and PostgreSQL with pgvector/AGE.
- `ultra-compare` for a long composite run that stitches together `neo4j-graph`, `surreal-graph`, and `surreal-suite` under one run id and one consolidated report.
- `tpch` for analytical SQL workloads.
- `tpcds` for analytical SQL workloads.
- `job` for join-heavy workloads based on the Join Order Benchmark.

The harnesses are tools, not claims. A benchmark family being present means the repository has a path to run it; it does not mean every query shape is optimized or that AionDB should be expected to win.

## Basic usage

Run AionDB and PostgreSQL side by side when you want a local reference point:

```bash
benchmarks/run.sh pgbench
```

Run only AionDB:

```bash
BENCH_ENGINES=aiondb benchmarks/run.sh pgbench
```

Run only PostgreSQL:

```bash
BENCH_ENGINES=pg benchmarks/run.sh pgbench
```

Run the SurrealDB 3 article-style comparison:

```bash
SURREAL_SUITE_ITERATIONS=1 \
SURREAL_SUITE_DURATION_SECONDS=20 \
SURREAL_SUITE_ROWS=2000 \
benchmarks/run.sh surreal-suite
```

Run the long tri-engine composite comparison:

```bash
benchmarks/run.sh ultra-compare
```

This composite harness is intentionally strict about comparability. Its report
keeps separate workload families instead of pretending that one giant number can
fairly summarize AionDB vs Neo4j vs SurrealDB across graph, CRUD, scans, and
hybrid/vector shapes.

This wrapper runs each test as warmup across all selected engines first, then one measured 20-second pass across all engines, and writes raw traces, metadata, per-iteration CSV, and summaries under `benchmarks/.state/surreal-suite/<run-id>/`.

Render the latest run as a docs page with visual bars and pgstack-relative ratios:

```bash
benchmarks/surreal-suite/render_docs.py \
  benchmarks/.state/surreal-suite/<run-id> \
  --out docs/content/documentation/evaluate/benchmark-results.md
```

The generated snapshot is visible in [Benchmark Results](/documentation/evaluate/benchmark-results.html).

## Important variables

```bash
AIONDB_PORT=15432
PG_PORT=5432
PG_DB=bench_ref
TPCH_SCALE=1
TPCDS_SCALE=1
BENCH_AUTO_CLEAN=1
SURREAL_SUITE_ENGINES="surrealdb aiondb pgstack"
SURREAL_SUITE_ITERATIONS=1
SURREAL_SUITE_DURATION_SECONDS=20
SURREAL_SUITE_WARMUP_SECONDS=3
SURREAL_SUITE_ROWS=2000
```

The heavier benchmarks may require external tools or datasets. Read the output before treating a run as comparable.

## Picking the right benchmark

| Goal | Benchmark shape |
| --- | --- |
| Connection and transaction smoke | small `pgbench` run |
| Write-path comparison | `pgbench` with disclosed WAL policy |
| SurrealDB 3 article-style comparison | `surreal-suite` with raw output retained |
| Long AionDB / Neo4j / SurrealDB comparison | `ultra-compare` with a consolidated report |
| Analytical scans | TPC-H or TPC-DS subset |
| Join optimizer pressure | Join Order Benchmark |
| Hybrid graph/vector claim | custom schema with published SQL |

For hybrid claims, standard SQL benchmarks are not enough. Publish a small workload that includes relational filters, relationship tables, and vector ranking so readers can inspect the model.

For three-engine claims, do not collapse every workload into one winner number.
Use a composite report that keeps the graph-vs-graph, graph-protocol, and
broader matrix workloads separate.

## SurrealDB Suite Comparison

The `surreal-suite` harness mirrors the public SurrealDB 3 benchmark families by name: create, read, update, scans, filters, ordering, grouping, subqueries, graph traversals, index build/remove, indexed scans, full-text, and HNSW vector search.

That choice is deliberate. Instead of inventing a benchmark mix that happens to fit AionDB especially well, the suite reuses the benchmark families SurrealDB itself publicly highlighted so the comparison is less biased toward workloads chosen by AionDB.

The protocol paths are explicit:

- SurrealDB uses WebSocket JSON-RPC.
- AionDB uses the PostgreSQL wire protocol.
- PostgreSQL stack uses PostgreSQL wire plus `pgvector` for vectors and Apache AGE for Cypher graph tests.

If `vector` or `age` is not installed in the local PostgreSQL cluster, affected PostgreSQL-stack tests are marked `UNSUPPORTED` and the raw extension error is kept in the trace.

Useful variables:

```bash
SURREAL_SUITE_ENGINES="surrealdb aiondb pgstack"
SURREAL_SUITE_ROWS=2000
SURREAL_SUITE_WARMUP_SECONDS=3
SURREAL_SUITE_ITERATIONS=1
SURREAL_SUITE_DURATION_SECONDS=20
SURREAL_SUITE_TESTS=all
SURREAL_PATH=memory
```

The run directory contains:

- `metadata.json` with commits, protocol paths, row count, durations, engines, and test names.
- `traces/*.log` with the raw error or query trace for every warmup and measured pass.
- `raw_results.csv` with every warmup and measured iteration.
- `summary.tsv` and `summary.md` with arithmetic means over the measured iterations.
- `benchmark-results.md` can be regenerated from `raw_results.csv` for a visual docs page.

## Recommended workflow

1. Build the release binary.
2. Run a small smoke benchmark to validate the environment.
3. Increase scale only after both engines complete the same workload.
4. Keep the raw output with the commit hash.
5. Change one variable at a time: clients, scale, durability, indexes, or query timeout.

Example:

```bash
cargo build --release -p aiondb-server --bin aiondb

PGBENCH_SCALE=1 \
PGBENCH_CLIENTS=1 \
PGBENCH_DURATION=10 \
benchmarks/run.sh pgbench
```

## Correctness before timing

For every benchmark, define a correctness check:

- row counts after load;
- sample query output;
- checksum-style aggregate if useful;
- expected error behavior for unsupported statements;
- same dataset loaded into each compared engine.

Only compare latency after the result is known to be correct.

## Interpreting results

Do not compare benchmark results unless both engines are using comparable durability, data volume, hardware, and query timeout settings. AionDB defaults in the harness are chosen to exercise the real server path, but alpha performance can change quickly.

The public benchmark rule for v0.1 is simple: publish commands, dataset size, commit hash, machine details, and raw output with any performance claim.

## Comparison examples

A useful result summary looks like:

```text
commit: <sha>
binary: target/release/aiondb
benchmark: pgbench
scale: 1
clients: 1
duration: 10s
durability: AIONDB_STORAGE_DURABLE_WAL_COMMIT_POLICY=always
machine: CPU / RAM / disk / OS
raw output: attached
```

An unusable result summary looks like:

```text
AionDB is faster on my machine.
```

The second form cannot be reproduced, debugged, or believed.

See [Benchmark Reproducibility](/documentation/evaluate/benchmark-reproducibility.html) and [Performance Tuning](/documentation/evaluate/performance-tuning.html) before changing durability, resource limits, or index definitions.
