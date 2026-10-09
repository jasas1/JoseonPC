//! An offscreen color target, its read-back, and the PNG encoder.

use crate::context::{RenderContext, TARGET_FORMAT};

pub struct OffscreenTarget {
    pub texture: wgpu::Texture,
    pub view: wgpu::TextureView,
    pub width: u32,
    pub height: u32,
}

impl OffscreenTarget {
    pub fn new(ctx: &RenderContext, width: u32, height: u32) -> Self {
        let texture = ctx.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("Joseon offscreen target"),
            size: wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: TARGET_FORMAT,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT
                | wgpu::TextureUsages::COPY_SRC
                | wgpu::TextureUsages::TEXTURE_BINDING,
            view_formats: &[],
        });
        let view = texture.create_view(&Default::default());
        Self {
            texture,
            view,
            width,
            height,
        }
    }

    /// The pixels as RGBA8, row by row from the top. Waits for the GPU.
    pub fn read_rgba(&self, ctx: &RenderContext) -> Option<Vec<u8>> {
        let (width, height) = (self.width as usize, self.height as usize);
        // A copy to a buffer needs rows of a multiple of 256 bytes.
        let padded_row = (width * 4).div_ceil(256) * 256;
        let buffer = ctx.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("Joseon read-back"),
            size: (padded_row * height) as u64,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        let mut encoder = ctx.device.create_command_encoder(&Default::default());
        encoder.copy_texture_to_buffer(
            self.texture.as_image_copy(),
            wgpu::TexelCopyBufferInfo {
                buffer: &buffer,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(padded_row as u32),
                    rows_per_image: None,
                },
            },
            wgpu::Extent3d {
                width: self.width,
                height: self.height,
                depth_or_array_layers: 1,
            },
        );
        ctx.queue.submit([encoder.finish()]);
        let (sender, receiver) = std::sync::mpsc::channel();
        buffer.slice(..).map_async(wgpu::MapMode::Read, move |result| {
            let _ = sender.send(result);
        });
        ctx.device.poll(wgpu::PollType::wait_indefinitely()).ok()?;
        receiver.recv().ok()?.ok()?;
        let mapped = buffer.slice(..).get_mapped_range().ok()?;
        let mut rgba = vec![0u8; width * height * 4];
        for y in 0..height {
            let row = &mapped[y * padded_row..y * padded_row + width * 4];
            let out = &mut rgba[y * width * 4..(y + 1) * width * 4];
            for (source, pixel) in row.chunks_exact(4).zip(out.chunks_exact_mut(4)) {
                // The target is BGRA.
                pixel.copy_from_slice(&[source[2], source[1], source[0], source[3]]);
            }
        }
        Some(rgba)
    }
}

/// An opaque 8-bit RGB PNG from RGBA pixels. The alpha is dropped, as on macOS.
pub fn encode_png(width: u32, height: u32, rgba: &[u8]) -> Vec<u8> {
    let rgb: Vec<u8> = rgba.chunks_exact(4).flat_map(|p| [p[0], p[1], p[2]]).collect();
    let mut out = Vec::new();
    {
        let mut encoder = png::Encoder::new(&mut out, width, height);
        encoder.set_color(png::ColorType::Rgb);
        encoder.set_depth(png::BitDepth::Eight);
        encoder.set_source_srgb(png::SrgbRenderingIntent::Perceptual);
        let mut writer = encoder.write_header().expect("a PNG header in memory");
        writer.write_image_data(&rgb).expect("PNG data in memory");
    }
    out
}

/// A decoded PNG: width, height and RGBA pixels. For tests.
pub fn decode_png(bytes: &[u8]) -> Option<(u32, u32, Vec<u8>)> {
    let mut reader = png::Decoder::new(std::io::Cursor::new(bytes)).read_info().ok()?;
    let mut buffer = vec![0; reader.output_buffer_size()?];
    let info = reader.next_frame(&mut buffer).ok()?;
    buffer.truncate(info.buffer_size());
    let rgba = match info.color_type {
        png::ColorType::Rgb => buffer.chunks_exact(3).flat_map(|p| [p[0], p[1], p[2], 255]).collect(),
        png::ColorType::Rgba => buffer,
        _ => return None,
    };
    Some((info.width, info.height, rgba))
}
