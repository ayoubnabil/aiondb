---
title: aiondb-vector
order: 40
---

# aiondb-vector

Vector type support for the engine. Centralises distance functions, index descriptors, quantization codecs, and the planner-side backend registry used by similarity search. The runtime `VectorValue` itself lives in `aiondb-core` and is re-exported here.

## cargo

```toml
[dependencies]
aiondb-vector = { path = "../aiondb-vector" }
```

## modules

| module | purpose |
|---|---|
| `distance` | distance functions and the `VectorDistance` enum. |
| `index` | `VectorIndexDescriptor` and `VectorIndexAlgorithmParams`. |
| `planner` | `build_vector_search_plan` entry points. |
| `planner_backends` | backend trait, registry, and built-in `hnsw` / `ivf_flat` backends. |
| `quantization` | scalar, binary, and product quantization codecs. |
| `simd` | architecture dispatch for distance kernels (`x86`, `arm`, scalar fallback). |
| `types` | thin type helpers used by the planner. |

## key types

| item | description |
|---|---|
| `VectorValue` | re-export of the core vector value (element type plus dims). |
| `VectorDistance` | `L2`, `Cosine`, `InnerProduct`, `Manhattan`. |
| `VectorIndexDescriptor` | catalog index id, algorithm, params, distance metric. |
| `VectorIndexAlgorithmParams` | `Hnsw(HnswParams)` or `Custom(BTreeMap<String, String>)`. |
| `VectorSearchAlgorithm`, `VectorSearchSpec`, `VectorSearchPlan`, `VectorDistanceMetric` | re-exports from `aiondb-plan`. |
| `VectorSearchBackend`, `VectorSearchBackendRegistry` | extension point for new ANN algorithms. |
| `QuantizationKind` | `None`, `Scalar`, `Binary`, `Product`. |
| `VectorQuantizer` trait | `encode`, `decode`, `approx_l2`. |
| `ScalarQuantizer` / `ScalarCode` | int8 scalar quantization. Wired into HNSW: trained at index build, used in the layer-0 hot loop, exact rescored against the retained raw vector. |
| `BinaryQuantizer` / `BinaryCode` | sign-bit binary quantization, packed `Vec<u64>`. Wired into HNSW with raw-vector dropped, so the in-index rescore path does not run. |
| `ProductQuantizer` / `ProductCode` | product quantization with k-means subspace centroids. Wired into HNSW with subspace count picked by the storage engine (`dims / 8` when divisible, else `dims / 4`, else `dims / 2`) and `k = 256` centroids per subspace. |
| `build_vector_search_plan`, `build_vector_search_plan_with_registry` | planner entry points. |
| `default_vector_search_backend_registry` | global registry pre-loaded with built-in backends. |

## distance functions

```rust
use aiondb_vector::distance::{
    cosine_distance, inner_product, l2_distance, manhattan_distance,
};

let a = [1.0_f32, 0.0, 0.0];
let b = [0.0_f32, 1.0, 0.0];

let _l2 = l2_distance(&a, &b);
let _cos = cosine_distance(&a, &b);
let _ip = inner_product(&a, &b);
let _l1 = manhattan_distance(&a, &b);
```

## quantization runtime

Scalar (`sq`) and Product (`pq`) quantization codecs are now trained inside the
HNSW storage engine and used end-to-end:

- The initial build / `from_rows_with_options` extracts every sample, trains the
  selected codec, and encodes the per-node code (`ScalarCode` or `ProductCode`).
- Layer-0 traversal compares the (already-encoded) query against stored codes
  via `VectorQuantizer::approx_l2`.
- The candidate set is oversampled by a factor of 3 (`sq`) or 5 (`pq`) so the
  rescoring pass has enough latitude to recover recall.
- After traversal the engine reads the retained raw f32 vector from each node
  and recomputes the exact metric distance, then sorts and truncates to the
  caller's `k`.

Binary quantization (`bq`) drops the raw vector entirely, so the in-index
rescore path does not apply. The current recall floor on a 64-dim synthetic
dataset is locked in by
`recall_at_k_with_binary_quantization_meets_threshold` and will be revisited
once heap-fetched rescoring is wired through the executor.

Live inserts arriving before the codebook has been trained (for example, an
empty `CREATE INDEX` followed by individual inserts) fall back to raw f32
storage. Once the index accumulates 256 nodes the storage engine triggers a
lazy training pass from the retained raw vectors, back-fills codes for every
existing node, and switches subsequent inserts onto the quantized path - no
explicit `REINDEX` required.

## example

```rust
use aiondb_core::IndexId;
use aiondb_vector::{VectorDistance, VectorIndexDescriptor};

let descriptor = VectorIndexDescriptor::hnsw(
    IndexId::new(7),
    16,
    200,
    VectorDistance::Cosine,
);

assert!(matches!(descriptor.distance_metric, VectorDistance::Cosine));
```
