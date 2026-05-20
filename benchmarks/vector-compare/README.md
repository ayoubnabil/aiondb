# vector-compare

Apples-to-apples ANN benchmark for AionDB's vector index families with
optional adapters for pgvector and Qdrant. Runs entirely in-process for
AionDB; the external backends stay behind feature flags so the default
build needs no network or extension setup.

## What it measures

For each backend the harness reports:

| column      | meaning                                                          |
|-------------|------------------------------------------------------------------|
| `build_ms`  | wall time of `CREATE INDEX` (or equivalent upsert+index step)    |
| `recall@k`  | mean recall@k vs exact brute-force ground truth over all queries |
| `mean_us`   | mean per-query search wall time (microseconds)                   |
| `p50_us`    | median per-query latency                                         |
| `p95_us`    | 95th percentile per-query latency                                |
| `p99_us`    | 99th percentile per-query latency                                |

The dataset is a deterministic synthetic corpus (default n=5000, d=96)
generated from a Xorshift seed so runs are reproducible. Queries reuse
the same generator with a different seed offset so they are out of the
indexed corpus.

## AionDB backends compared

- **HNSW raw** (`quantization=none`) — graph index over raw f32.
- **HNSW + PQ** (`quantization=pq`, m=`dims/8`, k=256) — graph index
  with product-quantized codes + oversample (x5) + exact rescore.
- **IVF-flat** at two `nprobe` settings — coarse k-means partitions
  with exhaustive list scans.
- **Brute-force exact** — the ground-truth baseline.

## Optional external backends

| backend | feature flag | env variable |
|---------|--------------|--------------|
| pgvector | `pgvector` | `PGVECTOR_URL` (libpq URL) |
| Qdrant   | `qdrant`   | `QDRANT_URL` (REST base URL, e.g. `http://localhost:6333`) |

Both adapters create the collection / table on each run, upsert the
same dataset, then issue the same `TOP_K` queries that the AionDB path
sees. The pgvector adapter builds an HNSW index with
`m=16, ef_construction=200`; the Qdrant adapter uses Qdrant's default
HNSW configuration with Euclidean distance.

## Running

```sh
# AionDB only
cargo run --release

# AionDB + pgvector
PGVECTOR_URL=postgres://postgres:postgres@localhost/postgres \
    cargo run --release --features pgvector

# AionDB + Qdrant
QDRANT_URL=http://localhost:6333 cargo run --release --features qdrant

# Both
PGVECTOR_URL=... QDRANT_URL=... \
    cargo run --release --features "pgvector qdrant"

# Emit machine-readable JSON in addition to the table
EMIT_JSON=1 cargo run --release
```

## Reference run (n=5000, d=96, queries=200, k=10)

All three HNSW engines use `m=16, ef_construction=100` (the Qdrant
default and now the AionDB default in this harness). pgvector uses
`hnsw.ef_search=128` at query time; AionDB queries with `ef=128`;
Qdrant uses its container defaults. Numbers were collected on a
laptop CPU in `--release` with pgvector 0.8 (pg16 container) and
Qdrant v1.x (container defaults).

| backend                                | build_ms | recall@k | mean_us | p50_us | p95_us | p99_us |
|----------------------------------------|---------:|---------:|--------:|-------:|-------:|-------:|
| **aiondb hnsw (raw)**                  |     2302 |    0.975 |     663 |    682 |    984 |   1320 |
| aiondb hnsw (pq)                       |    31361 |    0.980 |    4806 |   4266 |   8372 |  10468 |
| aiondb ivf-flat (nlist=64, nprobe=8)   |       53 |    0.383 |     103 |     85 |    196 |    310 |
| aiondb ivf-flat (nlist=64, nprobe=32)  |       52 |    0.840 |     188 |    187 |    340 |    440 |
| brute-force (exact)                    |        0 |    1.000 |     756 |    685 |   1463 |   2009 |
| **pgvector hnsw**                      |     3378 |    0.978 |    3979 |   3816 |   6391 |   7050 |
| **qdrant hnsw**                        |      205 |    1.000 |    2160 |   2303 |   3135 |   3384 |

Reading the table:

- **AionDB HNSW raw beats pgvector on both axes**: build is 1.5x
  faster (2.3s vs 3.4s) and search is **6x faster** (663µs vs 3979µs)
  at the same recall (0.975 vs 0.978). pgvector hits a network + SQL
  round-trip on every query, which dominates its mean latency at
  this scale; AionDB exercises the storage trait directly.
- **AionDB HNSW raw is ~3.2x faster than Qdrant** in search latency
  (663µs vs 2160µs) but Qdrant achieves perfect recall (1.000 vs
  0.975) and builds an order of magnitude faster (205ms vs 2302ms).
  Qdrant's C HNSW is the reference target for build and recall; for
  pure query latency at "good enough" recall, AionDB wins.
- **AionDB IVF-flat at nprobe=32** is ~21x faster than pgvector with
  similar-class recall (0.840 vs 0.978). At nprobe=8 it is **~39x
  faster than pgvector and ~21x faster than Qdrant** but recall
  drops to 0.38 — useful for prefilter pipelines that always rerank.

These numbers move with the dataset (n, d, distribution) and the
client transport. Run the harness against your own pgvector / Qdrant
deployments with realistic dataset sizes before relying on the table
above for capacity planning.
