---
title: aiondb-gpu
order: 64
---

# aiondb-gpu

GPU-accelerated batch distance computation for HNSW index construction. Defines a `BatchDistanceComputer` trait with a CPU implementation that is always compiled in and an optional Vulkan implementation behind the `vulkan` Cargo feature, built on `wgpu` and WGSL shaders. The Vulkan backend is enabled by compiling with `--features vulkan` and setting `AIONDB_GPU_ENABLED=true` at runtime; otherwise the CPU backend is used.

## cargo

```toml
[dependencies]
aiondb-gpu = { path = "../aiondb-gpu" }

# optional GPU backend
# aiondb-gpu = { path = "../aiondb-gpu", features = ["vulkan"] }
```

## modules

| module | purpose |
|---|---|
| `cpu` | `CpuBatchDistance`: cache-friendly CPU implementation, always available. |
| `vulkan` | `GpuBatchDistance`: Vulkan compute via `wgpu` and WGSL. Compiled only with `--features vulkan`. |

The `shaders/` directory holds the WGSL kernels used by the Vulkan backend.

## key types

| type | role |
|---|---|
| `DistanceMetric` | enum: `L2`, `Cosine`, `InnerProduct`, `Manhattan`. |
| `BatchDistanceComputer` | trait: `compute_distances(query, targets_flat, dims, metric)` returning `Vec<f32>`, plus `backend_name()`. |
| `CpuBatchDistance` | always-available CPU implementation. |
| `GpuBatchDistance` | Vulkan-backed implementation, only when `vulkan` is enabled. |
| `create_distance_computer(gpu_enabled)` | factory that returns the GPU backend when the feature is on, the runtime flag is true, and initialisation succeeds; otherwise returns `CpuBatchDistance`. |

The GPU backend falls back to CPU on any initialisation error (logged via `tracing::warn`).

## example

```rust
use aiondb_gpu::{create_distance_computer, BatchDistanceComputer, DistanceMetric};

let computer = create_distance_computer(false);
let query = vec![1.0_f32, 0.0, 0.0];
let targets = vec![
    1.0_f32, 0.0, 0.0,
    0.0, 1.0, 0.0,
    0.0, 0.0, 1.0,
];
let distances = computer
    .compute_distances(&query, &targets, 3, DistanceMetric::L2)
    .expect("matching dims");
assert_eq!(distances.len(), 3);
println!("backend: {}", computer.backend_name());
```

The targets buffer is laid out contiguously: `[t0_d0, t0_d1, ..., t0_dN-1, t1_d0, ...]`. The function returns `N` distances where `N = targets_flat.len() / dims`.
