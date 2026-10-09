// Text overlay: the text bitmap lives in a texture with the pixel size of the
// target. Only the rectangles that hold text are drawn.

@group(1) @binding(0) var text: texture_2d<f32>;

@vertex
fn overlay_vertex(@builtin(vertex_index) vid: u32, @location(0) r: vec4<f32>) -> @builtin(position) vec4<f32> {
    return to_clip(r.xy + r.zw * unit_corner(vid));
}

@fragment
fn overlay_fragment(@builtin(position) position: vec4<f32>) -> @location(0) vec4<f32> {
    let limit = textureDimensions(text) - vec2<u32>(1u, 1u);
    let p = min(vec2<u32>(position.xy), limit);
    return textureLoad(text, p, 0);   // premultiplied
}
