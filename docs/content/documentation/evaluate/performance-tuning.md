---
title: Performance Tuning
order: 72
---

# Performance Tuning

AionDB v0.1 performance work should start from reproducibility and correctness. Tune after the query returns the right answer.

Do not tune by changing many variables at once. The order should be: correctness, release build, stable dataset, baseline query, one configuration change, then measurement.

## Build mode

Use release builds for performance measurements:

```bash
cargo build --release -p aiondb-server --bin aiondb
```

Debug builds are useful for development but not meaningful for benchmark claims.

Also record the binary path. Accidentally benchmarking an old binary is a common source of misleading results.

## Durability settings

Durability changes write latency. Record the WAL policy with every benchmark:

```bash
AIONDB_STORAGE_DURABLE_WAL_COMMIT_POLICY=always
AIONDB_STORAGE_DURABLE_WAL_COMMIT_POLICY=every:10
AIONDB_STORAGE_DURABLE_WAL_COMMIT_POLICY=never
AIONDB_PERSIST_PAGED_STATE_ON_COMMIT=0
```

Use `always` for durability-sensitive comparisons.

For write-heavy tests, durability mode can dominate every other result. Do not compare `always` against relaxed settings unless the point of the benchmark is to show that exact tradeoff. Also disclose `AIONDB_PERSIST_PAGED_STATE_ON_COMMIT`: disabling per-commit paged-state persistence leaves WAL durability active, but it changes when the paged snapshot is refreshed.

## Resource limits

Development defaults are intentionally conservative. For controlled workloads:

```bash
AIONDB_LIMITS_STATEMENT_TIMEOUT_MS=0
AIONDB_LIMITS_MAX_RESULT_ROWS=2000000
AIONDB_LIMITS_MAX_RESULT_BYTES=67108864
AIONDB_LIMITS_MAX_MEMORY_BYTES=536870912
AIONDB_LIMITS_MAX_TEMP_BYTES=1073741824
AIONDB_ENGINE_POOL_WORKER_THREADS=8
```

Do not hide these settings. They materially affect results.

If a query hits a limit, decide whether the limit is protecting the server from an unreasonable query or whether the benchmark needs a larger budget. Raising limits is valid only when disclosed.

## Indexes

Create indexes for selective filters, joins, and vector top-k queries:

```sql
CREATE INDEX docs_kind_idx ON docs (kind);
CREATE INDEX docs_embedding_hnsw ON docs USING hnsw (embedding);
```

After adding indexes, inspect plans and verify output.

Indexing checklist:

- join keys used by frequent joins;
- graph edge endpoint columns;
- selective filter columns;
- vector columns used by top-k queries;
- composite indexes for common multi-column predicates.

Avoid adding every possible index. Extra indexes can slow writes and confuse performance analysis.

## Query diagnosis

When a query is slow, classify it:

| Symptom | First check |
| --- | --- |
| Slow first run only | build mode, cold cache, dataset load |
| Slow joins | indexes, join order, cardinality |
| Slow graph traversal | endpoint indexes, edge count, direction |
| Slow vector search | brute-force baseline, HNSW index, filter selectivity |
| Timeout | statement timeout and query shape |
| Huge result | missing `LIMIT` or missing predicate |

Then reduce the query until the bottleneck remains visible.

## Benchmark hygiene

- Use a fixed commit hash.
- Use a fixed dataset.
- Warm up when appropriate.
- Keep raw output.
- Record CPU, memory, disk, filesystem, kernel, and storage settings.
- Compare correctness before comparing latency.

## Reporting regressions

A useful performance regression report includes:

- previous commit and current commit;
- benchmark command;
- schema and data generator;
- raw output before and after;
- configuration differences;
- whether results are still correct.

Without a before/after command pair, a performance report is only anecdotal.
