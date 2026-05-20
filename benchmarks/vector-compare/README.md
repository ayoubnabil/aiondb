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

| backend                        | build_ms | recall@k | mean_us | p50_us | p95_us | p99_us |
|--------------------------------|---------:|---------:|--------:|-------:|-------:|-------:|
| aiondb hnsw (raw)              |     3163 |    0.976 |     596 |    505 |   1233 |   1747 |
| aiondb hnsw (pq)               |    35070 |    0.981 |    3248 |   2906 |   5609 |   6584 |
| aiondb ivf-flat (nlist=64, nprobe=8) |    45 |    0.383 |      74 |     70 |    124 |    201 |
| aiondb ivf-flat (nlist=64, nprobe=32) |   38 |    0.840 |     167 |    122 |    396 |    700 |
| brute-force (exact)            |        0 |    1.000 |     640 |    633 |   1056 |   1302 |

These numbers were collected on a laptop CPU in `--release`; they
illustrate the recall/latency tradeoff between the index families
and the impact of `nprobe` on IVF-flat. Treat them as reference
points, not as authoritative cross-vendor numbers — for those, point
the harness at production-sized pgvector / Qdrant deployments with the
appropriate feature flags enabled.
