// A panel picture (a texture) drawn into a rectangle of a window, with rounded
// corners and a dimming factor.

struct BlitUniforms {
    rect: vec4<f32>,        // points
    radius: f32,            // points
    alpha: f32,             // 1 = as it is
    pad0: f32,
    pad1: f32,
}

@group(1) @binding(0) var<uniform> u: BlitUniforms;
@group(2) @binding(0) var picture: texture_2d<f32>;
@group(2) @binding(1) var picture_sampler: sampler;

struct BlitVary {
    @builtin(position) position: vec4<f32>,
    @location(0) uv: vec2<f32>,
    @location(1) local: vec2<f32>,      // points from the center of the rectangle
}

@vertex
fn blit_vertex(@builtin(vertex_index) vid: u32) -> BlitVary {
    let c = unit_corner(vid);
    var o: BlitVary;
    o.position = to_clip(u.rect.xy + u.rect.zw * c);
    o.uv = c;
    o.local = (c - vec2<f32>(0.5)) * u.rect.zw;
    return o;
}

@fragment
fn blit_fragment(v: BlitVary) -> @location(0) vec4<f32> {
    let color = textureSampleLevel(picture, picture_sampler, v.uv, 0.0).rgb;
    // Distance to a rounded rectangle: negative inside.
    let half = u.rect.zw * 0.5 - vec2<f32>(u.radius);
    let q = abs(v.local) - half;
    let d = length(max(q, vec2<f32>(0.0))) + min(max(q.x, q.y), 0.0) - u.radius;
    let cover = clamp(0.5 - d * g.scale, 0.0, 1.0) * u.alpha;
    return vec4<f32>(color * cover, cover);
}
