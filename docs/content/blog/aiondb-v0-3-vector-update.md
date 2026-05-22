---
title: AionDB v0.3 turns vector search into a first-class engine feature
seo_title: AionDB v0.3 vector update - HNSW, IVF-flat, pgvector SQL, and Qdrant-style filters
description: AionDB v0.3 adds a stronger vector stack with pgvector-compatible SQL, HNSW, IVF-flat, Qdrant-style filtered helpers, and reproducible vector benchmarks.
date: 2026-05-22
tags: vector database, pgvector, Qdrant, HNSW, IVF-flat, RAG database
order: 2
---

# AionDB v0.3 turns vector search into a first-class engine feature

v0.3 is where AionDB's vector system becomes a product surface.

The release brings pgvector-style SQL, HNSW, IVF-flat, Qdrant-style filtered helper options, PostgreSQL ecosystem compatibility work, and a reproducible benchmark harness into the same engine. The goal is simple: one database for relational records, graph relationships, and semantic retrieval.

For RAG, recommendation, knowledge-base, support, and agent memory workloads, the interesting query is rarely just "nearest vectors." It is "nearest vectors inside this tenant, for this permission scope, related to this graph neighborhood, with these metadata constraints." AionDB v0.3 moves directly toward that shape.

## The v0.3 Vector Stack

The pgvector-facing SQL surface now covers more of the syntax and helper behavior that application tooling emits:

- `CREATE EXTENSION IF NOT EXISTS vector`;
- `VECTOR(n)`, unconstrained `VECTOR`, `HALFVEC(n)`, and `SPARSEVEC(n)`;
- casts between arrays, vector, halfvec, sparsevec, and real arrays;
- vector arithmetic and distance helpers;
- binary quantization, Hamming distance, and Jaccard distance helpers;
- `pg_catalog.`-qualified distance functions for ORM-generated SQL.

The index surface now has two ANN families:

- HNSW for high-recall graph-based vector search;
- IVF-flat for fast builds and low-latency approximate scans.

The helper layer now accepts Qdrant-style JSON options: `must`, `should`, `must_not`, match clauses, numeric ranges, nested JSONB paths, id filters, null checks, empty checks, payload controls, and vector return controls.

That is the product point of v0.3. AionDB goes beyond a distance function by bringing vector retrieval into the same place where the application already keeps tables, metadata, permissions, and relationships.

## Benchmark Snapshot

The vector benchmark is now a repository-level workflow:

```bash
cd benchmarks/vector-compare
cargo run --release
```

Default run:

| Setting | Value |
| --- | ---: |
| Vectors | 50000 |
| Dimensions | 96 |
| Queries | 200 |
| k | 10 |

Latest local output:

| Backend | Build ms | Recall@10 | Mean us | p50 us | p95 us | p99 us |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| AionDB HNSW raw | 36571 | 0.996 | 9423 | 8790 | 15674 | 17233 |
| AionDB HNSW PQ | 74742 | 0.994 | 13072 | 12355 | 17659 | 18471 |
| AionDB IVF-flat nprobe=8 | 418 | 0.466 | 809 | 827 | 1223 | 1766 |
| AionDB IVF-flat nprobe=32 | 416 | 0.863 | 2572 | 2474 | 3603 | 3977 |
| Brute-force exact | 0 | 1.000 | 9930 | 9520 | 14293 | 18991 |

The headline numbers:

- HNSW raw reaches `0.996` recall@10.
- HNSW PQ reaches `0.994` recall@10.
- IVF-flat builds in about `416-418 ms`.
- IVF-flat with `nprobe=32` reaches `0.863` recall@10 at about `2.57 ms` mean query latency.

## Why It Matters

The normal vector database pattern is to split data: relational state in PostgreSQL, relationships somewhere else, embeddings in a vector service, and glue code between them. AionDB's model is different. The table stays the source of truth, graph labels add connected context, and vector indexes rank semantic matches beside the same metadata.

That is why v0.3 matters. It gives the product a stronger vector core without giving up the PostgreSQL tooling path that application teams already understand.

Read next:

- [What's New in v0.3](/documentation/project/whats-new-v0-3.html)
- [v0.3 Vector Performance](/documentation/evaluate/v0-3-vector-performance.html)
- [Vector Reference](/documentation/query/vector-reference.html)
