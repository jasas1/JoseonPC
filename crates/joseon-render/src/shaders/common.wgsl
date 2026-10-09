// Shared by every shader of the panels.
//
// Port source: `Sources/JoseonRender/Shaders.swift` in the macOS repo (Metal
// Shading Language). All geometry is in view points with a top-left origin.
// `Globals.scale` turns points into pixels for anti-aliasing. Blending is
// premultiplied alpha ("over") or pure additive.

struct Globals {
    view_size: vec2<f32>,   // points
    scale: f32,             // pixels per point
    time: f32,
}

@group(0) @binding(0) var<uniform> g: Globals;

fn to_clip(p: vec2<f32>) -> vec4<f32> {
    return vec4<f32>(p.x / g.view_size.x * 2.0 - 1.0, 1.0 - p.y / g.view_size.y * 2.0, 0.0, 1.0);
}

// The corner of a quad drawn as a 4-vertex triangle strip: -1 or +1 on each axis.
fn signed_corner(vid: u32) -> vec2<f32> {
    return vec2<f32>(select(-1.0, 1.0, (vid & 1u) != 0u), select(-1.0, 1.0, (vid & 2u) != 0u));
}

// The same corner as 0 or 1 on each axis.
fn unit_corner(vid: u32) -> vec2<f32> {
    return vec2<f32>(select(0.0, 1.0, (vid & 1u) != 0u), select(0.0, 1.0, (vid & 2u) != 0u));
}
