//! The text of a panel (axis labels, readouts, the hover label) as a GPU texture.
//!
//! Port source: `Sources/JoseonRender/TextLayer.swift`.
//!
//! The items are painted into one CPU bitmap. Only what differs from the
//! bitmap is painted: axis labels and captions once per layout, a readout when
//! its digits change. The changed part of the bitmap is written to a texture,
//! and the panel pass draws one small quad per text rectangle.
//!
//! On macOS there are two bitmaps (front and back), because Metal reads the
//! bitmap memory directly. Here the texture is a copy, so one bitmap is enough.

use super::overlay::{OverlayContext, OverlayItem, Rect};
use crate::arena::FrameArena;
use crate::context::RenderContext;
use std::collections::HashMap;

struct Gpu {
    texture: wgpu::Texture,
    bind_group: wgpu::BindGroup,
}

#[derive(Default)]
pub struct TextLayer {
    pixmap: Option<tiny_skia::Pixmap>,
    gpu: Option<Gpu>,
    /// Points.
    width: f32,
    height: f32,
    scale: f32,
    /// What the bitmap holds.
    items: Vec<OverlayItem>,
    /// Rectangles that hold text, points, pixel aligned.
    rects: Vec<Rect>,
    /// Pixels of the bitmap that the texture does not have yet: x0, y0, x1, y1.
    pending: Option<[u32; 4]>,

    /// Counts redraws (tests and the frame cost report).
    pub redraw_count: u64,
    /// Ink boxes of the text of the newest redraw (layout tests).
    pub last_labels: Vec<(String, Rect)>,
    /// Items of the newest redraw, and how many of them had to be painted.
    pub last_item_count: usize,
    pub last_painted_count: usize,
    /// Rectangles of the items the newest redraw painted (tests).
    pub last_painted_rects: Vec<Rect>,
}

impl TextLayer {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn is_ready(&self) -> bool {
        self.pixmap.is_some()
    }

    /// Rectangles that hold text in the bitmap.
    pub fn rects(&self) -> &[Rect] {
        &self.rects
    }

    /// The bitmap: premultiplied RGBA, and its size in pixels.
    pub fn pixels(&self) -> Option<(&[u8], u32, u32)> {
        self.pixmap.as_ref().map(|p| (p.data(), p.width(), p.height()))
    }

    /// Makes the bitmap. Does nothing when the size and the scale are the same as before.
    pub fn resize(&mut self, width: f32, height: f32, scale: f32) {
        if width == self.width && height == self.height && scale == self.scale && self.is_ready() {
            return;
        }
        self.width = width;
        self.height = height;
        self.scale = scale;
        let w = ((width * scale).round() as u32).max(1);
        let h = ((height * scale).round() as u32).max(1);
        self.pixmap = tiny_skia::Pixmap::new(w, h);
        self.gpu = None;
        self.items.clear();
        self.rects.clear();
        self.pending = None;
    }

    fn pixel_rect(&self, r: &Rect) -> [u32; 4] {
        let Some(pixmap) = &self.pixmap else { return [0; 4] };
        let s = self.scale;
        let x0 = ((r.min_x() * s).floor().max(0.0) as u32).min(pixmap.width());
        let y0 = ((r.min_y() * s).floor().max(0.0) as u32).min(pixmap.height());
        let x1 = ((r.max_x() * s).ceil().max(0.0) as u32).min(pixmap.width());
        let y1 = ((r.max_y() * s).ceil().max(0.0) as u32).min(pixmap.height());
        [x0, y0, x1, y1]
    }

    /// Brings the text up to date. Only what differs from the bitmap is
    /// painted: changed items, and unchanged items that touch a rectangle that
    /// must be cleared.
    pub fn redraw(&mut self, o: &OverlayContext) {
        if !self.is_ready() {
            return;
        }
        let new = o.items();
        let old = std::mem::take(&mut self.items);

        // Same key = same pixels in the same place. Count the old items by key, then match the new ones against them.
        let mut available: HashMap<u64, usize> = HashMap::with_capacity(old.len());
        for item in &old {
            *available.entry(item.key).or_insert(0) += 1;
        }
        let mut paint = vec![false; new.len()];
        let mut dirty: Vec<Rect> = Vec::new();
        for (i, item) in new.iter().enumerate() {
            match available.get_mut(&item.key) {
                Some(count) if *count > 0 => *count -= 1,
                _ => {
                    paint[i] = true;
                    dirty.push(item.rect);
                }
            }
        }
        // Old items that are gone (or changed): their pixels must be cleared.
        for item in old.iter().rev() {
            if let Some(count) = available.get_mut(&item.key) {
                if *count > 0 {
                    *count -= 1;
                    dirty.push(item.rect);
                }
            }
        }
        // An unchanged item that touches a cleared rectangle is painted again, and its own rectangle is cleared too.
        let mut grew = !dirty.is_empty();
        while grew {
            grew = false;
            for (i, item) in new.iter().enumerate() {
                if !paint[i] && dirty.iter().any(|d| d.intersects(&item.rect)) {
                    paint[i] = true;
                    dirty.push(item.rect);
                    grew = true;
                }
            }
        }

        let mut painted = 0;
        if !dirty.is_empty() {
            let mut bounds = self.pending;
            for r in &dirty {
                let [x0, y0, x1, y1] = self.pixel_rect(r);
                let pixmap = self.pixmap.as_mut().unwrap();
                let stride = pixmap.width() as usize * 4;
                let data = pixmap.data_mut();
                for y in y0..y1 {
                    let row = y as usize * stride;
                    data[row + x0 as usize * 4..row + x1 as usize * 4].fill(0);
                }
                bounds = Some(match bounds {
                    None => [x0, y0, x1, y1],
                    Some(b) => [b[0].min(x0), b[1].min(y0), b[2].max(x1), b[3].max(y1)],
                });
            }
            self.pending = bounds;
            self.last_painted_rects.clear();
            let scale = self.scale;
            let pixmap = self.pixmap.as_mut().unwrap();
            for (i, item) in new.iter().enumerate() {
                if paint[i] {
                    item.draw(pixmap, scale);
                    painted += 1;
                    self.last_painted_rects.push(item.rect);
                }
            }
        }
        self.items = new.to_vec();
        // Rectangles for the GPU pass: leave out what another rectangle covers (a box and the text inside it).
        self.rects.clear();
        for item in new {
            if !self.rects.iter().any(|e| e.contains(&item.rect)) {
                self.rects.push(item.rect);
            }
        }
        self.last_labels = o.labels().to_vec();
        self.last_item_count = new.len();
        self.last_painted_count = painted;
        self.redraw_count += 1;
    }

    /// Writes the changed part of the bitmap to the texture. Call it before the pass is submitted.
    pub fn upload(&mut self, ctx: &RenderContext) {
        let Some(pixmap) = &self.pixmap else { return };
        let (w, h) = (pixmap.width(), pixmap.height());
        if self.gpu.is_none() {
            let texture = ctx.device.create_texture(&wgpu::TextureDescriptor {
                label: Some("Joseon text"),
                size: wgpu::Extent3d {
                    width: w,
                    height: h,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: wgpu::TextureFormat::Rgba8Unorm,
                usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
                view_formats: &[],
            });
            let view = texture.create_view(&Default::default());
            let bind_group = ctx.device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("text"),
                layout: &ctx.text_layout,
                entries: &[wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&view),
                }],
            });
            self.gpu = Some(Gpu { texture, bind_group });
            // A new texture has nothing: send the whole bitmap.
            self.pending = Some([0, 0, w, h]);
        }
        let Some([x0, y0, x1, y1]) = self.pending.take() else {
            return;
        };
        if x1 <= x0 || y1 <= y0 {
            return;
        }
        let gpu = self.gpu.as_ref().unwrap();
        let stride = w as usize * 4;
        let start = y0 as usize * stride + x0 as usize * 4;
        ctx.queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &gpu.texture,
                mip_level: 0,
                origin: wgpu::Origin3d { x: x0, y: y0, z: 0 },
                aspect: wgpu::TextureAspect::All,
            },
            &pixmap.data()[start..],
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(stride as u32),
                rows_per_image: None,
            },
            wgpu::Extent3d {
                width: x1 - x0,
                height: y1 - y0,
                depth_or_array_layers: 1,
            },
        );
    }

    /// Draws the text rectangles into the pass. `upload` must be called before the submit.
    pub fn draw(&self, pass: &mut wgpu::RenderPass<'_>, ctx: &RenderContext, arena: &mut FrameArena) {
        let Some(gpu) = &self.gpu else { return };
        let n = self.rects.len();
        if n == 0 {
            return;
        }
        let instances: Vec<[f32; 4]> = self.rects.iter().map(|r| [r.x, r.y, r.w, r.h]).collect();
        let Some(offset) = arena.push_instances(&instances) else {
            return;
        };
        pass.set_pipeline(&ctx.overlay);
        pass.set_bind_group(0, &arena.globals_group, &[]);
        pass.set_bind_group(1, &gpu.bind_group, &[]);
        pass.set_vertex_buffer(0, arena.instance_buffer.slice(offset..offset + n as u64 * 16));
        pass.draw(0..4, 0..n as u32);
    }
}
