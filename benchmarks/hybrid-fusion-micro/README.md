# AionDB Hybrid Fusion Microbench

This benchmark isolates hot paths used by hybrid and vector query fusion:

- full sort vs partial top-K selection for fused scores
- full sort vs partial top-K selection for vector distances
- clone-all JSON hit parsing vs borrowed hit windows
- cloned JSON option parsing vs borrowed `jsonb` option parsing
- cloned JSON range array elements vs direct borrowed range checks
- per-candidate text filter normalization vs pre-lowered filter text with ASCII no-allocation matching
- count-all `min_should` evaluation vs threshold short-circuiting
- old RRF shape with cloned payloads, `BTreeMap`, and full sort vs borrowed payloads, `HashMap`, and partial top-K
- old DBSF shape with normalized score vectors, cloned payloads, `BTreeMap`, and full sort vs a reusable normalizer, borrowed payloads, `HashMap`, and partial top-K

Run it from the repository root:

```sh
cargo xtask hybrid-fusion-microbench
```

or:

```sh
make bench-hybrid-fusion-micro
```

Run it directly from this directory:

```sh
cargo run --release --quiet
```

Useful knobs:

```sh
FUSION_CANDIDATES=500000 FUSION_K=100 FUSION_ITERS=10 JSON_CANDIDATES=100000 JSON_ITERS=5 cargo run --release --quiet
```
