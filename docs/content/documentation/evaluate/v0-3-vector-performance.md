---
title: v0.3 Vector Performance
order: 72
---

# v0.3 Vector Performance

v0.3 ships a vector benchmark harness. It measures HNSW raw, HNSW with product quantization, IVF-flat at different probe counts, and brute-force exact search as the recall reference.

Run it from the repository:

```bash
cd benchmarks/vector-compare
cargo run --release
```

Default dataset:

| Setting | Value |
| --- | ---: |
| Vectors | 50000 |
| Dimensions | 96 |
| Queries | 200 |
| k | 10 |

## Current Snapshot

Latest local run:

| Backend | Build ms | Recall@10 | Mean us | p50 us | p95 us | p99 us |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| AionDB HNSW raw | 36571 | 0.996 | 9423 | 8790 | 15674 | 17233 |
| AionDB HNSW PQ | 74742 | 0.994 | 13072 | 12355 | 17659 | 18471 |
| AionDB IVF-flat nprobe=8 | 418 | 0.466 | 809 | 827 | 1223 | 1766 |
| AionDB IVF-flat nprobe=32 | 416 | 0.863 | 2572 | 2474 | 3603 | 3977 |
| Brute-force exact | 0 | 1.000 | 9930 | 9520 | 14293 | 18991 |

## What The Numbers Show

HNSW raw is the high-recall path: `0.996` recall@10 on the default run.

HNSW PQ keeps recall high at `0.994` recall@10 while exercising the product-quantized search path and exact rescoring.

IVF-flat is the fast-build path: about `416-418 ms` build time on this dataset. With `nprobe=32`, it reaches `0.863` recall@10 at about `2.57 ms` mean query latency.

Brute-force exact remains in the benchmark as the ground truth. That keeps every approximate result tied to a measurable recall target instead of a standalone latency number.

## v0.3 surface

Vector performance is a normal evaluation surface in v0.3. SQL users create vector indexes through pgvector-style DDL. RAG workloads combine nearest-neighbor search with Qdrant-style metadata filters. Application data stays in one catalog with relational, graph, and vector access paths. Benchmark output includes build time, recall, and latency in one table.

The default run is self-contained. Provision external pgvector and Qdrant targets separately for service-to-service comparisons.

## Raw Output

```text
vector-compare  (n=50000  d=96  queries=200  k=10)

backend                        build_ms   recall@k    mean_us     p50_us     p95_us     p99_us
----------------------------------------------------------------------------------------------
aiondb hnsw (raw)                 36571      0.996       9423       8790      15674      17233
aiondb hnsw (pq)                  74742      0.994      13072      12355      17659      18471
aiondb ivf-flat (nlist=64,nprobe=8)        418      0.466        809        827       1223       1766
aiondb ivf-flat (nlist=64,nprobe=32)        416      0.863       2572       2474       3603       3977
brute-force (exact)                   0      1.000       9930       9520      14293      18991
```

Related pages:

- [What's New in v0.3](/documentation/project/whats-new-v0-3.html)
- [Vector Reference](/documentation/query/vector-reference.html)
- [Benchmarks](/documentation/evaluate/benchmarks.html)
