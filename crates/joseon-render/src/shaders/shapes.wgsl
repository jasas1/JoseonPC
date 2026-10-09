// Shapes: instanced quads with a rounded-box signed distance field.
// mode 0: rect (x, y, w, h), vertical gradient color0 (top) -> color1 (bottom)
// mode 1: line (x0, y0, x1, y1), params.x = half width, gradient along the line
// mode 2: rect, horizontal gradient color0 (left) -> color1 (right)
// params: x = corner radius (or line half width), y = glow sigma (0 = crisp),
//         z = mode, w = stroke width (0 = filled)

struct ShapeInstance {
    @location(0) rect: vec4<f32>,
    @location(1) color0: vec4<f32>,
    @location(2) color1: vec4<f32>,
    @location(3) params: vec4<f32>,
}

struct ShapeVary {
    @builtin(position) position: vec4<f32>,
    @location(0) local: vec2<f32>,
    @location(1) @interpolate(flat) hsize: vec2<f32>,
    @location(2) @interpolate(flat) color0: vec4<f32>,
    @location(3) @interpolate(flat) color1: vec4<f32>,
    @location(4) @interpolate(flat) params: vec4<f32>,
}

@group(1) @binding(0) var lut: texture_2d<f32>;
@group(1) @binding(1) var lut_sampler: sampler;

@vertex
fn shape_vertex(@builtin(vertex_index) vid: u32, s: ShapeInstance) -> ShapeVary {
    let corner = signed_corner(vid);
    let mode = i32(s.params.z + 0.5);
    let pad = s.params.y * 3.0 + 1.5 / g.scale;
    var center: vec2<f32>;
    var hsize: vec2<f32>;
    var ax = vec2<f32>(1.0, 0.0);
    var ay = vec2<f32>(0.0, 1.0);
    if (mode == 1) {
        let d = s.rect.zw - s.rect.xy;
        let len = max(length(d), 1e-4);
        ax = d / len;
        ay = vec2<f32>(-ax.y, ax.x);
        center = (s.rect.xy + s.rect.zw) * 0.5;
        hsize = vec2<f32>(len * 0.5, s.params.x);
    } else {
        center = s.rect.xy + s.rect.zw * 0.5;
        hsize = s.rect.zw * 0.5;
    }
    let local = corner * (hsize + pad);
    let pos = center + ax * local.x + ay * local.y;
    var o: ShapeVary;
    o.position = to_clip(pos);
    o.local = local;
    o.hsize = hsize;
    o.color0 = s.color0;
    o.color1 = s.color1;
    o.params = s.params;
    return o;
}

fn shape_alpha(v: ShapeVary) -> f32 {
    let mode = i32(v.params.z + 0.5);
    var radius = min(v.params.x, min(v.hsize.x, v.hsize.y));
    if (mode == 1) {
        radius = 0.0;
    }
    let q = abs(v.local) - v.hsize + radius;
    var d = length(max(q, vec2<f32>(0.0))) + min(max(q.x, q.y), 0.0) - radius;
    let stroke = v.params.w;
    if (stroke > 0.0) {
        d = abs(d + stroke * 0.5) - stroke * 0.5;
    }
    let soft = v.params.y;
    if (soft > 0.0) {
        let e = max(d, 0.0) / soft;
        return exp(-0.5 * e * e);
    }
    return clamp(0.5 - d * g.scale, 0.0, 1.0);
}

@fragment
fn shape_fragment(v: ShapeVary) -> @location(0) vec4<f32> {
    let mode = i32(v.params.z + 0.5);
    var t = v.local.x / max(2.0 * v.hsize.x, 1e-4) + 0.5;
    if (mode == 0) {
        t = v.local.y / max(2.0 * v.hsize.y, 1e-4) + 0.5;
    }
    let c = mix(v.color0, v.color1, clamp(t, 0.0, 1.0));
    let a = shape_alpha(v) * c.a;
    return vec4<f32>(c.rgb * a, a);
}

// Vertical color-map bar (legend). Samples the LUT bottom (0) to top (1).
@fragment
fn shape_lut_fragment(v: ShapeVary) -> @location(0) vec4<f32> {
    var t = 1.0 - clamp(v.local.y / max(2.0 * v.hsize.y, 1e-4) + 0.5, 0.0, 1.0);
    // The bar runs floor (bottom) to top on a linear dB scale: color1.x = gamma of the map.
    t = pow(t, max(v.color1.x, 0.05));
    let c = textureSampleLevel(lut, lut_sampler, vec2<f32>(t, 0.5), 0.0).rgb;
    let a = shape_alpha(v) * v.color0.a;
    return vec4<f32>(c * a, a);
}
