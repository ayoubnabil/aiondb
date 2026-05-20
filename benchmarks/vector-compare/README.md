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

Collected on a laptop CPU in `--release` against pgvector 0.8 (pg16
container, `hnsw.ef_search=128`) and Qdrant v1.x (container defaults).

| backend                                | build_ms | recall@k | mean_us | p50_us | p95_us | p99_us |
|----------------------------------------|---------:|---------:|--------:|-------:|-------:|-------:|
| **aiondb hnsw (raw)**                  |     3642 |    0.976 |     602 |    586 |   1034 |   1338 |
| aiondb hnsw (pq)                       |    40777 |    0.981 |    3644 |   3321 |   6960 |   8668 |
| aiondb ivf-flat (nlist=64, nprobe=8)   |       42 |    0.383 |      45 |     41 |     70 |    136 |
| aiondb ivf-flat (nlist=64, nprobe=32)  |       54 |    0.840 |     200 |    178 |    417 |    568 |
| brute-force (exact)                    |        0 |    1.000 |     758 |    614 |   1604 |   2465 |
| **pgvector hnsw**                      |     3569 |    0.982 |    3910 |   3333 |   7178 |   8519 |
| **qdrant hnsw**                        |      381 |    1.000 |    1947 |   1478 |   3712 |   4587 |

Reading the table:

- AionDB HNSW raw is **~6.5x faster than pgvector HNSW** at equivalent
  recall (0.976 vs 0.982). pgvector hits a network + SQL round-trip
  on every query, which dominates its mean latency at this scale.
- AionDB HNSW raw is **~3.2x faster than Qdrant** but Qdrant achieves
  perfect recall (1.000 vs 0.976). For workloads that need every last
  point of recall, Qdrant's HNSW tuning is the reference target.
- AionDB IVF-flat at `nprobe=32` is **~19x faster than pgvector** with
  comparable recall (0.840 vs 0.982). At `nprobe=8` it is **~45x
  faster than pgvector** but recall drops to 0.38 — useful for prefilter
  pipelines that always rerank.

These numbers move with the dataset (n, d, distribution) and the
client transport. Run the harness against your own pgvector / Qdrant
deployments with realistic dataset sizes before relying on the table
above for capacity planning.
