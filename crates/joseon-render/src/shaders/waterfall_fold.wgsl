// Waterfall, pass 1: the ring folded into the small field texture (x = frequency column of the field,
// y = time row, 0 = newest): the max of the ring rows a column covers, the mean of the ring columns a
// row covers.
//
// Port source: `waterfall_sample_fragment` in `Sources/JoseonRender/Shaders.swift` of the macOS repo.

struct WaterfallSampleUniforms {
    head: f32,            // next write column of the ring
    columns: f32,         // ring width
    rows: f32,            // ring height
    bins: f32,            // field columns
    depth: f32,           // field rows
    columns_per_row: f32, // ring columns per field row
    pad0: f32,
    pad1: f32,
}

@group(1) @binding(0) var<uniform> u: WaterfallSampleUniforms;
@group(2) @binding(0) var ring: texture_2d<f32>;
@group(2) @binding(1) var ring_sampler: sampler;
@group(2) @binding(2) var unused_lut: texture_2d<f32>;
@group(2) @binding(3) var unused_sampler: sampler;

struct QuadVary {
    @builtin(position) position: vec4<f32>,
    @location(0) uv: vec2<f32>,
}

@vertex
fn fold_vertex(@builtin(vertex_index) vid: u32) -> QuadVary {
    let c = unit_corner(vid);
    var o: QuadVary;
    o.position = vec4<f32>(c.x * 2.0 - 1.0, 1.0 - c.y * 2.0, 0.0, 1.0);
    o.uv = c;
    return o;
}

@fragment
fn fold_fragment(v: QuadVary) -> @location(0) vec4<f32> {
    let cols = i32(u.columns);
    let bin = min(i32(v.uv.x * u.bins), i32(u.bins) - 1);
    let age = min(i32(v.uv.y * u.depth), i32(u.depth) - 1);
    let r0 = i32(floor(f32(bin) / u.bins * u.rows));
    var r1 = max(i32(ceil(f32(bin + 1) / u.bins * u.rows)) - 1, r0);
    r1 = min(r1, i32(u.rows) - 1);
    let n = max(i32(round(u.columns_per_row)), 1);
    let newest = i32(floor(u.head - 1.0 - f32(age) * u.columns_per_row));
    var sum = 0.0;
    for (var k = 0; k < n; k = k + 1) {
        let c = ((newest - k) % cols + cols) % cols;
        var m = 0.0;
        for (var r = r0; r <= r1; r = r + 1) {
            m = max(m, textureLoad(ring, vec2<i32>(c, r), 0).r);
        }
        sum = sum + m;
    }
    return vec4<f32>(sum / f32(n), 0.0, 0.0, 1.0);
}
