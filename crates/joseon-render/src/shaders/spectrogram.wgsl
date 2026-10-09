// Spectrogram: ring texture, x = time column, y = log frequency row (row 0 = lowest frequency).

struct SpectrogramUniforms {
    rect: vec4<f32>,    // plot rect, points
    head: f32,          // newest column index + 1 (next write position), in columns
    columns: f32,       // texture width
    floor_v: f32,       // ring value (0...1 = -120...0 dB) that maps to the background
    gamma: f32,         // exponent of the map position
    top_v: f32,         // ring value that maps to the top of the color map
    pad0: f32,
    pad1: f32,
    pad2: f32,
}

@group(1) @binding(0) var<uniform> u: SpectrogramUniforms;
@group(2) @binding(0) var ring: texture_2d<f32>;
@group(2) @binding(1) var ring_sampler: sampler;
@group(2) @binding(2) var lut: texture_2d<f32>;
@group(2) @binding(3) var lut_sampler: sampler;

struct QuadVary {
    @builtin(position) position: vec4<f32>,
    @location(0) uv: vec2<f32>,
}

@vertex
fn rect_vertex(@builtin(vertex_index) vid: u32) -> QuadVary {
    let c = unit_corner(vid);
    var o: QuadVary;
    o.position = to_clip(u.rect.xy + u.rect.zw * c);
    o.uv = c;
    return o;
}

// dB to color map position: nothing at or under the floor, then a gamma curve up to the top.
fn heat_position(v: f32) -> f32 {
    if (v <= u.floor_v) {
        return 0.0;
    }
    return pow(clamp((v - u.floor_v) / max(u.top_v - u.floor_v, 1e-5), 0.0, 1.0), u.gamma);
}

@fragment
fn spectrogram_fragment(v: QuadVary) -> @location(0) vec4<f32> {
    // uv.x = 1 is "now". Keep half a column away from the seam between newest and oldest.
    let col = u.head - 0.5 - (1.0 - v.uv.x) * (u.columns - 1.0);
    let x = col / u.columns;
    let level = textureSampleLevel(ring, ring_sampler, vec2<f32>(x, 1.0 - v.uv.y), 0.0).r;
    let t = heat_position(level);
    let c = textureSampleLevel(lut, lut_sampler, vec2<f32>(t, 0.5), 0.0).rgb;
    return vec4<f32>(c, 1.0);
}
