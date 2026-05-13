// L2 (Euclidean) distance: sqrt(sum((a[i] - b[i])^2))
// query: single vector of `dims` f32 values
// targets: `count` vectors of `dims` f32 values, flattened
// distances: output array of `count` f32 distances

struct Params {
    dims: u32,
    count: u32,
}

@group(0) @binding(0) var<storage, read> query: array<f32>;
@group(0) @binding(1) var<storage, read> targets: array<f32>;
@group(0) @binding(2) var<storage, read_write> distances: array<f32>;
@group(0) @binding(3) var<uniform> params: Params;

@compute @workgroup_size(64)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let i = gid.x;
    if (i >= params.count) {
        return;
    }
    var sum: f32 = 0.0;
    let base = i * params.dims;
    for (var d: u32 = 0u; d < params.dims; d++) {
        let diff = query[d] - targets[base + d];
        sum += diff * diff;
    }
    distances[i] = sqrt(sum);
}
