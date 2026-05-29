---
title: What's New in v0.3
order: 88
---

# What's New in v0.3

v0.3 is the vector update. It adds pgvector-style SQL, HNSW, IVF-flat, Qdrant-style filtered retrieval, PostgreSQL ecosystem compatibility work, and a reproducible vector benchmark harness.

Rows, graph edges, and embeddings now sit in the same catalog. Applications no longer need a separate vector store or graph service next to the SQL database to support these workloads.

> New in v0.3: pgvector-facing SQL, two ANN index families, Qdrant-style filtered vector helpers, and benchmarked recall/latency numbers from the repository.

Start with the current benchmark snapshot in
[v0.3 Vector Performance](/documentation/evaluate/v0-3-vector-performance.html).

## Product Highlights

v0.3 delivers five major upgrades:

1. broader pgvector-style SQL for vector, halfvec, sparsevec, bit helpers, casts, functions, and ORM-generated catalog lookups;
2. HNSW search tuned for high recall on raw vectors and product-quantized paths;
3. IVF-flat indexing with fast builds, configurable probes, parallel search work, and pgvector-style DDL;
4. Qdrant-style JSON filter options for metadata-aware vector retrieval;
5. a standalone vector comparison harness that publishes build time, recall@k, and query latency.

## Vector SQL

The vector surface now speaks the SQL shape expected by PostgreSQL ecosystem tools:

- `CREATE EXTENSION IF NOT EXISTS vector`;
- `VECTOR(n)`, unconstrained `VECTOR`, `HALFVEC(n)`, and `SPARSEVEC(n)`;
- casts between arrays, vector, halfvec, sparsevec, and real arrays;
- vector arithmetic operators and distance helpers;
- binary quantization, Hamming distance, and Jaccard distance helpers;
- `pg_catalog.`-qualified distance functions for generated SQL.

This is the practical bridge for teams that already prototype RAG, recommendation, similarity, or hybrid search on the PostgreSQL stack and want those vectors to sit next to graph and relational context.

Useful references:

- [Vector Reference](/documentation/query/vector-reference.html)
- [Indexes and Constraints](/documentation/query/indexes-and-constraints.html)
- [PostgreSQL Compatibility](/documentation/connect/postgresql-compatibility.html)

## ANN Indexes

v0.3 gives AionDB two vector ANN families.

HNSW brings graph-based approximate search with:

- raw f32 search;
- product quantization search with exact rescoring;
- scalar and binary quantization paths;
- search statistics and oversampling controls;
- stronger search floors for large graphs.

IVF-flat brings clustered approximate search with:

- `USING ivfflat` DDL compatibility;
- configurable list and probe counts;
- contiguous centroid storage;
- parallel build assignment;
- parallel large-list scanning;
- per-list candidate trimming;
- L2 ordering without square roots on the IVF hot path.

The result is a real choice: HNSW for very high recall, IVF-flat for fast builds and low-latency approximate scans, and exact brute force as the ground-truth reference.

## Qdrant-Style Filters

AionDB v0.3 also moves vector search closer to real application retrieval. The vector helper functions accept Qdrant-style JSON options for filtered search, including:

- `must`, `should`, and `must_not` clauses;
- `match.value`, `match.any`, `match.except`, and `match.text`;
- numeric `range` filters;
- `values_count`;
- nested JSONB paths such as `payload.tags[]` and `payload.cities[].name`;
- `has_id`;
- `is_null` and `is_empty`;
- `with_payload` include/exclude controls;
- `with_vector` and `with_vectors`.

That gives RAG and recommendation workloads the normal retrieval controls they need: tenant filters, permissions, metadata constraints, freshness rules, and graph-derived subsets before ranking.

## Benchmark Snapshot

The v0.3 vector benchmark is built into the repository:

```bash
cd benchmarks/vector-compare
cargo run --release
```

Latest local run:

| Backend | Build ms | Recall@10 | Mean us | p50 us | p95 us | p99 us |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| AionDB HNSW raw | 36571 | 0.996 | 9423 | 8790 | 15674 | 17233 |
| AionDB HNSW PQ | 74742 | 0.994 | 13072 | 12355 | 17659 | 18471 |
| AionDB IVF-flat nprobe=8 | 418 | 0.466 | 809 | 827 | 1223 | 1766 |
| AionDB IVF-flat nprobe=32 | 416 | 0.863 | 2572 | 2474 | 3603 | 3977 |
| Brute-force exact | 0 | 1.000 | 9930 | 9520 | 14293 | 18991 |

The standout v0.3 story:

- HNSW raw reaches `0.996` recall@10.
- HNSW PQ keeps `0.994` recall@10.
- IVF-flat builds in about `416-418 ms`.
- IVF-flat with `nprobe=32` reaches `0.863` recall@10 at about `2.57 ms` mean query time.
- Exact brute force stays in the harness as the recall reference.

See:

- [v0.3 Vector Performance](/documentation/evaluate/v0-3-vector-performance.html)
- [Benchmarks](/documentation/evaluate/benchmarks.html)
- [Benchmark Reproducibility](/documentation/evaluate/benchmark-reproducibility.html)

## Ecosystem Momentum

v0.3 also advances the PostgreSQL ecosystem route. The release improves the compatibility path for:

- ORM-generated SQL;
- migration-tool introspection;
- pgvector extension metadata;
- PostgreSQL casts and function lookup;
- vector operators and helper functions;
- typed JSONB payload behavior in vector helpers.

For builders, that is the whole point of AionDB: keep SQL tooling, add graph context, add vector retrieval, and run the application against one local engine.

## Suggested Reading Order

1. [v0.3 Vector Performance](/documentation/evaluate/v0-3-vector-performance.html)
2. [Vector Reference](/documentation/query/vector-reference.html)
3. [Indexes and Constraints](/documentation/query/indexes-and-constraints.html)
4. [Benchmarks](/documentation/evaluate/benchmarks.html)
5. [Release Notes](/documentation/project/release-notes.html)
