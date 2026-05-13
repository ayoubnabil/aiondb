// Manhattan (L1) distance: sum(|a[i] - b[i]|)

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
        sum += abs(query[d] - targets[base + d]);
    }
    distances[i] = sum;
}
