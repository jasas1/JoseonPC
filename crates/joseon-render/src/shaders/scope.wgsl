// Vectorscope: additive points into a float density texture that fades, then a
// log tone-mapped composite. And the pan spectrum: one Gaussian splat per lit
// display bin into a float color texture, composited the same way.

struct ScopeUniforms {
    center: vec2<f32>,      // accumulation texture space, pixels
    radius: f32,            // pixels for |mid| = 1 (the top vertex of the diamond)
    gain: f32,
    point_size: f32,        // pixels (sigma)
    energy: f32,            // intensity per point
    tex_size: vec2<f32>,    // pixels
}

struct CompositeUniforms {
    rect: vec4<f32>,        // the field, points
    k: f32,                 // density scale inside the log
    norm: f32,              // 1 / log(1 + k * reference density)
    pad0: f32,
    pad1: f32,
}

@group(1) @binding(0) var<uniform> scope: ScopeUniforms;
@group(1) @binding(1) var<storage, read> points: array<f32>;

struct PointVary {
    @builtin(position) position: vec4<f32>,
    @location(0) local: vec2<f32>,      // pixels / sigma: x along the segment from its middle, y across
    @location(1) half_len: f32,
    @location(2) energy: f32,
}

// 45-degree goniometer: left = up-left, right = up-right, mid = up, side = horizontal.
// Contract: x = (R - L) / 2, positive = right, so x maps straight to screen x. y = (L + R) / 2 is up.
// |x| + |y| = max(|L|, |R|): full scale is the diamond |x| + |y| = 1. Nothing is clamped here.
fn scope_map(sm: vec2<f32>) -> vec2<f32> {
    return scope.center + vec2<f32>(sm.x, -sm.y) * (scope.gain * scope.radius);
}

// Light fades out between 92 % and 108 % of the full-scale diamond: loud peaks leave the field, they never pile up on the rim.
fn scope_edge(sm: vec2<f32>) -> f32 {
    let d = (abs(sm.x) + abs(sm.y)) * scope.gain;
    return 1.0 - smoothstep(0.92, 1.08, d);
}

// One instance = the beam path from sample i to sample i + 1, like the trace of a CRT scope.
@vertex
fn scope_point_vertex(@builtin(vertex_index) vid: u32, @builtin(instance_index) iid: u32) -> PointVary {
    let corner = signed_corner(vid);
    let s0 = vec2<f32>(points[iid * 2u], points[iid * 2u + 1u]);
    let s1 = vec2<f32>(points[iid * 2u + 2u], points[iid * 2u + 3u]);
    var p0 = scope_map(s0);
    let p1 = scope_map(s1);
    var d = p1 - p0;
    var len = length(d);
    // The points are evenly decimated samples, not always consecutive ones. A long jump between two points is a
    // chord across the figure, not the beam path: draw the end point as a dot. The dot keeps the low energy of the
    // long segment (travel), so the change from line to dot does not flash.
    let travel = len;
    if (len > max(scope.point_size * 24.0, scope.radius * 0.12)) {
        p0 = p1;
        d = vec2<f32>(0.0);
        len = 0.0;
    }
    var ax = vec2<f32>(1.0, 0.0);
    if (len > 1e-4) {
        ax = d / len;
    }
    let ay = vec2<f32>(-ax.y, ax.x);
    let ext = scope.point_size * 3.0;
    let local = corner * vec2<f32>(len * 0.5 + ext, ext);
    let pos = (p0 + p1) * 0.5 + ax * local.x + ay * local.y;
    var o: PointVary;
    o.position = vec4<f32>(pos.x / scope.tex_size.x * 2.0 - 1.0, 1.0 - pos.y / scope.tex_size.y * 2.0, 0.0, 1.0);
    o.local = local / scope.point_size;
    o.half_len = len * 0.5 / scope.point_size;
    // A fast beam leaves less light per pixel.
    o.energy = scope.energy / (1.0 + travel / (scope.point_size * 2.5)) * scope_edge(s1);
    return o;
}

@fragment
fn scope_point_fragment(v: PointVary) -> @location(0) vec4<f32> {
    let dx = max(abs(v.local.x) - v.half_len, 0.0);
    let d2 = dx * dx + v.local.y * v.local.y;
    let a = exp(-0.5 * d2) * v.energy;
    return vec4<f32>(a, a, a, a);
}

@vertex
fn fullscreen_vertex(@builtin(vertex_index) vid: u32) -> @builtin(position) vec4<f32> {
    let c = unit_corner(vid);
    return vec4<f32>(c.x * 2.0 - 1.0, 1.0 - c.y * 2.0, 0.0, 1.0);
}

// The pipeline multiplies the destination by the blend constant: dst = dst * fade.
@fragment
fn fade_fragment() -> @location(0) vec4<f32> {
    return vec4<f32>(0.0);
}

// MARK: Composite of the scope

@group(1) @binding(0) var<uniform> composite: CompositeUniforms;
@group(2) @binding(0) var accum: texture_2d<f32>;
@group(2) @binding(1) var accum_sampler: sampler;
@group(2) @binding(2) var lut: texture_2d<f32>;
@group(2) @binding(3) var lut_sampler: sampler;

struct QuadVary {
    @builtin(position) position: vec4<f32>,
    @location(0) uv: vec2<f32>,
}

@vertex
fn composite_vertex(@builtin(vertex_index) vid: u32) -> QuadVary {
    let c = unit_corner(vid);
    var o: QuadVary;
    o.position = to_clip(composite.rect.xy + composite.rect.zw * c);
    o.uv = c;
    return o;
}

// Density to color through log(1 + k d): the dense core compresses, thin filaments keep their place in the ramp.
@fragment
fn scope_composite_fragment(v: QuadVary) -> @location(0) vec4<f32> {
    let d = max(textureSampleLevel(accum, accum_sampler, v.uv, 0.0).r, 0.0);
    let t = clamp(log(1.0 + composite.k * d) * composite.norm, 0.0, 1.0);
    let c = textureSampleLevel(lut, lut_sampler, vec2<f32>(t, 0.5), 0.0).rgb;
    // Additive: the LUT starts at black, so empty areas add nothing.
    return vec4<f32>(c, 0.0);
}

// Keeps the hue of the accumulated light, compresses its amount with the same log curve as the scope.
@fragment
fn pan_composite_fragment(v: QuadVary) -> @location(0) vec4<f32> {
    let c = max(textureSampleLevel(accum, accum_sampler, v.uv, 0.0).rgb, vec3<f32>(0.0));
    let peak = max(max(c.r, c.g), max(c.b, 1e-6));
    let t = clamp(log(1.0 + composite.k * peak) * composite.norm, 0.0, 1.0);
    let hue = c / peak;
    // Dense cores drift toward a warm white, capped under full white.
    let rgb = mix(hue, vec3<f32>(1.0, 0.96, 0.90), smoothstep(0.72, 1.0, t) * 0.7) * min(t * 1.08, 0.94);
    return vec4<f32>(rgb, 0.0);
}
