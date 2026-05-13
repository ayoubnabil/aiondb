// Cosine distance: 1.0 - dot(a,b) / (norm(a) * norm(b))

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
    var dot_ab: f32 = 0.0;
    var norm_a: f32 = 0.0;
    var norm_b: f32 = 0.0;
    let base = i * params.dims;
    for (var d: u32 = 0u; d < params.dims; d++) {
        let a = query[d];
        let b = targets[base + d];
        dot_ab += a * b;
        norm_a += a * a;
        norm_b += b * b;
    }
    let denom = sqrt(norm_a) * sqrt(norm_b);
    if (denom < 1e-10) {
        distances[i] = 1.0;
    } else {
        distances[i] = 1.0 - dot_ab / denom;
    }
}
