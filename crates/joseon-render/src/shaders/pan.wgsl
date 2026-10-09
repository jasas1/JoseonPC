// Pan spectrum ("stereo placement by frequency"): one Gaussian splat per lit
// display bin. The CPU finds pan, height and light; the shader only places the
// splat. y = log frequency, x = pan, light = level, hue = the band hue of the frequency.

struct PanUniforms {
    tex_size: vec2<f32>,    // pixels
    center_x: f32,          // pan = 0, as a part of the texture width
    half_x: f32,            // pan = 1 is this far from the center, as a part of the texture width
    top: f32,               // y of the top of the frequency axis, part of the texture height
    height: f32,            // height of the frequency axis, part of the texture height
    sigma_x: f32,           // pixels
    energy: f32,
}

@group(1) @binding(0) var<uniform> u: PanUniforms;
@group(2) @binding(0) var hue: texture_2d<f32>;
@group(2) @binding(1) var hue_sampler: sampler;

struct PanVary {
    @builtin(position) position: vec4<f32>,
    @location(0) local: vec2<f32>,
    @location(1) light: vec3<f32>,
}

// instance: x = pan -1...1, y = axis position 0 (bottom) ... 1 (top), z = light 0...1, w = vertical sigma in pixels
@vertex
fn pan_point_vertex(@builtin(vertex_index) vid: u32, @location(0) d: vec4<f32>) -> PanVary {
    let corner = signed_corner(vid);
    let c = vec2<f32>(u.tex_size.x * (u.center_x + u.half_x * d.x), u.tex_size.y * (u.top + (1.0 - d.y) * u.height));
    let pos = c + corner * vec2<f32>(u.sigma_x, d.w) * 3.0;
    var o: PanVary;
    o.position = vec4<f32>(pos.x / u.tex_size.x * 2.0 - 1.0, 1.0 - pos.y / u.tex_size.y * 2.0, 0.0, 1.0);
    o.local = corner * 3.0;
    o.light = textureSampleLevel(hue, hue_sampler, vec2<f32>(d.y, 0.5), 0.0).rgb * (u.energy * d.z);
    return o;
}

@fragment
fn pan_point_fragment(v: PanVary) -> @location(0) vec4<f32> {
    let a = exp(-0.5 * dot(v.local, v.local));
    return vec4<f32>(v.light * a, a);
}
