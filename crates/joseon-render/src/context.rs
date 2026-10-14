//! Shared GPU state: device, queue, the shader modules and every pipeline.
//! Made once per process. `None` when there is no GPU adapter.
//!
//! Port source: `Sources/JoseonRender/RenderContext.swift` in the macOS repo.
//! Metal became wgpu: Direct3D 12 on Windows (Vulkan as the fallback), Metal on
//! a Mac. What Metal sets per draw with `setVertexBytes` is here a slice of an
//! arena buffer, chosen with a dynamic offset (see `arena.rs`).

use crate::gpu::{CurveUniforms, Globals, ShapeInstance};
use std::num::NonZeroU64;
use std::sync::{Arc, OnceLock};

/// The color target of every panel. Not sRGB: the shaders write display values, as on macOS.
pub const TARGET_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Bgra8Unorm;

/// The density of the goniometer, and the colored light of the pan spectrum.
pub const SCOPE_ACCUM_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::R16Float;
pub const PAN_ACCUM_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba16Float;

/// The folded field of the waterfall: one value per cell.
pub const FIELD_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::R16Float;
pub const WATERFALL_DEPTH_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Depth32Float;
pub const WATERFALL_SAMPLES: u32 = 4;

/// The bytes of curve values one draw can see: 16 384 values.
pub const CURVE_WINDOW_BYTES: u64 = 16_384 * 4;

pub struct RenderContext {
    /// For the surface of a window: it must come from the instance and the adapter of the device.
    pub instance: wgpu::Instance,
    pub adapter: wgpu::Adapter,
    pub device: wgpu::Device,
    pub queue: wgpu::Queue,
    /// The name of the adapter and its backend, for the probe and the logs.
    pub adapter_info: String,
    /// True for a software rasterizer (WARP, llvmpipe): a build server. Timing budgets do not hold there.
    pub software: bool,

    pub shape_over: wgpu::RenderPipeline,
    pub shape_add: wgpu::RenderPipeline,
    pub shape_lut: wgpu::RenderPipeline,
    pub curve_line_over: wgpu::RenderPipeline,
    pub curve_line_add: wgpu::RenderPipeline,
    pub curve_fill: wgpu::RenderPipeline,
    pub curve_band: wgpu::RenderPipeline,
    pub overlay: wgpu::RenderPipeline,
    pub spectrogram: wgpu::RenderPipeline,
    pub scope_fade: wgpu::RenderPipeline,
    pub scope_points: wgpu::RenderPipeline,
    pub scope_composite: wgpu::RenderPipeline,
    pub pan_fade: wgpu::RenderPipeline,
    pub pan_points: wgpu::RenderPipeline,
    pub pan_composite: wgpu::RenderPipeline,
    /// A panel picture into a rectangle of a window. Group 1: `uniform_layout`, group 2: `lut_layout`.
    pub blit: wgpu::RenderPipeline,
    /// The waterfall: the ring folded into the field, the bars with depth, the floor under them.
    pub waterfall_fold: wgpu::RenderPipeline,
    pub waterfall_bars: wgpu::RenderPipeline,
    pub waterfall_floor: wgpu::RenderPipeline,

    pub globals_layout: wgpu::BindGroupLayout,
    pub curve_layout: wgpu::BindGroupLayout,
    /// One uniform struct of a draw, chosen with a dynamic offset. Group 1 of the quad pipelines.
    pub uniform_layout: wgpu::BindGroupLayout,
    /// One uniform struct and a run of values, each chosen with a dynamic offset.
    pub points_layout: wgpu::BindGroupLayout,
    /// A history texture with its sampler, and a lookup texture with its sampler.
    pub history_layout: wgpu::BindGroupLayout,
    pub lut_layout: wgpu::BindGroupLayout,
    pub text_layout: wgpu::BindGroupLayout,
    pub linear_sampler: wgpu::Sampler,
    /// Linear, repeats in x (the ring of the spectrogram), clamps in y.
    pub ring_sampler: wgpu::Sampler,
    /// Bound when a shader declares a LUT but the draw does not use one.
    pub white_lut: Lut,
}

/// A 256 x 1 (or shorter) RGBA8 lookup texture with its bind group.
pub struct Lut {
    pub texture: wgpu::Texture,
    pub bind_group: wgpu::BindGroup,
}

#[derive(Clone, Copy)]
enum Blend {
    Over,
    Add,
    /// The target is multiplied by the blend constant of the pass.
    Fade,
    /// The source replaces the target.
    Replace,
}

impl Blend {
    fn state(self) -> wgpu::BlendState {
        use wgpu::{BlendComponent, BlendFactor, BlendOperation};
        let component = |src_factor, dst_factor| BlendComponent {
            src_factor,
            dst_factor,
            operation: BlendOperation::Add,
        };
        match self {
            Blend::Over => wgpu::BlendState {
                color: component(BlendFactor::One, BlendFactor::OneMinusSrcAlpha),
                alpha: component(BlendFactor::One, BlendFactor::OneMinusSrcAlpha),
            },
            Blend::Add => wgpu::BlendState {
                color: component(BlendFactor::One, BlendFactor::One),
                alpha: component(BlendFactor::Zero, BlendFactor::One),
            },
            Blend::Replace => wgpu::BlendState::REPLACE,
            Blend::Fade => wgpu::BlendState {
                color: component(BlendFactor::Zero, BlendFactor::Constant),
                alpha: component(BlendFactor::Zero, BlendFactor::Constant),
            },
        }
    }
}

impl RenderContext {
    /// The context of the process. `None` when no adapter gives a device.
    pub fn shared() -> Option<Arc<RenderContext>> {
        static SHARED: OnceLock<Option<Arc<RenderContext>>> = OnceLock::new();
        SHARED.get_or_init(|| RenderContext::new().map(Arc::new)).clone()
    }

    fn new() -> Option<Self> {
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle());
        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::LowPower,
            ..Default::default()
        }))
        .ok()?;
        let info = adapter.get_info();
        let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
            label: Some("Joseon"),
            ..Default::default()
        }))
        .ok()?;

        let module = |name: &str, source: &str| {
            device.create_shader_module(wgpu::ShaderModuleDescriptor {
                label: Some(name),
                source: wgpu::ShaderSource::Wgsl(format!("{}\n{source}", include_str!("shaders/common.wgsl")).into()),
            })
        };
        let shapes = module("shapes", include_str!("shaders/shapes.wgsl"));
        let curves = module("curves", include_str!("shaders/curves.wgsl"));
        let overlay_module = module("overlay", include_str!("shaders/overlay.wgsl"));
        let spectrogram_module = module("spectrogram", include_str!("shaders/spectrogram.wgsl"));

        // Bind group layouts.
        let uniform_entry = |binding, size: u64, dynamic| wgpu::BindGroupLayoutEntry {
            binding,
            visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
            ty: wgpu::BindingType::Buffer {
                ty: wgpu::BufferBindingType::Uniform,
                has_dynamic_offset: dynamic,
                min_binding_size: NonZeroU64::new(size),
            },
            count: None,
        };
        let storage_entry = |binding| wgpu::BindGroupLayoutEntry {
            binding,
            visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
            ty: wgpu::BindingType::Buffer {
                ty: wgpu::BufferBindingType::Storage { read_only: true },
                has_dynamic_offset: true,
                min_binding_size: NonZeroU64::new(4),
            },
            count: None,
        };
        let texture_entry = |binding| wgpu::BindGroupLayoutEntry {
            binding,
            visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
            ty: wgpu::BindingType::Texture {
                sample_type: wgpu::TextureSampleType::Float { filterable: true },
                view_dimension: wgpu::TextureViewDimension::D2,
                multisampled: false,
            },
            count: None,
        };
        let sampler_entry = |binding| wgpu::BindGroupLayoutEntry {
            binding,
            visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
            ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
            count: None,
        };
        let layout = |label, entries: &[wgpu::BindGroupLayoutEntry]| {
            device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some(label),
                entries,
            })
        };
        let globals_layout = layout(
            "globals",
            &[uniform_entry(0, std::mem::size_of::<Globals>() as u64, false)],
        );
        let curve_layout = layout(
            "curve",
            &[
                uniform_entry(0, std::mem::size_of::<CurveUniforms>() as u64, true),
                storage_entry(1),
                storage_entry(2),
            ],
        );
        let lut_layout = layout("lut", &[texture_entry(0), sampler_entry(1)]);
        // No minimum size here: each pipeline has its own struct, and the bound window (256 bytes) holds all of them.
        let uniform_layout = layout("uniform", &[uniform_entry(0, 0, true)]);
        let points_layout = layout("points", &[uniform_entry(0, 0, true), storage_entry(1)]);
        let history_layout = layout(
            "history",
            &[texture_entry(0), sampler_entry(1), texture_entry(2), sampler_entry(3)],
        );
        let text_layout = layout("text", &[texture_entry(0)]);

        // Vertex buffers: one instance per quad.
        const SHAPE_ATTRIBUTES: [wgpu::VertexAttribute; 4] =
            wgpu::vertex_attr_array![0 => Float32x4, 1 => Float32x4, 2 => Float32x4, 3 => Float32x4];
        let shape_buffer = wgpu::VertexBufferLayout {
            array_stride: std::mem::size_of::<ShapeInstance>() as u64,
            step_mode: wgpu::VertexStepMode::Instance,
            attributes: &SHAPE_ATTRIBUTES,
        };
        const RECT_ATTRIBUTES: [wgpu::VertexAttribute; 1] = wgpu::vertex_attr_array![0 => Float32x4];
        let rect_buffer = wgpu::VertexBufferLayout {
            array_stride: 16,
            step_mode: wgpu::VertexStepMode::Instance,
            attributes: &RECT_ATTRIBUTES,
        };

        let pipeline = |label: &str,
                        module: &wgpu::ShaderModule,
                        vertex: &str,
                        fragment: &str,
                        (blend, format): (Blend, wgpu::TextureFormat),
                        groups: &[&wgpu::BindGroupLayout],
                        buffers: &[Option<wgpu::VertexBufferLayout>]| {
            let groups: Vec<Option<&wgpu::BindGroupLayout>> = groups.iter().map(|g| Some(*g)).collect();
            let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some(label),
                bind_group_layouts: &groups,
                immediate_size: 0,
            });
            device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some(label),
                layout: Some(&layout),
                vertex: wgpu::VertexState {
                    module,
                    entry_point: Some(vertex),
                    compilation_options: Default::default(),
                    buffers,
                },
                primitive: wgpu::PrimitiveState {
                    topology: wgpu::PrimitiveTopology::TriangleStrip,
                    ..Default::default()
                },
                depth_stencil: None,
                multisample: Default::default(),
                fragment: Some(wgpu::FragmentState {
                    module,
                    entry_point: Some(fragment),
                    compilation_options: Default::default(),
                    targets: &[Some(wgpu::ColorTargetState {
                        format,
                        blend: Some(blend.state()),
                        write_mask: wgpu::ColorWrites::ALL,
                    })],
                }),
                multiview_mask: None,
                cache: None,
            })
        };

        let shape_groups = [&globals_layout];
        let shape_lut_groups = [&globals_layout, &lut_layout];
        let curve_groups = [&globals_layout, &curve_layout, &lut_layout];
        let overlay_groups = [&globals_layout, &text_layout];
        let shape_buffers = [Some(shape_buffer)];
        let rect_buffers = [Some(rect_buffer)];

        let shape_over = pipeline(
            "shape over",
            &shapes,
            "shape_vertex",
            "shape_fragment",
            (Blend::Over, TARGET_FORMAT),
            &shape_groups,
            &shape_buffers,
        );
        let shape_add = pipeline(
            "shape add",
            &shapes,
            "shape_vertex",
            "shape_fragment",
            (Blend::Add, TARGET_FORMAT),
            &shape_groups,
            &shape_buffers,
        );
        let shape_lut = pipeline(
            "shape lut",
            &shapes,
            "shape_vertex",
            "shape_lut_fragment",
            (Blend::Over, TARGET_FORMAT),
            &shape_lut_groups,
            &shape_buffers,
        );
        let curve = |label, vertex, fragment, blend| {
            pipeline(
                label,
                &curves,
                vertex,
                fragment,
                (blend, TARGET_FORMAT),
                &curve_groups,
                &[],
            )
        };
        let curve_line_over = curve(
            "curve line over",
            "curve_line_vertex",
            "curve_line_fragment",
            Blend::Over,
        );
        let curve_line_add = curve("curve line add", "curve_line_vertex", "curve_line_fragment", Blend::Add);
        let curve_fill = curve("curve fill", "curve_fill_vertex", "curve_fill_fragment", Blend::Over);
        let curve_band = curve("curve band", "curve_band_vertex", "curve_band_fragment", Blend::Over);
        let overlay = pipeline(
            "overlay",
            &overlay_module,
            "overlay_vertex",
            "overlay_fragment",
            (Blend::Over, TARGET_FORMAT),
            &overlay_groups,
            &rect_buffers,
        );

        let spectrogram = pipeline(
            "spectrogram",
            &spectrogram_module,
            "rect_vertex",
            "spectrogram_fragment",
            (Blend::Replace, TARGET_FORMAT),
            &[&globals_layout, &uniform_layout, &history_layout],
            &[],
        );

        let scope_module = module("scope", include_str!("shaders/scope.wgsl"));
        let pan_module = module("pan", include_str!("shaders/pan.wgsl"));
        let fade = |label, format| {
            pipeline(
                label,
                &scope_module,
                "fullscreen_vertex",
                "fade_fragment",
                (Blend::Fade, format),
                &[&globals_layout],
                &[],
            )
        };
        let scope_fade = fade("scope fade", SCOPE_ACCUM_FORMAT);
        let pan_fade = fade("pan fade", PAN_ACCUM_FORMAT);
        let scope_points = pipeline(
            "scope points",
            &scope_module,
            "scope_point_vertex",
            "scope_point_fragment",
            (Blend::Add, SCOPE_ACCUM_FORMAT),
            &[&globals_layout, &points_layout],
            &[],
        );
        let scope_composite = pipeline(
            "scope composite",
            &scope_module,
            "composite_vertex",
            "scope_composite_fragment",
            (Blend::Add, TARGET_FORMAT),
            &[&globals_layout, &uniform_layout, &history_layout],
            &[],
        );
        let pan_composite = pipeline(
            "pan composite",
            &scope_module,
            "composite_vertex",
            "pan_composite_fragment",
            (Blend::Add, TARGET_FORMAT),
            &[&globals_layout, &uniform_layout, &lut_layout],
            &[],
        );
        let pan_points = pipeline(
            "pan points",
            &pan_module,
            "pan_point_vertex",
            "pan_point_fragment",
            (Blend::Add, PAN_ACCUM_FORMAT),
            &[&globals_layout, &uniform_layout, &lut_layout],
            &rect_buffers,
        );

        let blit_module = module("blit", include_str!("shaders/blit.wgsl"));
        let blit = pipeline(
            "blit",
            &blit_module,
            "blit_vertex",
            "blit_fragment",
            (Blend::Over, TARGET_FORMAT),
            &[&globals_layout, &uniform_layout, &lut_layout],
            &[],
        );

        // The waterfall. The fold is a quad like the others; the bars and the floor have depth and four samples.
        let fold_module = module("waterfall fold", include_str!("shaders/waterfall_fold.wgsl"));
        let waterfall_module = module("waterfall", include_str!("shaders/waterfall.wgsl"));
        let waterfall_fold = pipeline(
            "waterfall fold",
            &fold_module,
            "fold_vertex",
            "fold_fragment",
            (Blend::Replace, FIELD_FORMAT),
            &[&globals_layout, &uniform_layout, &history_layout],
            &[],
        );
        let pipeline_3d = |label: &str, vertex: &str, fragment: &str, topology, cull| {
            let groups = [Some(&globals_layout), Some(&uniform_layout), Some(&history_layout)];
            let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some(label),
                bind_group_layouts: &groups,
                immediate_size: 0,
            });
            device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some(label),
                layout: Some(&layout),
                vertex: wgpu::VertexState {
                    module: &waterfall_module,
                    entry_point: Some(vertex),
                    compilation_options: Default::default(),
                    buffers: &[],
                },
                primitive: wgpu::PrimitiveState {
                    topology,
                    front_face: wgpu::FrontFace::Ccw,
                    cull_mode: cull,
                    ..Default::default()
                },
                depth_stencil: Some(wgpu::DepthStencilState {
                    format: WATERFALL_DEPTH_FORMAT,
                    depth_write_enabled: Some(true),
                    depth_compare: Some(wgpu::CompareFunction::Less),
                    stencil: Default::default(),
                    bias: Default::default(),
                }),
                multisample: wgpu::MultisampleState {
                    count: WATERFALL_SAMPLES,
                    ..Default::default()
                },
                fragment: Some(wgpu::FragmentState {
                    module: &waterfall_module,
                    entry_point: Some(fragment),
                    compilation_options: Default::default(),
                    targets: &[Some(wgpu::ColorTargetState {
                        format: TARGET_FORMAT,
                        blend: Some(wgpu::BlendState::REPLACE),
                        write_mask: wgpu::ColorWrites::ALL,
                    })],
                }),
                multiview_mask: None,
                cache: None,
            })
        };
        let waterfall_bars = pipeline_3d(
            "waterfall bars",
            "waterfall_vertex",
            "waterfall_fragment",
            wgpu::PrimitiveTopology::TriangleList,
            Some(wgpu::Face::Back),
        );
        let waterfall_floor = pipeline_3d(
            "waterfall floor",
            "floor_vertex",
            "floor_fragment",
            wgpu::PrimitiveTopology::TriangleStrip,
            None,
        );

        let ring_sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("ring"),
            address_mode_u: wgpu::AddressMode::Repeat,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });
        let linear_sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("linear clamp"),
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });
        let white_lut = make_lut(
            &device,
            &queue,
            &lut_layout,
            &linear_sampler,
            &[[1.0, 1.0, 1.0], [1.0, 1.0, 1.0]],
        );

        Some(Self {
            adapter_info: format!("{} ({:?})", info.name, info.backend),
            software: info.device_type == wgpu::DeviceType::Cpu,
            instance,
            adapter,
            device,
            queue,
            shape_over,
            shape_add,
            shape_lut,
            curve_line_over,
            curve_line_add,
            curve_fill,
            curve_band,
            overlay,
            spectrogram,
            scope_fade,
            scope_points,
            scope_composite,
            pan_fade,
            pan_points,
            pan_composite,
            blit,
            waterfall_fold,
            waterfall_bars,
            waterfall_floor,
            points_layout,
            globals_layout,
            curve_layout,
            uniform_layout,
            history_layout,
            ring_sampler,
            lut_layout,
            text_layout,
            linear_sampler,
            white_lut,
        })
    }

    /// A lookup texture: one texel per color.
    pub fn make_lut(&self, colors: &[[f32; 3]]) -> Lut {
        make_lut(
            &self.device,
            &self.queue,
            &self.lut_layout,
            &self.linear_sampler,
            colors,
        )
    }
}

/// The bytes of a lookup texture: RGBA8, alpha 255, each color clamped to 0...1.
pub fn lut_bytes(colors: &[[f32; 3]]) -> Vec<u8> {
    let mut bytes = vec![255u8; colors.len() * 4];
    for (i, color) in colors.iter().enumerate() {
        for (channel, value) in color.iter().enumerate() {
            bytes[i * 4 + channel] = (value.clamp(0.0, 1.0) * 255.0 + 0.5) as u8;
        }
    }
    bytes
}

fn make_lut(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    layout: &wgpu::BindGroupLayout,
    sampler: &wgpu::Sampler,
    colors: &[[f32; 3]],
) -> Lut {
    let size = wgpu::Extent3d {
        width: colors.len().max(1) as u32,
        height: 1,
        depth_or_array_layers: 1,
    };
    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("lut"),
        size,
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba8Unorm,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    });
    queue.write_texture(
        texture.as_image_copy(),
        &lut_bytes(colors),
        wgpu::TexelCopyBufferLayout {
            offset: 0,
            bytes_per_row: Some(size.width * 4),
            rows_per_image: None,
        },
        size,
    );
    let view = texture.create_view(&Default::default());
    let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("lut"),
        layout,
        entries: &[
            wgpu::BindGroupEntry {
                binding: 0,
                resource: wgpu::BindingResource::TextureView(&view),
            },
            wgpu::BindGroupEntry {
                binding: 1,
                resource: wgpu::BindingResource::Sampler(sampler),
            },
        ],
    });
    Lut { texture, bind_group }
}
