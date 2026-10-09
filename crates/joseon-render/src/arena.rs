//! The buffers of one frame, and the batch of instanced shapes.
//!
//! Port source: `FrameArena` and `ShapeBatch` in
//! `Sources/JoseonRender/RenderContext.swift`.
//!
//! On macOS the arena is three shared Metal buffers used in turn. Here the
//! frame is collected in CPU vectors and written to the GPU once, before the
//! command buffer is submitted: wgpu applies the writes in order with the
//! submits, so one set of buffers is enough. Nothing is allocated per frame.

use crate::context::{RenderContext, CURVE_WINDOW_BYTES};
use crate::gpu::{Color, CurveUniforms, Globals, ShapeInstance};
use bytemuck::Pod;

/// Where a run of `f32` values is in the arena.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FloatSlice {
    /// Byte offset in the float buffer. A multiple of the storage alignment.
    pub offset: u32,
    pub count: usize,
}

pub struct FrameArena {
    uniforms: Vec<u8>,
    floats: Vec<u8>,
    instances: Vec<u8>,
    uniform_capacity: usize,
    float_capacity: usize,
    instance_capacity: usize,
    uniform_align: usize,
    storage_align: usize,

    uniform_buffer: wgpu::Buffer,
    float_buffer: wgpu::Buffer,
    pub instance_buffer: wgpu::Buffer,
    globals_buffer: wgpu::Buffer,
    /// Group 0 of every pipeline.
    pub globals_group: wgpu::BindGroup,
    /// Group 1 of the curve pipelines. Dynamic offsets: uniforms, values, other values.
    pub curve_group: wgpu::BindGroup,
    /// Group 1 of the quad pipelines: one uniform struct, chosen with a dynamic offset.
    pub uniform_group: wgpu::BindGroup,
    /// One uniform struct and a run of values. Dynamic offsets: uniforms, values.
    pub points_group: wgpu::BindGroup,
}

impl FrameArena {
    pub const UNIFORM_BYTES: usize = 1 << 18;
    pub const FLOAT_BYTES: usize = 1 << 20;
    pub const INSTANCE_BYTES: usize = 1 << 20;
    /// The bytes of the uniform buffer one draw of a quad pipeline can see.
    pub const UNIFORM_WINDOW: u64 = 256;

    pub fn new(ctx: &RenderContext) -> Self {
        let device = &ctx.device;
        let limits = device.limits();
        let buffer = |label, size: usize, usage| {
            device.create_buffer(&wgpu::BufferDescriptor {
                label: Some(label),
                size: size as u64,
                usage: usage | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            })
        };
        let uniform_buffer = buffer(
            "Joseon arena uniforms",
            Self::UNIFORM_BYTES,
            wgpu::BufferUsages::UNIFORM,
        );
        // One window of room after the last value, so a draw at the end can bind its whole window.
        let float_buffer = buffer(
            "Joseon arena values",
            Self::FLOAT_BYTES + CURVE_WINDOW_BYTES as usize,
            wgpu::BufferUsages::STORAGE,
        );
        let instance_buffer = buffer(
            "Joseon arena instances",
            Self::INSTANCE_BYTES,
            wgpu::BufferUsages::VERTEX,
        );
        let globals_buffer = buffer(
            "Joseon globals",
            std::mem::size_of::<Globals>(),
            wgpu::BufferUsages::UNIFORM,
        );
        let globals_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("globals"),
            layout: &ctx.globals_layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: globals_buffer.as_entire_binding(),
            }],
        });
        let window = |buffer, size: u64| {
            wgpu::BindingResource::Buffer(wgpu::BufferBinding {
                buffer,
                offset: 0,
                size: std::num::NonZeroU64::new(size),
            })
        };
        let curve_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("curve"),
            layout: &ctx.curve_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: window(&uniform_buffer, std::mem::size_of::<CurveUniforms>() as u64),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: window(&float_buffer, CURVE_WINDOW_BYTES),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: window(&float_buffer, CURVE_WINDOW_BYTES),
                },
            ],
        });
        let uniform_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("uniform"),
            layout: &ctx.uniform_layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: window(&uniform_buffer, Self::UNIFORM_WINDOW),
            }],
        });
        let points_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("points"),
            layout: &ctx.points_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: window(&uniform_buffer, Self::UNIFORM_WINDOW),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: window(&float_buffer, CURVE_WINDOW_BYTES),
                },
            ],
        });
        Self {
            uniforms: Vec::with_capacity(Self::UNIFORM_BYTES),
            floats: Vec::with_capacity(Self::FLOAT_BYTES),
            instances: Vec::with_capacity(Self::INSTANCE_BYTES),
            uniform_capacity: Self::UNIFORM_BYTES,
            float_capacity: Self::FLOAT_BYTES,
            instance_capacity: Self::INSTANCE_BYTES,
            uniform_align: limits.min_uniform_buffer_offset_alignment.max(16) as usize,
            storage_align: limits.min_storage_buffer_offset_alignment.max(16) as usize,
            uniform_buffer,
            float_buffer,
            instance_buffer,
            globals_buffer,
            globals_group,
            curve_group,
            uniform_group,
            points_group,
        }
    }

    pub fn begin_frame(&mut self) {
        self.uniforms.clear();
        self.floats.clear();
        self.instances.clear();
    }

    fn reserve(staging: &mut Vec<u8>, align: usize, bytes: usize, capacity: usize) -> Option<usize> {
        let start = staging.len().div_ceil(align) * align;
        if start + bytes > capacity {
            return None; // the arena is full: the draw is skipped
        }
        staging.resize(start + bytes, 0);
        Some(start)
    }

    /// One uniform struct. Gives its dynamic offset, or `None` when the arena is full.
    pub fn push_uniform<T: Pod>(&mut self, value: &T) -> Option<u32> {
        let bytes = bytemuck::bytes_of(value);
        let start = Self::reserve(
            &mut self.uniforms,
            self.uniform_align,
            bytes.len(),
            self.uniform_capacity,
        )?;
        self.uniforms[start..start + bytes.len()].copy_from_slice(bytes);
        Some(start as u32)
    }

    /// Room for `count` values, all zero. Write them with `floats_mut`.
    pub fn allocate_floats(&mut self, count: usize) -> Option<FloatSlice> {
        let start = Self::reserve(&mut self.floats, self.storage_align, count * 4, self.float_capacity)?;
        Some(FloatSlice {
            offset: start as u32,
            count,
        })
    }

    pub fn floats_mut(&mut self, slice: FloatSlice) -> &mut [f32] {
        let start = slice.offset as usize;
        bytemuck::cast_slice_mut(&mut self.floats[start..start + slice.count * 4])
    }

    pub fn floats(&self, slice: FloatSlice) -> &[f32] {
        let start = slice.offset as usize;
        bytemuck::cast_slice(&self.floats[start..start + slice.count * 4])
    }

    pub fn push_floats(&mut self, values: &[f32]) -> Option<FloatSlice> {
        let slice = self.allocate_floats(values.len())?;
        self.floats_mut(slice).copy_from_slice(values);
        Some(slice)
    }

    /// Room for `count` instances of `T`. Gives the byte offset in the instance buffer.
    pub fn allocate_instances<T: Pod>(&mut self, count: usize) -> Option<u64> {
        let bytes = std::mem::size_of::<T>() * count;
        Self::reserve(&mut self.instances, 16, bytes, self.instance_capacity).map(|start| start as u64)
    }

    pub fn instances_mut<T: Pod>(&mut self, offset: u64, count: usize) -> &mut [T] {
        let start = offset as usize;
        bytemuck::cast_slice_mut(&mut self.instances[start..start + std::mem::size_of::<T>() * count])
    }

    pub fn push_instances<T: Pod>(&mut self, values: &[T]) -> Option<u64> {
        let offset = self.allocate_instances::<T>(values.len())?;
        self.instances_mut::<T>(offset, values.len()).copy_from_slice(values);
        Some(offset)
    }

    /// Writes the frame to the GPU. Call it before the command buffer is submitted.
    pub fn upload(&self, queue: &wgpu::Queue, globals: &Globals) {
        queue.write_buffer(&self.globals_buffer, 0, bytemuck::bytes_of(globals));
        // A write must be a multiple of 4 bytes long: every part here is.
        if !self.uniforms.is_empty() {
            queue.write_buffer(&self.uniform_buffer, 0, &self.uniforms);
        }
        if !self.floats.is_empty() {
            queue.write_buffer(&self.float_buffer, 0, &self.floats);
        }
        if !self.instances.is_empty() {
            queue.write_buffer(&self.instance_buffer, 0, &self.instances);
        }
    }
}

/// Collects instanced quads and draws them in order. Storage comes from the frame arena.
pub struct ShapeBatch {
    base: Option<u64>,
    count: usize,
    flushed: usize,
    capacity: usize,
    pub scale: f32,
}

impl ShapeBatch {
    pub const CAPACITY: usize = 4096;

    pub fn new() -> Self {
        Self {
            base: None,
            count: 0,
            flushed: 0,
            capacity: Self::CAPACITY,
            scale: 2.0,
        }
    }

    /// A batch with room for `capacity` shapes in a frame.
    pub fn with_capacity(capacity: usize) -> Self {
        Self {
            capacity,
            ..Self::new()
        }
    }

    pub fn begin_frame(&mut self, arena: &mut FrameArena, scale: f32) {
        self.scale = scale;
        self.count = 0;
        self.flushed = 0;
        self.base = arena.allocate_instances::<ShapeInstance>(self.capacity);
    }

    /// Shapes added in this frame.
    pub fn count(&self) -> usize {
        self.count
    }

    fn push(&mut self, arena: &mut FrameArena, shape: ShapeInstance) {
        let Some(base) = self.base else { return };
        if self.count >= self.capacity {
            return;
        }
        let offset = base + (self.count * std::mem::size_of::<ShapeInstance>()) as u64;
        arena.instances_mut::<ShapeInstance>(offset, 1)[0] = shape;
        self.count += 1;
    }

    /// To the nearest pixel.
    pub fn snap(&self, v: f32) -> f32 {
        (v * self.scale).round() / self.scale
    }

    /// A rectangle with a vertical gradient.
    #[allow(clippy::too_many_arguments)]
    pub fn gradient_rect(
        &mut self,
        arena: &mut FrameArena,
        rect: [f32; 4],
        top: Color,
        bottom: Color,
        radius: f32,
        glow: f32,
        stroke: f32,
    ) {
        self.push(
            arena,
            ShapeInstance {
                rect,
                color0: top,
                color1: bottom,
                params: [radius, glow, 0.0, stroke],
            },
        );
    }

    pub fn rect(&mut self, arena: &mut FrameArena, rect: [f32; 4], color: Color) {
        self.gradient_rect(arena, rect, color, color, 0.0, 0.0, 0.0);
    }

    pub fn rounded_rect(&mut self, arena: &mut FrameArena, rect: [f32; 4], color: Color, radius: f32) {
        self.gradient_rect(arena, rect, color, color, radius, 0.0, 0.0);
    }

    /// A rectangle with a horizontal gradient.
    pub fn hgradient(&mut self, arena: &mut FrameArena, rect: [f32; 4], left: Color, right: Color, radius: f32) {
        self.push(
            arena,
            ShapeInstance {
                rect,
                color0: left,
                color1: right,
                params: [radius, 0.0, 2.0, 0.0],
            },
        );
    }

    /// A horizontal line of `pixels` device pixels, on the pixel grid.
    pub fn hline(&mut self, arena: &mut FrameArena, x0: f32, x1: f32, y: f32, color: Color, pixels: f32) {
        let t = pixels / self.scale;
        let rect = [self.snap(x0), self.snap(y - t * 0.5), self.snap(x1) - self.snap(x0), t];
        self.rect(arena, rect, color);
    }

    /// A vertical line of `pixels` device pixels, on the pixel grid.
    pub fn vline(&mut self, arena: &mut FrameArena, x: f32, y0: f32, y1: f32, color: Color, pixels: f32) {
        let t = pixels / self.scale;
        let rect = [self.snap(x - t * 0.5), self.snap(y0), t, self.snap(y1) - self.snap(y0)];
        self.rect(arena, rect, color);
    }

    /// A line from `(x0, y0)` to `(x1, y1)` with a gradient along it.
    #[allow(clippy::too_many_arguments)]
    pub fn line(
        &mut self,
        arena: &mut FrameArena,
        from: [f32; 2],
        to: [f32; 2],
        width: f32,
        color: Color,
        end_color: Option<Color>,
        glow: f32,
    ) {
        self.push(
            arena,
            ShapeInstance {
                rect: [from[0], from[1], to[0], to[1]],
                color0: color,
                color1: end_color.unwrap_or(color),
                params: [width * 0.5, glow, 1.0, 0.0],
            },
        );
    }

    pub fn circle(&mut self, arena: &mut FrameArena, center: [f32; 2], r: f32, color: Color, stroke: f32, glow: f32) {
        let rect = [center[0] - r, center[1] - r, r * 2.0, r * 2.0];
        self.gradient_rect(arena, rect, color, color, r, glow, stroke);
    }

    /// Draws all the shapes added after the last flush with the given pipeline.
    /// `lut` is group 1 of the LUT pipeline.
    pub fn flush(
        &mut self,
        pass: &mut wgpu::RenderPass<'_>,
        arena: &FrameArena,
        pipeline: &wgpu::RenderPipeline,
        lut: Option<&wgpu::BindGroup>,
    ) {
        let n = self.count - self.flushed;
        let Some(base) = self.base else { return };
        if n == 0 {
            return;
        }
        let stride = std::mem::size_of::<ShapeInstance>() as u64;
        let start = base + self.flushed as u64 * stride;
        pass.set_pipeline(pipeline);
        pass.set_bind_group(0, &arena.globals_group, &[]);
        if let Some(lut) = lut {
            pass.set_bind_group(1, lut, &[]);
        }
        pass.set_vertex_buffer(0, arena.instance_buffer.slice(start..start + n as u64 * stride));
        pass.draw(0..4, 0..n as u32);
        self.flushed = self.count;
    }
}

impl Default for ShapeBatch {
    fn default() -> Self {
        Self::new()
    }
}
