//! The structs the shaders read. The layout matches `shaders/*.wgsl`.
//!
//! Port source: the "GPU structs" part of `Sources/JoseonRender/RenderContext.swift`.

use bytemuck::{Pod, Zeroable};

/// RGBA, each 0...1, not premultiplied.
pub type Color = [f32; 4];

#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Pod, Zeroable)]
pub struct Globals {
    /// Points.
    pub view_size: [f32; 2],
    /// Pixels per point.
    pub scale: f32,
    pub time: f32,
}

#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Pod, Zeroable)]
pub struct ShapeInstance {
    pub rect: [f32; 4],
    pub color0: Color,
    pub color1: Color,
    pub params: [f32; 4],
}

#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Pod, Zeroable)]
pub struct CurveUniforms {
    pub rect: [f32; 4],
    pub color: Color,
    pub half_width: f32,
    pub soft: f32,
    pub count: f32,
    pub mode: f32,
    pub dash_period: f32,
    pub dash_duty: f32,
    pub whiten: f32,
    pub fill_top: f32,
    pub fill_bottom: f32,
    pub pad0: f32,
    /// Points over which the curve fades out at the right end of the plot. 0 = no fade.
    pub fade_right: f32,
    pub pad2: f32,
}

impl Default for CurveUniforms {
    fn default() -> Self {
        Self {
            rect: [0.0; 4],
            color: [1.0; 4],
            half_width: 0.5,
            soft: 0.0,
            count: 0.0,
            mode: 0.0,
            dash_period: 0.0,
            dash_duty: 0.5,
            whiten: 0.0,
            fill_top: 0.6,
            fill_bottom: 0.1,
            pad0: 0.0,
            fade_right: 0.0,
            pad2: 0.0,
        }
    }
}
