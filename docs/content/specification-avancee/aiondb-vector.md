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
| `ScalarQuantizer` / `ScalarCode` | int8 scalar quantization. |
| `BinaryQuantizer` / `BinaryCode` | sign-bit binary quantization, packed `Vec<u64>`. |
| `ProductQuantizer` / `ProductCode` | product quantization with k-means subspace centroids. |
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
