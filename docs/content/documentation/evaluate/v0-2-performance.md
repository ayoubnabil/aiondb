---
title: v0.2 Performance Snapshot
order: 72
---

# v0.2 Performance Snapshot

A short performance snapshot for v0.2. Narrower than a full benchmark dashboard: named local artifacts, graph-vs-Neo4j kept separate from the SurrealDB/pgstack matrix, no single winner number across workloads.

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

2. **CRUD refresh vs SurrealDB and pgstack**
   - harness: `benchmarks/run.sh surreal-suite`
   - run id: `20260519T135910Z`
   - engines: `aiondb`, `surrealdb`, `pgstack`
   - storage: all three engines on durable local storage
   - tests: `[C]reate`, `[R]ead`, `[U]pdate`
   - artifacts:
     - `benchmarks/.state/surreal-suite/20260519T135910Z/summary.tsv`
     - `benchmarks/.state/surreal-suite/20260519T135910Z/metadata.json`

3. **Broader matrix vs SurrealDB and pgstack**
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

The v0.2 graph snapshot favors AionDB on core traversals and graph scans.

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

This is not a claim that AionDB beats Neo4j on every graph workload. It is a narrow claim: on this pinned local graph workload, with this dataset, with this harness, AionDB is ahead on the measured traversal and shortest-path shapes.

## Broader matrix vs SurrealDB and pgstack

The broader matrix is mixed, which is why this page keeps it separate.

Fresh CRUD snapshot from `surreal-suite`:

| Workload | AionDB | SurrealDB | pgstack | Read |
| --- | ---: | ---: | ---: | --- |
| `[C]reate` | `203.8 ops/s` | `197.0 ops/s` | `486.4 ops/s` | near tie with SurrealDB; pgstack ahead |
| `[R]ead` | `1409.1 ops/s` | `1840.3 ops/s` | `4545.6 ops/s` | AionDB behind both, but same order of magnitude as SurrealDB |
| `[U]pdate` | `556.1 ops/s` | `520.1 ops/s` | `1128.1 ops/s` | near tie with SurrealDB; pgstack ahead |

Representative broader graph/hybrid results from the full `surreal-suite` snapshot:

| Workload | AionDB | SurrealDB | pgstack | Read |
| --- | ---: | ---: | ---: | --- |
| `[S]can::count_all (2000)` | `7819.5 ops/s` | `168.85 ops/s` | `1073.5 ops/s` | AionDB far ahead |
| `[S]can::graph_edge_filter (2000)` | `1056.55 ops/s` | `219.0 ops/s` | `446.55 ops/s` | AionDB ahead |
| `[S]can::graph_bidirectional (2000)` | `2874.1 ops/s` | `1322.1 ops/s` | `8.66 ops/s` | AionDB ahead |
| `[S]can::graph_multi_count (2000)` | `2597.4 ops/s` | `22.82 ops/s` | `532.8 ops/s` | AionDB ahead |
| `[S]can::graph_multi_out (2000)` | `328.9 ops/s` | `341.9 ops/s` | `UNSUPPORTED` | near tie, slight SurrealDB lead |
| `[Complex]::graph_two_hop_filter_aggregate (5000)` | `35.20 ops/s` | `UNSUPPORTED` | `0.35 ops/s` | AionDB ahead where supported |
| `[Complex]::vector_join_graph_filter_rank (5000)` | `61.15 ops/s` | `UNSUPPORTED` | `418.05 ops/s` | pgstack ahead |

### What this means

The broader v0.2 picture:

- graph scans and graph-shaped filters favor AionDB;
- CRUD throughput is competitive with SurrealDB on this all-durable refresh; pgstack remains ahead;
- hybrid graph/vector is real and runnable, but not a universal win;
- some comparison cells are still `UNSUPPORTED` on one side. Fairness depends on the exact workload family.

Describe v0.2 as:

> strong on a growing set of graph and hybrid shapes, mixed as a general benchmark matrix.

Not:

> faster than Neo4j, PostgreSQL, and SurrealDB overall.

## Short read

- against Neo4j on the pinned graph snapshot, AionDB is ahead;
- against the broader SurrealDB / pgstack matrix, AionDB wins several graph-heavy scans and loses other workload families;
- v0.2 is a graph-evaluation performance story, not a universal cross-engine win.

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
