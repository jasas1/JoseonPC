// Curves: values are uniformly spaced in x across `rect`, 0 (bottom) ... 1 (top).
// The vertex shader expands the polyline. No CPU tessellation.

struct CurveUniforms {
    rect: vec4<f32>,        // x, y, w, h in points
    color: vec4<f32>,
    half_width: f32,        // points
    soft: f32,              // glow sigma in points, 0 = crisp line
    count: f32,             // number of values
    mode: f32,              // 0 = uniform color, 1 = hue LUT by x
    dash_period: f32,       // points along x, 0 = solid
    dash_duty: f32,
    whiten: f32,            // mix toward white (LUT mode)
    fill_top: f32,          // fill alpha at the curve
    fill_bottom: f32,       // fill alpha at the base
    pad0: f32,              // lines: segments to search on each side. fills: reference height at the left end
    fade_right: f32,        // points: alpha falls to 0 over this distance at the right end of the plot (0 = off)
    pad2: f32,              // fills: reference height at the right end
}

@group(1) @binding(0) var<uniform> u: CurveUniforms;
@group(1) @binding(1) var<storage, read> values: array<f32>;
@group(1) @binding(2) var<storage, read> other: array<f32>;
@group(2) @binding(0) var lut: texture_2d<f32>;
@group(2) @binding(1) var lut_sampler: sampler;

fn edge_fade(x: f32) -> f32 {
    if (u.fade_right <= 0.0) {
        return 1.0;
    }
    return smoothstep(0.0, u.fade_right, u.rect.x + u.rect.z - x);
}

struct CurveVary {
    @builtin(position) position: vec4<f32>,
    @location(0) dist: f32,     // lines: y of the pixel, points
    @location(1) u: f32,        // 0...1 across the plot
    @location(2) value: f32,    // lines: the index along the curve
    @location(3) h: f32,        // lines: x of the pixel. fills: height in the plot 0 (bottom) ... 1 (top)
}

fn curve_x(i: i32) -> f32 {
    return u.rect.x + u.rect.z * (f32(i) / max(u.count - 1.0, 1.0));
}

fn curve_point(i: i32) -> vec2<f32> {
    return vec2<f32>(curve_x(i), u.rect.y + u.rect.w * (1.0 - values[i]));
}

fn other_point(i: i32) -> vec2<f32> {
    return vec2<f32>(curve_x(i), u.rect.y + u.rect.w * (1.0 - other[i]));
}

// Lines are drawn as a distance field: the strip only bounds the pixels near the
// curve, the fragment shader measures the true distance to the nearest segments.
// Width and glow stay even on sharp spikes.
@vertex
fn curve_line_vertex(@builtin(vertex_index) vid: u32) -> CurveVary {
    let n = i32(u.count);
    let i = i32(vid / 2u);
    let top = (vid & 1u) == 0u;
    let k = i32(u.pad0) + 1;
    var vmax = values[i];
    var vmin = values[i];
    for (var j = max(i - k, 0); j <= min(i + k, n - 1); j++) {
        vmax = max(vmax, values[j]);
        vmin = min(vmin, values[j]);
    }
    let reach = u.half_width + u.soft * 3.0 + 1.5 / g.scale;
    let x = curve_x(i);
    var y = u.rect.y + u.rect.w * (1.0 - vmin) + reach;
    if (top) {
        y = u.rect.y + u.rect.w * (1.0 - vmax) - reach;
    }
    var o: CurveVary;
    o.position = to_clip(vec2<f32>(x, y));
    o.dist = y;
    o.u = f32(i) / max(u.count - 1.0, 1.0);
    o.value = f32(i);
    o.h = x;
    return o;
}

@fragment
fn curve_line_fragment(v: CurveVary) -> @location(0) vec4<f32> {
    let n = i32(u.count);
    let k = i32(u.pad0);
    let i0 = clamp(i32(floor(v.value)), 0, n - 2);
    let p = vec2<f32>(v.h, v.dist);
    var best = 1e9;
    let lo = max(i0 - k, 0);
    let hi = min(i0 + k, n - 2);
    var a = curve_point(lo);
    for (var j = lo; j <= hi; j++) {
        let b = curve_point(j + 1);
        let ab = b - a;
        let ap = p - a;
        let t = clamp(dot(ap, ab) / max(dot(ab, ab), 1e-8), 0.0, 1.0);
        let d2 = ap - ab * t;
        best = min(best, dot(d2, d2));
        a = b;
    }
    let d = sqrt(best) - u.half_width;
    var alpha: f32;
    if (u.soft > 0.0) {
        let e = max(d, 0.0) / u.soft;
        alpha = exp(-0.5 * e * e);
    } else {
        alpha = clamp(0.5 - d * g.scale, 0.0, 1.0);
    }
    if (u.dash_period > 0.0) {
        let ph = fract(v.u * u.rect.z / u.dash_period);
        let edge = 0.75 / (u.dash_period * g.scale);
        alpha *= smoothstep(0.0, edge, ph) * (1.0 - smoothstep(u.dash_duty, u.dash_duty + edge, ph));
    }
    var c = u.color;
    if (u.mode > 0.5) {
        let hue = textureSampleLevel(lut, lut_sampler, vec2<f32>(v.u, 0.5), 0.0).rgb;
        c = vec4<f32>(mix(hue, vec3<f32>(1.0), u.whiten), c.a);
    }
    alpha *= c.a * edge_fade(p.x);
    return vec4<f32>(c.rgb * alpha, alpha);
}

// The fill is one polygon (the strip from the curve to the floor) with ONE
// gradient: alpha and brightness depend on the height of the pixel in the plot
// and on a straight reference line across x (`pad0` at the left end, `pad2` at
// the right end, both as plot heights 0...1). Nothing here reads the level of a
// column, so a narrow peak cannot draw a stripe down through the fill under it.
// Hue follows x through the LUT.
@vertex
fn curve_fill_vertex(@builtin(vertex_index) vid: u32) -> CurveVary {
    let i = i32(vid / 2u);
    let top = (vid & 1u) == 0u;
    let p = curve_point(i);
    var pos = vec2<f32>(p.x, u.rect.y + u.rect.w);
    var h = 0.0;
    if (top) {
        pos = p;
        h = values[i];   // linear in y inside every triangle: the height of the pixel in the plot
    }
    var o: CurveVary;
    o.position = to_clip(pos);
    o.dist = 0.0;
    o.u = f32(i) / max(u.count - 1.0, 1.0);
    o.value = 0.0;
    o.h = h;
    return o;
}

@fragment
fn curve_fill_fragment(v: CurveVary) -> @location(0) vec4<f32> {
    var hue = u.color.rgb;
    if (u.mode > 0.5) {
        hue = textureSampleLevel(lut, lut_sampler, vec2<f32>(v.u, 0.5), 0.0).rgb;
    }
    let reference = max(mix(u.pad0, u.pad2, v.u), 0.05);
    let rel = clamp(v.h / reference, 0.0, 1.0);   // 0 at the floor, 1 at the reference line and over it
    let shade = rel * rel;
    let a = mix(u.fill_bottom, u.fill_top, shade) * u.color.a * edge_fade(u.rect.x + u.rect.z * v.u);
    let c = mix(hue * 0.80, mix(hue, vec3<f32>(1.0), 0.16), shade);
    return vec4<f32>(c * a, a);
}

// Area between two curves (`values` and `other`): the response-vs-target error
// of the headphone overlay.
@vertex
fn curve_band_vertex(@builtin(vertex_index) vid: u32) -> CurveVary {
    let i = i32(vid / 2u);
    let top = (vid & 1u) == 0u;
    var p = other_point(i);
    if (top) {
        p = curve_point(i);
    }
    var o: CurveVary;
    o.position = to_clip(p);
    o.dist = 0.0;
    o.u = f32(i) / max(u.count - 1.0, 1.0);
    o.value = values[i] - other[i];
    o.h = 0.0;
    return o;
}

@fragment
fn curve_band_fragment(v: CurveVary) -> @location(0) vec4<f32> {
    let a = u.color.a * edge_fade(u.rect.x + u.rect.z * v.u);
    return vec4<f32>(u.color.rgb * a, a);
}
