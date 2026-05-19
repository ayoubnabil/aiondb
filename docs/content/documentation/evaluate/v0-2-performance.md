---
title: v0.2 Performance Snapshot
order: 72
---

# v0.2 Performance Snapshot

This page is the short, product-facing performance snapshot for v0.2.

It is intentionally narrower than a full benchmark dashboard:

- it uses named local benchmark artifacts from this repository;
- it keeps graph-vs-Neo4j separate from the broader SurrealDB/pgstack matrix;
- it does **not** collapse every workload into one winner number.

If you want the full benchmark machinery, use:

- [Benchmarks](/documentation/evaluate/benchmarks.html)
- [Benchmark Results](/documentation/evaluate/benchmark-results.html)
- [Benchmark Reproducibility](/documentation/evaluate/benchmark-reproducibility.html)

## Snapshots used here

This page is based on two concrete local benchmark snapshots:

1. **Graph vs Neo4j**
   - harness: `benchmarks/neo4j-graph-compare/run.py`
   - run id: `20260519-150230`
   - dataset: `rows=5000`, `degree=4`, `edges=20000`
   - warmup: `2`
   - iterations: `6`
   - artifact: `target/benchmarks/neo4j-graph-compare/20260519-150230/report.json`

2. **Broader matrix vs SurrealDB and pgstack**
   - harness: `benchmarks/run.sh surreal-suite`
   - run id: `full-all-20260512T192959Z`
   - engines: `aiondb`, `surrealdb`, `pgstack`
   - artifacts:
     - `benchmarks/.state/surreal-suite/full-all-20260512T192959Z/summary.tsv`
     - [Benchmark Results](/documentation/evaluate/benchmark-results.html)

The repository also contains `ultra-compare`, but this page is based on the two
snapshots above because they are the current inspectable artifacts with actual
results.

## Graph vs Neo4j

The current v0.2 graph snapshot is strong on core traversals and graph scans.

All representative queries below kept **result parity = true**.

| Query | AionDB p50 | Neo4j p50 | Relative result |
| --- | ---: | ---: | --- |
| `out_depth1` | `0.546 ms` | `9.595 ms` | AionDB faster |
| `out_depth2` | `0.512 ms` | `7.457 ms` | AionDB faster |
| `out_depth3` | `0.407 ms` | `4.698 ms` | AionDB faster |
| `in_depth1` | `0.653 ms` | `4.476 ms` | AionDB faster |
| `edge_filter` | `12.810 ms` | `30.640 ms` | AionDB faster |
| `multi_out_where` | `15.845 ms` | `34.657 ms` | AionDB faster |
| `variable_len_4` | `0.549 ms` | `6.024 ms` | AionDB faster |
| `shortest_path` | `0.927 ms` | `3.022 ms` | AionDB faster |

### Read this correctly

This is not a claim that AionDB beats Neo4j on every graph workload.

It is a precise claim:

- on this pinned local graph workload,
- with this dataset shape,
- with this exact harness,
- AionDB is ahead on the measured traversal and shortest-path shapes in this snapshot.

## Broader matrix vs SurrealDB and pgstack

The broader matrix is mixed, which is exactly why this page keeps it separate.

Representative results from `surreal-suite`:

| Workload | AionDB | SurrealDB | pgstack | Read |
| --- | ---: | ---: | ---: | --- |
| `[C]reate` | `109.15 ops/s` | `1553.5 ops/s` | `580.85 ops/s` | AionDB behind |
| `[R]ead` | `1420.15 ops/s` | `1684.35 ops/s` | `3299.3 ops/s` | AionDB behind |
| `[S]can::count_all (2000)` | `7819.5 ops/s` | `168.85 ops/s` | `1073.5 ops/s` | AionDB far ahead |
| `[S]can::graph_edge_filter (2000)` | `1056.55 ops/s` | `219.0 ops/s` | `446.55 ops/s` | AionDB ahead |
| `[S]can::graph_bidirectional (2000)` | `2874.1 ops/s` | `1322.1 ops/s` | `8.66 ops/s` | AionDB ahead |
| `[S]can::graph_multi_count (2000)` | `2597.4 ops/s` | `22.82 ops/s` | `532.8 ops/s` | AionDB ahead |
| `[S]can::graph_multi_out (2000)` | `328.9 ops/s` | `341.9 ops/s` | `UNSUPPORTED` | near tie, slight SurrealDB lead |
| `[Complex]::graph_two_hop_filter_aggregate (5000)` | `35.20 ops/s` | `UNSUPPORTED` | `0.35 ops/s` | AionDB ahead where supported |
| `[Complex]::vector_join_graph_filter_rank (5000)` | `61.15 ops/s` | `UNSUPPORTED` | `418.05 ops/s` | pgstack ahead |

### What this means

The broader v0.2 picture is:

- **graph scans and graph-shaped filters** are already a strong area for AionDB;
- **simple CRUD throughput** is not yet where the best specialized competitors are;
- **hybrid graph/vector** is real and runnable, but not yet a universal win;
- some comparison cells are still `UNSUPPORTED` on one side or another, so fairness depends on the exact workload family.

That is why v0.2 should be described as:

> strong on a growing set of graph and hybrid graph/query shapes, but still mixed as a general benchmark matrix.

Not:

> faster than Neo4j, PostgreSQL, and SurrealDB overall.

## Current v0.2 performance read

If you want the short version:

- against **Neo4j** on the current pinned graph snapshot, AionDB looks strong;
- against the broader **SurrealDB / pgstack** matrix, AionDB is strong on several graph-heavy scans but clearly not the winner on every workload family;
- v0.2 has a credible performance story for graph evaluation, not a universal cross-engine win story.

## Reproduce the snapshots

Current Neo4j graph snapshot:

```bash
python3 benchmarks/neo4j-graph-compare/run.py \
  --rows 5000 \
  --degree 4 \
  --warmup 2 \
  --iterations 6
```

Broader SurrealDB / pgstack matrix:

```bash
SURREAL_SUITE_ROWS=2000 \
SURREAL_SUITE_WARMUP_SECONDS=3 \
SURREAL_SUITE_ITERATIONS=1 \
SURREAL_SUITE_DURATION_SECONDS=20 \
benchmarks/run.sh surreal-suite
```

For claim discipline, keep:

- the exact command;
- the run id;
- the commit hash;
- the hardware;
- the raw output artifacts.
