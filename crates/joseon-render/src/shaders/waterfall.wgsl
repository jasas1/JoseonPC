// Waterfall, pass 2: one box per cell of the field texture, instanced, with depth; and the floor plate
// under them. The main pass then puts the resolved picture into the plot (the blit pipeline).
//
// Port source: `waterfall_vertex`, `waterfall_fragment`, `waterfall_floor_*` in
// `Sources/JoseonRender/Shaders.swift` of the macOS repo. Metal's clip z (0...1) is wgpu's too.

struct WaterfallUniforms {
    view_proj: mat4x4<f32>,
    fog: vec4<f32>,       // rgb = the panel background the far rows fade into
    floor_v: f32,         // ring value at the floor
    top_v: f32,           // ring value at the automatic top
    bins: f32,
    depth: f32,
    height: f32,          // world units of a bar at the top
    depth_step: f32,      // world units between two time rows
    hover_bin: f32,       // lit column, far negative = none
    hover_row: f32,       // lit row, negative = none
    hue_lo: f32,          // hue LUT position of the lowest frequency
    hue_hi: f32,          // ... and of the highest
    fog_start: f32,       // fraction of the depth where the fog begins
    contrast: f32,        // 1 = Increase Contrast: harder edges
}

@group(1) @binding(0) var<uniform> u: WaterfallUniforms;
@group(2) @binding(0) var field: texture_2d<f32>;
@group(2) @binding(1) var field_sampler: sampler;
@group(2) @binding(2) var hue: texture_2d<f32>;
@group(2) @binding(3) var hue_sampler: sampler;

// A box without its bottom: five faces, four corners each, counter-clockwise seen from outside.
const CORNERS = array<vec3<f32>, 20>(
    vec3<f32>(-0.5, 1.0, 0.5), vec3<f32>(0.5, 1.0, 0.5), vec3<f32>(0.5, 1.0, -0.5), vec3<f32>(-0.5, 1.0, -0.5),     // top
    vec3<f32>(-0.5, 0.0, 0.5), vec3<f32>(0.5, 0.0, 0.5), vec3<f32>(0.5, 1.0, 0.5), vec3<f32>(-0.5, 1.0, 0.5),       // front, +z
    vec3<f32>(0.5, 0.0, -0.5), vec3<f32>(-0.5, 0.0, -0.5), vec3<f32>(-0.5, 1.0, -0.5), vec3<f32>(0.5, 1.0, -0.5),   // back, -z
    vec3<f32>(0.5, 0.0, 0.5), vec3<f32>(0.5, 0.0, -0.5), vec3<f32>(0.5, 1.0, -0.5), vec3<f32>(0.5, 1.0, 0.5),       // right, +x
    vec3<f32>(-0.5, 0.0, -0.5), vec3<f32>(-0.5, 0.0, 0.5), vec3<f32>(-0.5, 1.0, 0.5), vec3<f32>(-0.5, 1.0, -0.5)    // left, -x
);
const NORMALS = array<vec3<f32>, 5>(
    vec3<f32>(0.0, 1.0, 0.0), vec3<f32>(0.0, 0.0, 1.0), vec3<f32>(0.0, 0.0, -1.0), vec3<f32>(1.0, 0.0, 0.0), vec3<f32>(-1.0, 0.0, 0.0)
);
const TRIANGLE = array<u32, 6>(0u, 1u, 2u, 0u, 2u, 3u);

struct WaterfallVary {
    @builtin(position) position: vec4<f32>,
    @location(0) color: vec3<f32>,
    @location(1) nrm: vec3<f32>,
    @location(2) fog: f32,
    @location(3) top: f32,       // 1 on the top face
}

@vertex
fn waterfall_vertex(@builtin(vertex_index) vid: u32, @builtin(instance_index) iid: u32) -> WaterfallVary {
    let bins = u32(u.bins);
    let bin = iid % bins;
    let age = iid / bins;
    let v = textureLoad(field, vec2<i32>(i32(bin), i32(age)), 0).r;
    let mag = clamp((v - u.floor_v) / max(u.top_v - u.floor_v, 1e-4), 0.0, 1.0);
    let h = max(mag * u.height, 0.05);
    let face = vid / 6u;
    let k = vid % 6u;
    var corners = CORNERS;
    var normals = NORMALS;
    var triangle = TRIANGLE;
    let c = corners[face * 4u + triangle[k]];
    let gap_x = 0.14;
    let gap_z = 0.24;
    let p = vec3<f32>(
        (f32(bin) - u.bins * 0.5 + 0.5) + c.x * (1.0 - gap_x),
        c.y * h,
        (-f32(age) + c.z * (1.0 - gap_z)) * u.depth_step
    );
    var o: WaterfallVary;
    o.position = u.view_proj * vec4<f32>(p, 1.0);
    let t = mix(u.hue_lo, u.hue_hi, (f32(bin) + 0.5) / u.bins);
    let base = textureSampleLevel(hue, hue_sampler, vec2<f32>(t, 0.5), 0.0).rgb;
    let glow = smoothstep(0.80, 1.0, mag);
    var col = base * (0.20 + 0.95 * mag) + vec3<f32>(0.30) * glow * glow;
    let lit = 1.0 - smoothstep(0.5, 2.5, abs(f32(bin) - u.hover_bin));
    var lit_row = 0.0;
    if (u.hover_row >= 0.0) {
        lit_row = 1.0 - smoothstep(0.5, 2.0, abs(f32(age) - u.hover_row));
    }
    col = col + min(lit + lit_row, 1.0) * vec3<f32>(0.26, 0.30, 0.36) * (0.5 + mag);
    o.color = col;
    o.nrm = normals[face];
    o.fog = smoothstep(u.fog_start, 1.0, f32(age) / max(u.depth - 1.0, 1.0));
    o.top = select(0.0, 1.0, face == 0u);
    return o;
}

@fragment
fn waterfall_fragment(v: WaterfallVary) -> @location(0) vec4<f32> {
    let n = normalize(v.nrm);
    let key = max(dot(n, normalize(vec3<f32>(0.35, 0.9, 0.45))), 0.0);
    let fill = max(dot(n, normalize(vec3<f32>(-0.6, 0.3, 0.7))), 0.0);
    var light = 0.38 + 0.62 * key + 0.18 * fill;
    // Increase Contrast: the sides go darker, so every bar reads as a block.
    let hard = select(light * 0.6, max(light, 1.0), v.top > 0.5);
    light = mix(light, hard, u.contrast);
    let c = mix(v.color * light, u.fog.rgb, v.fog);
    return vec4<f32>(c, 1.0);
}

// The floor under the bars: a plate a little lighter than the background, so a gap between bars is floor, not void.
struct FloorVary {
    @builtin(position) position: vec4<f32>,
    @location(0) fog: f32,
}

@vertex
fn floor_vertex(@builtin(vertex_index) vid: u32) -> FloorVary {
    let c = unit_corner(vid);
    let x = (c.x - 0.5) * (u.bins + 6.0);
    let z = mix(1.2, -(u.depth + 1.5) * u.depth_step, c.y);
    var o: FloorVary;
    o.position = u.view_proj * vec4<f32>(x, -0.02, z, 1.0);
    o.fog = smoothstep(u.fog_start, 1.0, c.y);
    return o;
}

@fragment
fn floor_fragment(v: FloorVary) -> @location(0) vec4<f32> {
    let plate = u.fog.rgb * 1.9 + vec3<f32>(0.012, 0.016, 0.03);
    return vec4<f32>(mix(plate, u.fog.rgb, v.fog), 1.0);
}
