//! The vectorscope panel. Two pictures in one field:
//!
//! - Goniometer with persistence. Points add light into a half-float density
//!   texture; every frame the texture fades by exp(-dt / tau). A log tone map
//!   turns density into color.
//! - Stereo placement by frequency: y = log frequency (20 Hz at the bottom,
//!   20 kHz at the top), x = pan position of each analyzer bin, light = level,
//!   hue = the band hue of the frequency.
//!
//! Beside or under the field: correlation, width, balance and the 8-band table.
//!
//! Port source: `Sources/JoseonRender/VectorscopeRenderer.swift`.

pub mod pan_field;

use crate::axis::LogAxis;
use crate::context::{Lut, PAN_ACCUM_FORMAT, SCOPE_ACCUM_FORMAT};
use crate::contracts::{PanelCursor, PanelKind, VectorscopeMode};
use crate::cursor::{self, header, CursorReadoutItem, Tone};
use crate::fmt::{self, band_labels};
use crate::gpu::Color;
use crate::palette::{mix, rgba, scaled_alpha, with_alpha, Palette, WHITE};
use crate::panel::{Panel, PanelBase, Signature};
use crate::text::fonts::{self, Weight};
use crate::text::{HAlign, OverlayContext, Rect, VAlign};
use bytemuck::{Pod, Zeroable};
use joseon_core::contracts::{AnalysisFrame, BandEnergy};
use pan_field::PanField;
use std::any::Any;

/// The automatic gain never zooms in further: beyond this the figure would be the noise floor, not the music.
pub const MAX_GAIN: f32 = 8.0;
/// The light scale of the pan spectrum spans this many dB under its top (never under the gate of `PanField`).
const PAN_RANGE_DB: f32 = 54.0;
const HISTOGRAM_BINS: usize = 128;
/// The full-scale diamond reaches this part of the half field. The rest is room for the axis labels.
const DIAMOND: f32 = 0.86;
const PAN_AXIS: LogAxis = LogAxis {
    min_hz: 20.0,
    max_hz: 20_000.0,
};

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct ScopeUniforms {
    center: [f32; 2],
    radius: f32,
    gain: f32,
    point_size: f32,
    energy: f32,
    tex_size: [f32; 2],
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct PanUniforms {
    tex_size: [f32; 2],
    center_x: f32,
    half_x: f32,
    top: f32,
    height: f32,
    sigma_x: f32,
    energy: f32,
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct CompositeUniforms {
    rect: [f32; 4],
    k: f32,
    norm: f32,
    pad: [f32; 2],
}

/// Where the numbers go. The figure comes first: it takes the card height (or width), the numbers take what is left.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Arrangement {
    TableBeside,
    StripUnder,
    KeysBeside,
    KeysUnder,
}

/// A float texture that collects light, with the group that reads it back.
struct Accum {
    view: wgpu::TextureView,
    /// The scope: built on first use (it also holds the color ramp). The pan spectrum: built with the texture.
    group: Option<wgpu::BindGroup>,
    width: u32,
    height: u32,
}

pub struct VectorscopePanel {
    pub persistence_seconds: f64,
    mode: VectorscopeMode,
    scope_lut: Option<Lut>,
    accum: Option<Accum>,
    pan_accum: Option<Accum>,
    pan_hue: Option<Lut>,
    pub pan_field: PanField,
    /// Splats of the newest pan frame (for tests): pan, axis position 0...1, light, vertical sigma in pixels.
    pub last_splats: Vec<[f32; 4]>,
    pub keeps_splats_for_testing: bool,
    accum_needs_clear: bool,
    pending_fade: f64,
    pending_points: bool,
    since_light: f64,
    gain: f32,
    gain_snapped: bool,
    max_points: usize,
    pan_top_db: f32,
    /// Peak hold of the scope level for the automatic gain.
    held_level: f32,
    held_for: f64,
    histogram: [i32; HISTOGRAM_BINS],

    // Layout.
    field: Rect,
    corr_bar: Rect,
    width_bar: Rect,
    bands: Rect,
    corr_header: [f32; 2],
    side_by_side: bool,
    narrow_table: bool,
    arrangement: Arrangement,
    /// Key numbers (correlation, width, balance) when there is no room for the band table.
    key_cells: Vec<Rect>,
    balance_bar: Rect,

    // Smoothed display values.
    shown_correlation: f32,
}

impl Default for VectorscopePanel {
    fn default() -> Self {
        Self::new()
    }
}

/// "R 19%": the share of the level difference, with its unit. "C" at the center, a dash without signal.
pub fn balance_text(b: f32, silent: bool) -> String {
    if silent {
        return fmt::DASH.to_string();
    }
    let pct = (b.abs() * 100.0).round() as i32;
    if pct == 0 {
        return "C".to_string();
    }
    format!("{}{pct}%", if b < 0.0 { "L " } else { "R " })
}

/// No signal = no stereo measurement. The analyzer marks silence with `is_silent` and still sends its (all zero)
/// scope points and a correlation of 0: that 0 is a placeholder, not a reading.
pub fn no_signal(f: &AnalysisFrame) -> bool {
    f.is_silent || f.stereo.scope_points.is_empty()
}

pub fn pan_legend_steps(width: f32, scale: f32) -> usize {
    ((width * scale).round() as usize).clamp(12, 512)
}

fn clamp1(v: f32) -> f32 {
    v.max(-1.0).min(1.0)
}

impl VectorscopePanel {
    pub fn new() -> Self {
        Self {
            persistence_seconds: 0.45,
            mode: VectorscopeMode::Lissajous,
            scope_lut: None,
            accum: None,
            pan_accum: None,
            pan_hue: None,
            pan_field: PanField::new(),
            last_splats: Vec::new(),
            keeps_splats_for_testing: false,
            accum_needs_clear: true,
            pending_fade: 0.0,
            pending_points: false,
            since_light: 100.0,
            gain: 1.0,
            gain_snapped: false,
            max_points: 4096,
            pan_top_db: -20.0,
            held_level: 0.0,
            held_for: 0.0,
            histogram: [0; HISTOGRAM_BINS],
            field: Rect::default(),
            corr_bar: Rect::default(),
            width_bar: Rect::default(),
            bands: Rect::default(),
            corr_header: [0.0; 2],
            side_by_side: false,
            narrow_table: false,
            arrangement: Arrangement::TableBeside,
            key_cells: Vec::new(),
            balance_bar: Rect::default(),
            shown_correlation: 0.0,
        }
    }

    pub fn mode(&self) -> VectorscopeMode {
        self.mode
    }

    pub fn set_mode(&mut self, base: &mut PanelBase, mode: VectorscopeMode) {
        if mode != self.mode {
            self.mode = mode;
            self.accum_needs_clear = true;
            self.pan_field.reset();
            base.needs_display = true;
            self.layout_changed(base);
        }
    }

    pub fn gain(&self) -> f32 {
        self.gain
    }

    pub fn arrangement(&self) -> Arrangement {
        self.arrangement
    }

    /// The scope square (or the pan field).
    pub fn field(&self) -> Rect {
        self.field
    }

    pub fn correlation_bar(&self) -> Rect {
        self.corr_bar
    }

    pub fn width_bar(&self) -> Rect {
        self.width_bar
    }

    pub fn band_table(&self) -> Rect {
        self.bands
    }

    pub fn key_cells(&self) -> &[Rect] {
        &self.key_cells
    }

    fn is_pan(&self) -> bool {
        self.mode == VectorscopeMode::PanSpectrum
    }

    // MARK: Geometry of the pan spectrum

    /// The pan spectrum keeps a gutter on the left for the frequency labels.
    fn pan_gutter(&self) -> f32 {
        if self.field.w < 300.0 {
            30.0
        } else {
            36.0
        }
    }

    pub fn pan_center_x(&self) -> f32 {
        self.field.min_x() + self.pan_gutter() + (self.field.w - self.pan_gutter()) / 2.0
    }

    pub fn pan_half(&self) -> f32 {
        (self.field.w - self.pan_gutter()) / 2.0 * 0.90
    }

    /// The frequency axis inside the field: room for the L / C / R row over 20 kHz and for the legend under 20 Hz.
    pub fn pan_axis_rect(&self) -> Rect {
        let top = 24.0;
        let bottom = if self.field.h < 220.0 { 10.0 } else { 24.0 };
        Rect::new(
            self.field.min_x(),
            self.field.min_y() + top,
            self.field.w,
            (self.field.h - top - bottom).max(20.0),
        )
    }

    /// Intensity legend, bottom right inside the field. `None` when the field is too small.
    pub fn pan_legend_rect(&self) -> Option<Rect> {
        if !(self.field.w >= 200.0 && self.field.h >= 220.0) {
            return None;
        }
        // Wide enough to read three numbers off it: the ends and the middle.
        let w = (self.field.w * 0.34).min(160.0);
        Some(Rect::new(
            self.field.max_x() - 12.0 - 22.0 - w,
            self.field.max_y() - 21.0,
            w,
            8.0,
        ))
    }

    /// y of a frequency on the placement axis.
    pub fn pan_y(&self, hz: f32) -> f32 {
        let axis = self.pan_axis_rect();
        axis.max_y() - PAN_AXIS.position(hz) * axis.h
    }

    // MARK: State

    /// Automatic gain: the 99.5th percentile of the sample level sits at 85 % of the full-scale diamond, and the
    /// loudest sample stays inside 96 %, so the trace never touches the tips. A percentile, so a single stray sample
    /// does not shrink the figure. The level is held for 1.5 s and then released slowly: a kick drum must not find a
    /// gain that crept up in the gap before it. Down is instant. The factor is on screen.
    fn update_gain(&mut self, pts: &[[f32; 2]], dt: f64) {
        if pts.len() < 16 {
            return;
        }
        let bins = HISTOGRAM_BINS;
        self.histogram.fill(0);
        let mut top = 0.0f32;
        for p in pts {
            let d = p[0].abs() + p[1].abs(); // = max(|L|, |R|) with x = (R - L) / 2, y = (L + R) / 2
            top = top.max(d);
            self.histogram[((d * bins as f32) as usize).min(bins - 1)] += 1;
        }
        let limit = (pts.len() as f32 * 0.005).ceil() as i32;
        let mut above = 0;
        let mut bin = bins - 1;
        while bin > 0 {
            above += self.histogram[bin];
            if above >= limit {
                break;
            }
            bin -= 1;
        }
        // The level the gain must fit: the percentile at 85 %, or the maximum at 96 %, whichever asks for less gain.
        let level = ((bin as f32 + 1.0) / bins as f32).max(top * (0.85 / 0.96));
        if level >= self.held_level {
            self.held_level = level;
            self.held_for = 0.0;
        } else {
            self.held_for += dt;
            if self.held_for > 1.5 {
                self.held_level += (level - self.held_level) * (1.0 - (-dt / 2.5).exp()) as f32;
            }
        }
        if !(self.held_level > 0.004) {
            return; // under -48 dBFS: keep the gain, do not zoom into noise
        }
        let want = (0.85 / self.held_level).max(0.85).min(MAX_GAIN);
        if !self.gain_snapped || want < self.gain {
            self.gain_snapped = true;
            self.gain = want;
            return;
        }
        self.gain += (want - self.gain) * (1.0 - (-dt / 1.0).exp()) as f32;
    }

    fn make_accum(base: &PanelBase, format: wgpu::TextureFormat, width: u32, height: u32, label: &str) -> Accum {
        let texture = base.ctx.device.create_texture(&wgpu::TextureDescriptor {
            label: Some(label),
            size: wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING,
            view_formats: &[],
        });
        Accum {
            view: texture.create_view(&Default::default()),
            group: None,
            width,
            height,
        }
    }

    fn begin_accum_pass<'a>(
        encoder: &'a mut wgpu::CommandEncoder,
        view: &wgpu::TextureView,
        clear: bool,
        label: &str,
    ) -> wgpu::RenderPass<'a> {
        encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some(label),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view,
                depth_slice: None,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: if clear {
                        wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT)
                    } else {
                        wgpu::LoadOp::Load
                    },
                    store: wgpu::StoreOp::Store,
                },
            })],
            ..Default::default()
        })
    }

    fn encode_scope_pre_pass(&mut self, base: &mut PanelBase, encoder: &mut wgpu::CommandEncoder) {
        let Some(accum) = &self.accum else { return };
        let ctx = base.ctx.clone();
        let mut pass = Self::begin_accum_pass(encoder, &accum.view, self.accum_needs_clear, "Joseon scope accumulate");
        self.accum_needs_clear = false;

        if self.pending_fade > 0.0 {
            let fade = (-self.pending_fade / self.persistence_seconds.max(0.01)).exp();
            pass.set_pipeline(&ctx.scope_fade);
            pass.set_bind_group(0, &base.arena.globals_group, &[]);
            pass.set_blend_constant(wgpu::Color {
                r: fade,
                g: fade,
                b: fade,
                a: fade,
            });
            pass.draw(0..4, 0..1);
            self.pending_fade = 0.0;
        }

        if self.pending_points {
            if let Some(f) = base.frame.clone() {
                let pts = &f.stereo.scope_points;
                let n = pts.len().min(self.max_points);
                if n > 1 {
                    let newest: &[f32] = bytemuck::cast_slice(&pts[pts.len() - n..]);
                    let tex = accum.width as f32;
                    // A thin beam: filaments stay filaments. No wide halo pass: the log tone map lifts the faint light.
                    let sigma = (base.scale * 0.42).max(0.6);
                    // Energy per point scales with 1 / count so the picture keeps its brightness when the point count
                    // changes. A small field packs the same trace into fewer pixels: the light scales with the field.
                    let energy = (1024.0 / n.max(64) as f32)
                        * (base.frame_delta * 60.0).min(3.0) as f32
                        * (tex / 1000.0).powf(1.6).max(0.08).min(2.5);
                    let uniforms = ScopeUniforms {
                        center: [tex / 2.0, tex / 2.0],
                        radius: tex / 2.0 * DIAMOND,
                        gain: self.gain,
                        point_size: sigma,
                        energy,
                        tex_size: [tex, tex],
                    };
                    if let (Some(values), Some(offset)) =
                        (base.arena.push_floats(newest), base.arena.push_uniform(&uniforms))
                    {
                        pass.set_pipeline(&ctx.scope_points);
                        pass.set_bind_group(0, &base.arena.globals_group, &[]);
                        pass.set_bind_group(1, &base.arena.points_group, &[offset, values.offset]);
                        pass.draw(0..4, 0..(n - 1) as u32);
                    }
                }
            }
            self.pending_points = false;
        }
    }

    fn encode_pan_pre_pass(&mut self, base: &mut PanelBase, encoder: &mut wgpu::CommandEncoder) {
        let Some(accum) = &self.pan_accum else { return };
        let ctx = base.ctx.clone();
        let mut pass = Self::begin_accum_pass(
            encoder,
            &accum.view,
            self.accum_needs_clear,
            "Joseon pan spectrum accumulate",
        );
        self.accum_needs_clear = false;

        if self.pending_fade > 0.0 {
            // Short persistence: the cloud shimmers, it does not smear.
            let fade = (-self.pending_fade / (self.persistence_seconds * 0.6).max(0.01)).exp();
            pass.set_pipeline(&ctx.pan_fade);
            pass.set_bind_group(0, &base.arena.globals_group, &[]);
            pass.set_blend_constant(wgpu::Color {
                r: fade,
                g: fade,
                b: fade,
                a: fade,
            });
            pass.draw(0..4, 0..1);
            self.pending_fade = 0.0;
        }

        if self.pending_points {
            if let (Some(f), Some(hue)) = (base.frame.clone(), &self.pan_hue) {
                let freqs = &f.spectrum.frequencies;
                let n = self.pan_field.count().min(freqs.len());
                if n >= 8 {
                    let (tex_w, tex_h) = (accum.width as f32, accum.height as f32);
                    let axis = self.pan_axis_rect();
                    let field = self.field;
                    let axis_top = (axis.min_y() - field.min_y()) / field.h;
                    let axis_height = axis.h / field.h;
                    // Splat size: sigma about 1.2 % of the pan span across, a third of that in height: a tone is a
                    // compact dot, noise a smooth cloud. Bins lie closer than the splat is high: the energy is shared.
                    let sigma_x = (self.pan_half() * 2.0 * base.scale * 0.012).max(1.5);
                    let sigma_y = (sigma_x * 0.34).max(1.2);
                    let bins_in_axis =
                        (n - 1) as f32 * (PAN_AXIS.max_hz / PAN_AXIS.min_hz).ln() / (freqs[n - 1] / freqs[0]).ln();
                    let pitch = tex_h * axis_height / bins_in_axis.max(1.0);
                    let top = self.pan_top_db + 3.0;
                    let bottom = top - PAN_RANGE_DB;
                    let mut splats: Vec<[f32; 4]> = Vec::with_capacity(n);
                    let (pan, spread) = (self.pan_field.pan(), self.pan_field.spread());
                    for i in 0..n {
                        if !(freqs[i] >= PAN_AXIS.min_hz && freqs[i] <= PAN_AXIS.max_hz) {
                            continue;
                        }
                        let light = self.pan_field.light(i, top, bottom);
                        if !(light > 0.0) {
                            continue;
                        }
                        // Under 150 Hz one FFT bin covers several display bins: the splat grows to the height of
                        // that FFT bin.
                        let sy = sigma_y.max(spread[i] as f32 * pitch * 0.9);
                        // Louder = more light, steeply: a partial 30 dB down is a faint veil. Same light per height
                        // for every sigma.
                        splats.push([
                            clamp1(pan[i]),
                            PAN_AXIS.position(freqs[i]),
                            light * light.sqrt() * (sigma_y / sy),
                            sy,
                        ]);
                    }
                    if !splats.is_empty() {
                        let energy =
                            0.30 * (pitch / (sigma_y * 2.5)).min(1.0) * (base.frame_delta * 60.0).min(3.0) as f32;
                        let uniforms = PanUniforms {
                            tex_size: [tex_w, tex_h],
                            center_x: (self.pan_center_x() - field.min_x()) / field.w,
                            half_x: self.pan_half() / field.w,
                            top: axis_top,
                            height: axis_height,
                            sigma_x,
                            energy,
                        };
                        if let (Some(start), Some(offset)) =
                            (base.arena.push_instances(&splats), base.arena.push_uniform(&uniforms))
                        {
                            pass.set_pipeline(&ctx.pan_points);
                            pass.set_bind_group(0, &base.arena.globals_group, &[]);
                            pass.set_bind_group(1, &base.arena.uniform_group, &[offset]);
                            pass.set_bind_group(2, &hue.bind_group, &[]);
                            pass.set_vertex_buffer(
                                0,
                                base.arena
                                    .instance_buffer
                                    .slice(start..start + splats.len() as u64 * 16),
                            );
                            pass.draw(0..4, 0..splats.len() as u32);
                        }
                    }
                    if self.keeps_splats_for_testing {
                        self.last_splats = splats;
                    }
                }
            }
            self.pending_points = false;
        }
    }

    // MARK: Shapes

    fn draw_scope_guides(&self, base: &mut PanelBase) {
        let p = base.palette;
        let (fx, fy, fs) = (self.field.min_x(), self.field.min_y(), self.field.w);
        let (cx, cy) = (fx + fs / 2.0, fy + fs / 2.0);
        let r = fs / 2.0 * DIAMOND;
        // Full-scale diamond, half-scale diamond.
        Self::diamond(base, cx, cy, r, scaled_alpha(p.grid_major, 1.5));
        Self::diamond(base, cx, cy, r * 0.5, scaled_alpha(p.grid_minor, 1.6));
        // M and S axes, L and R diagonals.
        base.vline(cx, cy - r, cy + r, scaled_alpha(p.grid_major, 1.4));
        base.hline(cx - r, cx + r, cy, p.grid_major);
        let dg = r * 0.5;
        let c = scaled_alpha(p.grid_minor, 1.8);
        Self::line(base, [cx - dg, cy - dg], [cx + dg, cy + dg], c);
        Self::line(base, [cx + dg, cy - dg], [cx - dg, cy + dg], c);
    }

    fn line(base: &mut PanelBase, from: [f32; 2], to: [f32; 2], color: Color) {
        base.batch.line(&mut base.arena, from, to, 1.0, color, None, 0.0);
    }

    fn diamond(base: &mut PanelBase, cx: f32, cy: f32, d: f32, c: Color) {
        Self::line(base, [cx, cy - d], [cx + d, cy], c);
        Self::line(base, [cx + d, cy], [cx, cy + d], c);
        Self::line(base, [cx, cy + d], [cx - d, cy], c);
        Self::line(base, [cx - d, cy], [cx, cy - d], c);
    }

    fn stroke_rect(base: &mut PanelBase, rect: [f32; 4], color: Color, radius: f32) {
        base.batch
            .gradient_rect(&mut base.arena, rect, color, color, radius, 0.0, 1.0);
    }

    fn draw_pan_guides(&self, base: &mut PanelBase) {
        let p = base.palette;
        let (fx, fs) = (self.field.min_x(), self.field.w);
        let axis = self.pan_axis_rect();
        let (y0, y1) = (axis.min_y(), axis.max_y());
        let cx = self.pan_center_x();
        let half = self.pan_half();
        for f in LogAxis::LABELED {
            if f >= PAN_AXIS.min_hz && f <= PAN_AXIS.max_hz {
                let k = if f == 20.0 || f == 20_000.0 { 1.8 } else { 1.3 };
                base.hline(
                    fx + self.pan_gutter() - 4.0,
                    fx + fs - 1.0,
                    self.pan_y(f),
                    scaled_alpha(p.grid_minor, k),
                );
            }
        }
        base.vline(cx, y0, y1, scaled_alpha(p.grid_major, 1.5));
        for k in [-1.0f32, -0.5, 0.5, 1.0] {
            let color = if k.abs() == 1.0 {
                p.grid_major
            } else {
                scaled_alpha(p.grid_minor, 1.3)
            };
            base.vline(cx + k * half, y0, y1, color);
        }
    }

    fn corr_color(p: &Palette, c: f32) -> Color {
        if c < 0.0 {
            return p.danger;
        }
        mix(p.accent, p.good, c.max(0.0).min(1.0))
    }

    fn draw_correlation_bar(&self, base: &mut PanelBase, pass: &mut wgpu::RenderPass<'_>) {
        let p = base.palette;
        let b = self.corr_bar;
        if !(b.w > 0.0) {
            return;
        }
        let (x0, y0, w, h) = (b.x, b.y, b.w, b.h);
        let mid = x0 + w / 2.0;
        // Track: the negative half carries a faint red wash.
        base.rounded_rect([x0, y0, w, h], p.track, h / 2.0);
        base.batch.hgradient(
            &mut base.arena,
            [x0, y0, w / 2.0, h],
            with_alpha(p.danger, 0.30),
            with_alpha(p.danger, 0.05),
            h / 2.0,
        );
        let c = clamp1(self.shown_correlation);
        let vx = mid + c * w / 2.0;
        let col = Self::corr_color(&p, c);
        let ticks = |base: &mut PanelBase| {
            for t in [-1.0f32, -0.5, 0.0, 0.5, 1.0] {
                let tx = x0 + (t + 1.0) / 2.0 * w;
                base.vline(
                    tx,
                    y0 + h + 2.0,
                    y0 + h + if t == 0.0 { 7.0 } else { 5.0 },
                    p.grid_strong,
                );
            }
        };
        let wb = self.width_bar;
        let frame = base.frame.clone();
        // Without signal there is no correlation: the track alone, no marker at a made-up zero.
        let Some(f) = frame.filter(|f| !no_signal(f)) else {
            ticks(base);
            if wb.w > 0.0 {
                base.rounded_rect([wb.x, wb.y, wb.w, wb.h], p.track, wb.h / 2.0);
            }
            base.flush(pass);
            return;
        };
        if c >= 0.0 {
            base.batch.hgradient(
                &mut base.arena,
                [mid, y0, (vx - mid).max(0.5), h],
                with_alpha(col, 0.55),
                col,
                1.0,
            );
        } else {
            base.batch.hgradient(
                &mut base.arena,
                [vx, y0, (mid - vx).max(0.5), h],
                col,
                with_alpha(col, 0.55),
                1.0,
            );
        }
        ticks(base);
        // Width: a 0...1 bar. Over 1 (more side than mid) the bar is full and turns to the warning color.
        if wb.w > 0.0 {
            base.rounded_rect([wb.x, wb.y, wb.w, wb.h], p.track, wb.h / 2.0);
            let v = f.stereo.width.max(0.0).min(1.0);
            if v > 0.005 {
                let color = if f.stereo.width > 1.0 { p.warn } else { p.accent };
                base.rounded_rect([wb.x, wb.y, (wb.w * v).max(wb.h), wb.h], color, wb.h / 2.0);
            }
        }
        if self.balance_bar.w > 0.0 {
            // Balance: a dot on a track, like the rows of the band table.
            let bb = self.balance_bar;
            let (bx0, by, bw) = (bb.min_x(), bb.mid_y(), bb.w);
            base.rounded_rect([bx0, by - 1.0, bw, 2.0], p.track, 1.0);
            base.vline(bx0 + bw / 2.0, by - 4.0, by + 4.0, p.grid_strong);
            let bx = bx0 + bw / 2.0 + clamp1(f.stereo.balance) * (bw / 2.0 - 4.0);
            base.circle([bx, by], 3.5, p.accent, 0.0, 0.0);
            base.circle([bx, by], 3.5, [1.0, 1.0, 1.0, 0.85], 1.0, 0.0);
        }
        base.flush(pass);
        let glow = with_alpha(col, 0.5);
        base.batch.gradient_rect(
            &mut base.arena,
            [vx - 5.0, y0 - 4.0, 10.0, h + 8.0],
            glow,
            glow,
            4.0,
            3.0,
            0.0,
        );
        base.flush_add(pass);
        base.rounded_rect([vx - 1.25, y0 - 3.0, 2.5, h + 6.0], WHITE, 1.25);
        base.flush(pass);
    }

    /// Row geometry for the band table (side by side) or column geometry for the strip (stacked).
    /// The table uses the whole height under the correlation bar.
    pub fn band_cell(&self, i: usize) -> Rect {
        let b = self.bands;
        if self.side_by_side {
            let row_h = (b.h / 8.0).min(60.0);
            return Rect::new(b.min_x(), b.min_y() + i as f32 * row_h, b.w, row_h);
        }
        let cw = b.w / 8.0;
        Rect::new(b.min_x() + i as f32 * cw, b.min_y(), cw, b.h)
    }

    fn table_label_width(&self) -> f32 {
        if self.narrow_table {
            50.0
        } else {
            88.0
        }
    }

    fn table_value_width(&self) -> f32 {
        if self.narrow_table {
            0.0
        } else {
            48.0
        }
    }

    /// The correlation track and the balance track of a band cell.
    pub fn band_tracks(&self, cell: Rect) -> (Rect, Rect) {
        if self.side_by_side {
            let (label_w, value_w) = (self.table_label_width(), self.table_value_width());
            let gap = if self.narrow_table { 14.0 } else { 24.0 };
            let tw = ((cell.w - label_w - value_w * 2.0 - gap) / 2.0).max(10.0);
            let c = Rect::new(cell.min_x() + label_w, cell.mid_y() - 3.0, tw, 6.0);
            let b = Rect::new(c.max_x() + value_w + gap, cell.mid_y() - 3.0, tw, 6.0);
            return (c, b);
        }
        let inset = 5.0;
        let c = Rect::new(cell.min_x() + inset, cell.min_y() + 20.0, cell.w - inset * 2.0, 6.0);
        let b = Rect::new(cell.min_x() + inset, cell.min_y() + 38.0, cell.w - inset * 2.0, 6.0);
        (c, b)
    }

    fn draw_bands(&self, base: &mut PanelBase, pass: &mut wgpu::RenderPass<'_>) {
        let Some(f) = base.frame.clone() else { return };
        if !(self.bands.w > 0.0) {
            return;
        }
        let p = base.palette;
        let side = self.side_by_side;
        if let Some(b) = self.cursor_band(base) {
            // The band that holds the cursor frequency: a quiet plate behind its row, an accent edge.
            let cell = self.band_cell(b);
            let r = if side {
                cell.inset(-6.0, 1.0)
            } else {
                cell.inset(1.0, -3.0)
            };
            base.rounded_rect([r.x, r.y, r.w, r.h], with_alpha(p.accent, 0.16), 4.0);
            if side {
                base.rounded_rect([r.x, r.y + 3.0, 2.0, r.h - 6.0], p.accent, 1.0);
            } else {
                base.rounded_rect([r.x + 3.0, r.max_y() - 2.0, r.w - 6.0, 2.0], p.accent, 1.0);
            }
        }
        for i in 0..8 {
            let cell = self.band_cell(i);
            let (tc, tb) = self.band_tracks(cell);
            let corr = clamp1(f.stereo.band_correlation[i]);
            let bal = clamp1(f.stereo.band_balance[i]);
            let hue = rgba(
                Palette::spectrum_color((BandEnergy::EDGES_HZ[i] * BandEnergy::EDGES_HZ[i + 1]).sqrt()),
                1.0,
            );
            // A gated empty band has no measurement: its zeros are placeholders. Show grey tracks only.
            let active = f.stereo.band_active[i];

            // Correlation: bipolar bar from the center.
            let (x0, y0, w, h) = (tc.x, tc.y, tc.w, tc.h);
            let mid = x0 + w / 2.0;
            let track = if active { p.track } else { scaled_alpha(p.track, 0.5) };
            base.rounded_rect([x0, y0, w, h], track, h / 2.0);
            if side && i > 0 {
                base.hline(cell.min_x(), cell.max_x(), cell.min_y(), p.grid_minor);
            }
            let (x0, y0, w, h) = (tb.x, tb.y, tb.w, tb.h);
            if !active {
                base.rounded_rect([x0, y0 + h / 2.0 - 1.0, w, 2.0], scaled_alpha(p.track, 0.5), 1.0);
                // Hollow grey dot: "no value", different in shape from a centered balance.
                base.circle([x0 + w / 2.0, y0 + h / 2.0], 3.0, p.text_faint, 1.0, 0.0);
                continue;
            }
            let vx = mid + corr * tc.w / 2.0;
            let col = Self::corr_color(&p, corr);
            if corr >= 0.0 {
                base.rounded_rect([mid, tc.y, (vx - mid).max(0.5), tc.h], col, 1.0);
            } else {
                base.rounded_rect([vx, tc.y, (mid - vx).max(0.5), tc.h], col, 1.0);
            }
            base.vline(mid, tc.y - 2.0, tc.y + tc.h + 2.0, p.grid_strong);

            // Balance: a dot on a track, tinted by the band's hue.
            let mid = x0 + w / 2.0;
            base.rounded_rect([x0, y0 + h / 2.0 - 1.0, w, 2.0], p.track, 1.0);
            base.vline(mid, y0 - 2.0, y0 + h + 2.0, p.grid_strong);
            let bx = mid + bal * (w / 2.0 - 4.0);
            base.circle([bx, y0 + h / 2.0], 4.0, hue, 0.0, 0.0);
            base.circle([bx, y0 + h / 2.0], 4.0, [1.0, 1.0, 1.0, 0.85], 1.0, 0.0);
        }
        base.flush(pass);
    }

    // MARK: Linked cursor

    /// The listening band under the cursor: its row of the band table is highlighted (in both modes), or, under the
    /// goniometer without a table, the block under the key numbers shows it.
    fn cursor_band(&self, base: &PanelBase) -> Option<usize> {
        if !base.cursor_linked {
            return None;
        }
        cursor::band(base.cursor?.frequency_hz)
    }

    /// Pan of the analyzer bin at the cursor frequency, from the state the field is drawn from. `None` where the
    /// field shows no light (under the gate, or the skirt of a tone): there is no placement to read.
    pub fn cursor_pan(&self, base: &PanelBase) -> Option<f32> {
        if !self.is_pan() {
            return None;
        }
        let c = base.cursor?;
        let f = base.frame.as_ref()?;
        let i = cursor::nearest_bin(&f.spectrum.frequencies, self.pan_field.count(), c.frequency_hz)?;
        let top = self.pan_top_db + 3.0;
        if !(self.pan_field.light(i, top, top - PAN_RANGE_DB) > 0.0) {
            return None;
        }
        Some(clamp1(self.pan_field.pan()[i]))
    }

    /// The block under the key numbers (no band table at this size): the band at the cursor, its correlation and
    /// balance.
    fn band_block(&self, base: &PanelBase) -> Option<Rect> {
        if self.is_pan() || self.cursor_band(base).is_none() || self.arrangement != Arrangement::KeysBeside {
            return None;
        }
        let last = self.key_cells.last()?;
        let r = Rect::new(last.min_x() - 4.0, last.max_y() + 8.0, last.w + 8.0, 36.0);
        (r.max_y() <= base.height - 8.0 && r.w >= 96.0).then_some(r)
    }

    fn draw_linked_cursor(&self, base: &mut PanelBase, pass: &mut wgpu::RenderPass<'_>) {
        let accent = base.palette.accent;
        if let Some(r) = self.band_block(base) {
            base.rounded_rect([r.x, r.y, r.w, r.h], with_alpha(accent, 0.16), 4.0);
            base.rounded_rect([r.x, r.y + 3.0, 2.0, r.h - 6.0], accent, 1.0);
            base.flush(pass);
        }
        let Some(c) = base.cursor.filter(|c| {
            base.cursor_linked
                && self.is_pan()
                && c.frequency_hz >= PAN_AXIS.min_hz
                && c.frequency_hz <= PAN_AXIS.max_hz
        }) else {
            return;
        };
        let yy = self.pan_y(c.frequency_hz);
        base.draw_cursor_horizontal(
            yy,
            self.field.min_x() + self.pan_gutter() - 4.0,
            self.field.max_x() - 1.0,
        );
        if let Some(pan) = self.cursor_pan(base) {
            // Where the sound of this row sits between left and right.
            let mx = self.pan_center_x() + pan * self.pan_half();
            let plate = with_alpha(base.palette.plot, 0.85);
            base.circle([mx, yy], 4.0, plate, 0.0, 0.0);
            base.circle([mx, yy], 4.0, [1.0, 1.0, 1.0, 0.95], 1.2, 0.0);
        }
        base.flush(pass);
    }

    fn draw_cursor_text(&mut self, base: &PanelBase, o: &mut OverlayContext) {
        if !(base.cursor_linked && base.cursor.is_some()) {
            return;
        }
        let p = &base.palette;
        if self.is_pan() {
            if !self.shows_header_readout(base) {
                return;
            }
            let items = self.cursor_items(base);
            header::draw(
                o,
                &items,
                self.field.max_x(),
                self.field.min_x(),
                self.field.min_y() - 9.0,
                true,
                base.cursor.is_some_and(|c| c.is_pinned),
                p,
            );
        } else if let (Some(r), Some(b)) = (self.band_block(base), self.cursor_band(base)) {
            let items = self.cursor_items(base);
            if items.len() != 3 {
                return;
            }
            let cap = fonts::ui(10.0, Weight::Semibold);
            let val = fonts::ui(11.0, Weight::Medium);
            // The name in capitals like the other captions; the unit keeps its case (kHz).
            let long = format!("{} {}", BandEnergy::NAMES[b].to_uppercase(), cursor::band_range(b));
            let name = if o.measure(&long, &cap, 0.6) <= r.w - 10.0 {
                long
            } else {
                format!("{} {}", band_labels::SHORT[b].to_uppercase(), cursor::band_range(b))
            };
            o.text_tracked(
                &name,
                r.min_x() + 6.0,
                r.min_y() + 5.0,
                &cap,
                mix(p.accent, WHITE, 0.55),
                HAlign::Left,
                VAlign::Top,
                0.6,
            );
            let line = format!("{}   {}", items[1].text, items[2].text);
            let shown = if o.measure(&line, &val, 0.0) <= r.w - 10.0 {
                line.as_str()
            } else {
                items[1].text.as_str()
            };
            o.text(
                shown,
                r.min_x() + 6.0,
                r.max_y() - 7.0,
                &val,
                p.text,
                HAlign::Left,
                VAlign::Baseline,
            );
        }
    }
}

impl Panel for VectorscopePanel {
    fn kind(&self) -> PanelKind {
        PanelKind::Vectorscope
    }

    fn as_any(&self) -> &dyn Any {
        self
    }

    fn as_any_mut(&mut self) -> &mut dyn Any {
        self
    }

    fn makes_pointer_cursors(&self) -> bool {
        self.is_pan()
    }

    fn needs_pre_pass_per_frame(&self) -> bool {
        true
    }

    /// The light keeps fading for a while after the last points.
    fn is_animating(&self) -> bool {
        self.since_light < self.persistence_seconds.max(0.05) * 7.0
    }

    fn palette_changed(&mut self, base: &mut PanelBase) {
        self.scope_lut = Some(base.ctx.make_lut(&base.palette.scope_lut(256)));
        let hues: Vec<[f32; 3]> = (0..256)
            .map(|i| Palette::spectrum_color(PAN_AXIS.frequency(i as f32 / 255.0)))
            .collect();
        self.pan_hue = Some(base.ctx.make_lut(&hues));
        if let Some(accum) = &mut self.accum {
            accum.group = None;
        }
    }

    fn layout_changed(&mut self, base: &mut PanelBase) {
        let (w, h) = (base.width, base.height);
        let pad: f32 = if w < 340.0 || h < 240.0 { 8.0 } else { 12.0 };
        let tall_field = self.is_pan();
        let strip = 104.0;
        // Linked stereo placement keeps a header row over the field for the cursor readout (nothing covers the
        // field, and the field does not jump when a cursor comes and goes). The goniometer shows the cursor in its
        // band rows: no row.
        let head = if base.cursor_linked && tall_field { 16.0 } else { 0.0 };
        self.key_cells.clear();
        self.balance_bar = Rect::default();
        // The band table needs about 250 pt beside a figure that takes the card height. The pan field is not a
        // square: it gives way to the table.
        self.arrangement = if w / h > 1.45 && w >= 520.0 && (tall_field || w - (h - pad * 2.0) - 56.0 >= 250.0) {
            Arrangement::TableBeside
        } else if h - pad * 2.0 - strip >= (w - pad * 2.0) * 0.8 {
            Arrangement::StripUnder
        } else if w - (h - pad * 2.0) - pad * 3.0 >= 70.0 {
            Arrangement::KeysBeside
        } else {
            Arrangement::KeysUnder
        };
        self.side_by_side = self.arrangement == Arrangement::TableBeside;
        match self.arrangement {
            Arrangement::TableBeside => {
                let side = (h - pad * 2.0).min(w * 0.46);
                // The pan spectrum is a frequency axis: it takes the whole height. The goniometer is a square.
                self.field = if tall_field {
                    Rect::new(pad, pad + head, side, h - pad * 2.0 - head)
                } else {
                    Rect::new(pad, (h - side) / 2.0, side, side)
                };
                let rx = self.field.max_x() + 28.0;
                let rw = (w - rx - pad - 4.0).max(60.0);
                self.narrow_table = rw < 330.0;
                self.corr_header = [rx, self.field.min_y() - head + 2.0];
                self.corr_bar = Rect::new(rx, self.corr_header[1] + 68.0, rw, 10.0);
                let ww = (rw * 0.24).min(96.0);
                self.width_bar = Rect::new(
                    rx + rw - ww - if self.narrow_table { 62.0 } else { 104.0 },
                    self.corr_header[1] + 47.0,
                    ww,
                    4.0,
                );
                let top = self.corr_bar.max_y() + 54.0;
                self.bands = Rect::new(rx, top, rw, (self.field.max_y() - top).max(40.0));
            }
            Arrangement::StripUnder => {
                self.narrow_table = true;
                let side = (w - pad * 2.0).min(h - strip - pad * 2.0).max(40.0);
                self.field = if tall_field {
                    Rect::new(pad, pad + head, w - pad * 2.0, (h - strip - pad * 2.0 - head).max(40.0))
                } else {
                    Rect::new((w - side) / 2.0, pad, side, side)
                };
                let rx = pad + 2.0;
                let rw = w - rx * 2.0;
                self.corr_header = [rx, self.field.max_y() + 16.0];
                self.corr_bar = Rect::new(rx + 44.0, self.field.max_y() + 18.0, (rw - 44.0 - 58.0).max(40.0), 8.0);
                self.width_bar = Rect::default();
                self.bands = Rect::new(
                    rx,
                    self.corr_bar.max_y() + 18.0,
                    rw,
                    (h - (self.corr_bar.max_y() + 22.0) - pad).max(30.0),
                );
            }
            Arrangement::KeysBeside => {
                // The figure takes the card height. Correlation, width and balance stand in a column beside it.
                self.narrow_table = true;
                let side = h - pad * 2.0;
                let column = if tall_field {
                    (w * 0.24).max(70.0).min(120.0)
                } else {
                    (w - side - pad * 3.0).min(150.0)
                };
                self.field = if tall_field {
                    Rect::new(pad, pad + head, w - column - pad * 3.0, side - head)
                } else {
                    Rect::new(pad, pad, side, side)
                };
                let x = self.field.max_x() + pad + 2.0;
                let cw = w - x - pad;
                let cell_h = ((h - pad * 2.0) / 3.0).min(64.0);
                let top = self.field.mid_y() - cell_h * 1.5;
                self.key_cells = (0..3)
                    .map(|i| Rect::new(x, top + i as f32 * cell_h, cw, cell_h))
                    .collect();
                let bars = cell_h >= 52.0;
                let bar = |cell: Rect, dy: f32, height: f32| {
                    if bars {
                        Rect::new(x, cell.min_y() + dy, cw, height)
                    } else {
                        Rect::default()
                    }
                };
                self.corr_bar = bar(self.key_cells[0], 40.0, 5.0);
                self.width_bar = bar(self.key_cells[1], 40.0, 4.0);
                self.balance_bar = bar(self.key_cells[2], 39.0, 6.0);
                self.corr_header = [self.key_cells[0].x, self.key_cells[0].y];
                self.bands = Rect::default();
            }
            Arrangement::KeysUnder => {
                self.narrow_table = true;
                let row = 30.0;
                let side = (w - pad * 2.0).min(h - pad * 2.0 - row).max(40.0);
                self.field = if tall_field {
                    Rect::new(pad, pad + head, w - pad * 2.0, (h - pad * 2.0 - row - head).max(40.0))
                } else {
                    Rect::new((w - side) / 2.0, pad, side, side)
                };
                let cw = (w - pad * 2.0) / 3.0;
                let y = self.field.max_y() + 6.0;
                self.key_cells = (0..3)
                    .map(|i| Rect::new(pad + i as f32 * cw, y, cw, row - 6.0))
                    .collect();
                self.corr_bar = Rect::default();
                self.width_bar = Rect::default();
                self.corr_header = [self.key_cells[0].x, self.key_cells[0].y];
                self.bands = Rect::default();
            }
        }
        let px = ((self.field.w * base.scale) as u32).max(16);
        if !tall_field && !self.accum.as_ref().is_some_and(|a| a.width == px) {
            self.accum = Some(Self::make_accum(
                base,
                SCOPE_ACCUM_FORMAT,
                px,
                px,
                "Joseon scope density",
            ));
            self.accum_needs_clear = true;
        }
        let py = ((self.field.h * base.scale) as u32).max(16);
        if tall_field && !self.pan_accum.as_ref().is_some_and(|a| a.width == px && a.height == py) {
            let mut accum = Self::make_accum(base, PAN_ACCUM_FORMAT, px, py, "Joseon pan spectrum light");
            accum.group = Some(base.ctx.device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("pan spectrum light"),
                layout: &base.ctx.lut_layout,
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: wgpu::BindingResource::TextureView(&accum.view),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: wgpu::BindingResource::Sampler(&base.ctx.linear_sampler),
                    },
                ],
            }));
            self.pan_accum = Some(accum);
            self.accum_needs_clear = true;
        }
        if !tall_field {
            self.pan_accum = None;
        }
    }

    fn static_signature(&mut self, base: &PanelBase) -> u64 {
        let mut h = Signature::new();
        h.add(base.static_signature()).add(self.is_pan());
        h.finish()
    }

    fn dynamic_signature(&self, base: &PanelBase) -> u64 {
        let Some(f) = &base.frame else { return 0 };
        let mut h = Signature::new();
        h.add((f.stereo.correlation * 100.0) as i32)
            .add((f.stereo.width * 100.0) as i32)
            .add((f.stereo.balance * 100.0) as i32)
            .add((self.gain * 10.0) as i32)
            .add((self.pan_top_db / 2.0).round() as i32)
            .add(f.stereo.band_active);
        for v in f.stereo.band_correlation {
            h.add((v * 100.0) as i32);
        }
        for v in f.stereo.band_balance {
            h.add((v * 100.0) as i32);
        }
        h.finish()
    }

    fn update(&mut self, _base: &mut PanelBase, f: &AnalysisFrame, dt: f64) {
        self.pending_fade += dt;
        let pts = &f.stereo.scope_points;
        self.pending_points = !f.is_silent && (self.is_pan() || !pts.is_empty());
        if self.pending_points {
            self.since_light = 0.0;
        } else {
            self.since_light += dt;
        }
        self.update_gain(pts, dt);
        let kc = (1.0 - (-dt / 0.12).exp()) as f32;
        self.shown_correlation += (f.stereo.correlation - self.shown_correlation) * kc;

        // Pan spectrum: the top of the light scale follows the loudest bin. Fast up, slow down.
        let mut peak = -120.0f32;
        let mut i = 0;
        let mid = &f.spectrum.mid;
        while i < mid.len() {
            peak = peak.max(mid[i]);
            i += 3;
        }
        if peak > -100.0 {
            let k = (1.0 - (-dt / if peak > self.pan_top_db { 0.15 } else { 2.5 }).exp()) as f32;
            self.pan_top_db += (peak - self.pan_top_db) * k;
        }
        if self.is_pan() {
            let sample_rate = f.stream.as_ref().map_or(48_000.0, |s| s.sample_rate);
            self.pan_field.update(&f.spectrum, dt, sample_rate);
        }
    }

    fn idle(&mut self, _base: &mut PanelBase, dt: f64) {
        self.pending_fade += dt;
        self.since_light += dt;
    }

    fn encode_pre_pass(&mut self, base: &mut PanelBase, encoder: &mut wgpu::CommandEncoder) {
        if self.is_pan() {
            self.encode_pan_pre_pass(base, encoder);
        } else {
            self.encode_scope_pre_pass(base, encoder);
        }
    }

    fn draw(&mut self, base: &mut PanelBase, pass: &mut wgpu::RenderPass<'_>) {
        let p = base.palette;
        let ctx = base.ctx.clone();
        let (fx, fy, fs, fh) = (self.field.x, self.field.y, self.field.w, self.field.h);

        // Field.
        base.gradient_rect([fx, fy, fs, fh], mix(p.plot, p.panel, 0.5), p.plot, 6.0);
        Self::stroke_rect(base, [fx, fy, fs, fh], p.grid_major, 6.0);
        if self.is_pan() {
            self.draw_pan_guides(base);
        } else {
            self.draw_scope_guides(base);
        }
        base.flush(pass);

        // The light.
        let composite = |k: f32, reference: f32| CompositeUniforms {
            rect: [fx, fy, fs, fh],
            k,
            norm: 1.0 / (1.0 + k * reference).ln(),
            pad: [0.0; 2],
        };
        if self.is_pan() {
            if let Some(group) = self.pan_accum.as_ref().and_then(|a| a.group.as_ref()) {
                // The light is clipped to the frequency axis (20 Hz ... 20 kHz): the splat of a sub-bass bin is as
                // high as its FFT bin, and its glow would stand under the 20 Hz line, where the axis has no frequency.
                let sc = base.scale;
                let clip = self.pan_axis_rect();
                let cy = ((clip.min_y() * sc).round() as i64).max(0);
                let cx = ((fx * sc).round() as i64).max(0);
                let full_w = ((base.width * sc) as i64).max(1);
                let full_h = ((base.height * sc) as i64).max(1);
                pass.set_scissor_rect(
                    cx.min(full_w - 1) as u32,
                    cy.min(full_h - 1) as u32,
                    ((fs * sc).round() as i64).min(full_w - cx).max(1) as u32,
                    ((clip.h * sc).round() as i64).min(full_h - cy).max(1) as u32,
                );
                if let Some(offset) = base.arena.push_uniform(&composite(30.0, 5.0)) {
                    pass.set_pipeline(&ctx.pan_composite);
                    pass.set_bind_group(0, &base.arena.globals_group, &[]);
                    pass.set_bind_group(1, &base.arena.uniform_group, &[offset]);
                    pass.set_bind_group(2, group, &[]);
                    pass.draw(0..4, 0..1);
                }
                pass.set_scissor_rect(0, 0, full_w as u32, full_h as u32);
            }
        } else if let (Some(accum), Some(lut)) = (&mut self.accum, &self.scope_lut) {
            let group = accum.group.get_or_insert_with(|| {
                let lut_view = lut.texture.create_view(&Default::default());
                ctx.device.create_bind_group(&wgpu::BindGroupDescriptor {
                    label: Some("scope density"),
                    layout: &ctx.history_layout,
                    entries: &[
                        wgpu::BindGroupEntry {
                            binding: 0,
                            resource: wgpu::BindingResource::TextureView(&accum.view),
                        },
                        wgpu::BindGroupEntry {
                            binding: 1,
                            resource: wgpu::BindingResource::Sampler(&ctx.linear_sampler),
                        },
                        wgpu::BindGroupEntry {
                            binding: 2,
                            resource: wgpu::BindingResource::TextureView(&lut_view),
                        },
                        wgpu::BindGroupEntry {
                            binding: 3,
                            resource: wgpu::BindingResource::Sampler(&ctx.linear_sampler),
                        },
                    ],
                })
            });
            // log(1 + k d) / log(1 + k dRef): the reference density is far over what a music trace reaches in its
            // core, so the ramp's warm white end shows only where the beam really dwells.
            if let Some(offset) = base.arena.push_uniform(&composite(22.0, 60.0)) {
                pass.set_pipeline(&ctx.scope_composite);
                pass.set_bind_group(0, &base.arena.globals_group, &[]);
                pass.set_bind_group(1, &base.arena.uniform_group, &[offset]);
                pass.set_bind_group(2, &*group, &[]);
                pass.draw(0..4, 0..1);
            }
        }

        if let Some(legend) = self.pan_legend_rect().filter(|_| self.is_pan()) {
            // Intensity legend, hue-coded like the plot: the light scale from its bottom to its top in three hues of
            // the frequency axis (highs on top, like the axis). Hue = frequency, brightness = level.
            let (lx, ly, lw, lh) = (legend.x, legend.y, legend.w, legend.h);
            // One step per device pixel: a smooth ramp, no bands.
            let steps = pan_legend_steps(legend.w, base.scale);
            let hues = [9_000.0f32, 700.0, 60.0];
            let row_h = lh / hues.len() as f32;
            for (row, hz) in hues.iter().enumerate() {
                let hue = rgba(Palette::spectrum_color(*hz), 1.0);
                for k in 0..steps {
                    let a = (k as f32 + 0.5) / steps as f32;
                    let v = 0.05 + 0.89 * a * a.sqrt();
                    // The brightest end drifts toward warm white, as the dense cores of the plot do.
                    let c = mix(hue, [1.0, 0.96, 0.90, 1.0], (a - 0.72).max(0.0) / 0.28 * 0.6);
                    base.rect(
                        [
                            lx + lw * k as f32 / steps as f32,
                            ly + row_h * row as f32,
                            lw / steps as f32 + 0.5,
                            row_h + 0.25,
                        ],
                        [c[0] * v, c[1] * v, c[2] * v, 1.0],
                    );
                }
            }
            Self::stroke_rect(base, [lx - 0.5, ly - 0.5, lw + 1.0, lh + 1.0], p.grid_major, 1.5);
            // Middle tick; its number is in the text layer.
            base.vline(lx + lw / 2.0, ly + lh, ly + lh + 3.0, p.grid_strong);
            base.flush(pass);
        }

        self.draw_linked_cursor(base, pass);
        self.draw_correlation_bar(base, pass);
        self.draw_bands(base, pass);
    }

    // MARK: Text

    // 0.7071 is the value of the macOS app.
    #[allow(clippy::approx_constant)]
    fn draw_static(&mut self, base: &PanelBase, o: &mut OverlayContext) {
        let p = &base.palette;
        let f = fonts::ui(11.0, Weight::Semibold);
        let field = self.field;
        let (cx, cy) = (field.mid_x(), field.mid_y());
        if self.is_pan() {
            let small = fonts::mono(if field.w < 300.0 { 10.0 } else { 11.0 }, Weight::Regular);
            let (half, pcx) = (self.pan_half(), self.pan_center_x());
            let y = field.min_y() + 7.0;
            o.text("L", pcx - half, y, &f, p.left, HAlign::Center, VAlign::Top);
            o.text("R", pcx + half, y, &f, p.right, HAlign::Center, VAlign::Top);
            o.text("C", pcx, y, &f, p.text_dim, HAlign::Center, VAlign::Top);
            let label_x = field.min_x() + self.pan_gutter() - 7.0;
            o.text("Hz", label_x, y, &small, p.text_faint, HAlign::Right, VAlign::Top);
            // Every labeled frequency from 20 Hz to 20 kHz; the ends always, the rest where there is room.
            let mut taken: Vec<f32> = Vec::new();
            let inner = LogAxis::LABELED.into_iter().filter(|hz| *hz > 20.0 && *hz < 20_000.0);
            for hz in [20.0, 20_000.0].into_iter().chain(inner) {
                let yy = self.pan_y(hz);
                if taken.iter().any(|t| (t - yy).abs() < 13.0) {
                    continue;
                }
                taken.push(yy);
                o.text(
                    &fmt::axis_hz(hz),
                    label_x,
                    yy,
                    &small,
                    p.text_dim,
                    HAlign::Right,
                    VAlign::Middle,
                );
            }
        } else {
            let r = field.w / 2.0 * DIAMOND;
            // Under 160 pt the labels would pile up on the figure: the diamond alone still reads.
            if field.w >= 160.0 {
                // Labels stand 10 pt outside the diamond: the trace never runs through them.
                // "M" stands between the tip and the border: under 230 pt there is no room for it there.
                if field.w >= 230.0 {
                    o.text("M", cx, cy - r - 5.0, &f, p.text_dim, HAlign::Center, VAlign::Bottom);
                }
                let off = r / 2.0 + 10.0 * 0.7071 + 3.0;
                o.text("L", cx - off, cy - off, &f, p.left, HAlign::Right, VAlign::Bottom);
                o.text("R", cx + off, cy - off, &f, p.right, HAlign::Left, VAlign::Bottom);
                o.text(
                    "+S",
                    field.min_x() + 7.0,
                    cy - 6.0,
                    &f,
                    p.text_faint,
                    HAlign::Left,
                    VAlign::Bottom,
                );
                let minus_s = format!("{}S", fmt::MINUS);
                o.text(
                    &minus_s,
                    field.max_x() - 7.0,
                    cy - 6.0,
                    &f,
                    p.text_faint,
                    HAlign::Right,
                    VAlign::Bottom,
                );
            }
        }

        let small = fonts::mono(11.0, Weight::Regular);
        if self.side_by_side {
            for t in [-1.0f32, 0.0, 1.0] {
                let tx = self.corr_bar.min_x() + (t + 1.0) / 2.0 * self.corr_bar.w;
                let (label, h) = if t == 0.0 {
                    ("0".to_string(), HAlign::Center)
                } else if t < 0.0 {
                    (format!("{}1", fmt::MINUS), HAlign::Left)
                } else {
                    ("+1".to_string(), HAlign::Right)
                };
                o.text(
                    &label,
                    tx,
                    self.corr_bar.max_y() + 10.0,
                    &small,
                    p.text_dim,
                    h,
                    VAlign::Top,
                );
            }
        }

        let cap = fonts::ui(11.0, Weight::Semibold);
        let bands = self.bands;
        if self.side_by_side {
            let [hx, hy] = self.corr_header;
            o.text_tracked(
                "CORRELATION",
                hx,
                hy,
                &cap,
                p.text_faint,
                HAlign::Left,
                VAlign::Top,
                1.0,
            );
            let (first_corr, first_bal) = self.band_tracks(self.band_cell(0));
            let y = bands.min_y() - 7.0;
            let narrow = self.narrow_table;
            o.text_tracked(
                "BAND",
                bands.min_x(),
                y,
                &cap,
                p.text_faint,
                HAlign::Left,
                VAlign::Bottom,
                1.0,
            );
            o.text_tracked(
                if narrow { "CORR" } else { "CORRELATION" },
                first_corr.min_x(),
                y,
                &cap,
                p.text_faint,
                HAlign::Left,
                VAlign::Bottom,
                1.0,
            );
            o.text_tracked(
                if narrow { "BAL" } else { "BALANCE" },
                first_bal.min_x(),
                y,
                &cap,
                p.text_faint,
                HAlign::Left,
                VAlign::Bottom,
                1.0,
            );
            let name_font = fonts::ui(if narrow { 11.0 } else { 12.0 }, Weight::Regular);
            for i in 0..8 {
                let cell = self.band_cell(i);
                let name = if narrow {
                    band_labels::SHORT[i]
                } else {
                    BandEnergy::NAMES[i]
                };
                o.text(
                    name,
                    cell.min_x(),
                    cell.mid_y(),
                    &name_font,
                    p.text,
                    HAlign::Left,
                    VAlign::Middle,
                );
            }
        } else if !self.key_cells.is_empty() {
            let small = fonts::ui(10.0, Weight::Semibold);
            for (i, name) in ["CORR", "WIDTH", "BAL"].into_iter().enumerate() {
                let cell = self.key_cells[i];
                if self.arrangement == Arrangement::KeysBeside {
                    let (x, y) = (cell.min_x(), cell.min_y() + 3.0);
                    o.text_tracked(name, x, y, &small, p.text_faint, HAlign::Left, VAlign::Top, 0.8);
                } else {
                    let (x, y) = (cell.min_x() + 2.0, cell.mid_y());
                    o.text_tracked(name, x, y, &small, p.text_faint, HAlign::Left, VAlign::Middle, 0.6);
                }
            }
        } else {
            let [hx, _] = self.corr_header;
            let y = self.corr_bar.mid_y();
            o.text_tracked("CORR", hx, y, &cap, p.text_faint, HAlign::Left, VAlign::Middle, 1.0);
            let name_font = fonts::ui(if bands.w / 8.0 < 40.0 { 10.0 } else { 11.0 }, Weight::Regular);
            let names = band_labels::fitting(o, &name_font, bands.w / 8.0, 6.0, base.width);
            for i in 0..8 {
                let cell = self.band_cell(i);
                let (x, y) = (cell.mid_x(), cell.min_y() + 2.0);
                o.text(names[i], x, y, &name_font, p.text_dim, HAlign::Center, VAlign::Top);
            }
        }
    }

    fn draw_dynamic(&mut self, base: &mut PanelBase, o: &mut OverlayContext) {
        let Some(f) = base.frame.clone() else { return };
        let p = base.palette;
        let c = f.stereo.correlation;
        let silent = no_signal(&f);
        let text = if silent {
            fmt::DASH.to_string()
        } else {
            fmt::number(c, 2, true)
        };
        let col = if silent {
            p.text_dim
        } else {
            mix(Self::corr_color(&p, c), WHITE, 0.25)
        };
        let width_text = if silent {
            fmt::DASH.to_string()
        } else {
            fmt::number(f.stereo.width, 2, false)
        };
        let base_line = VAlign::Baseline;
        if self.side_by_side {
            let [hx, hy] = self.corr_header;
            let narrow = self.narrow_table;
            let big = fonts::ui(if narrow { 24.0 } else { 30.0 }, Weight::Light);
            o.text(&text, hx, hy + 44.0, &big, col, HAlign::Left, base_line);
            // Width and balance, right aligned on the same baseline.
            let cap = fonts::ui(11.0, Weight::Semibold);
            let val = fonts::ui(15.0, Weight::Regular);
            let rx = self.corr_bar.max_x();
            let wb = self.width_bar;
            o.text_tracked(
                if narrow { "BAL" } else { "BALANCE" },
                rx,
                hy,
                &cap,
                p.text_faint,
                HAlign::Right,
                VAlign::Top,
                1.0,
            );
            let balance = balance_text(f.stereo.balance, silent);
            o.text(&balance, rx, hy + 38.0, &val, p.text, HAlign::Right, base_line);
            o.text_tracked(
                "WIDTH",
                wb.max_x(),
                hy,
                &cap,
                p.text_faint,
                HAlign::Right,
                VAlign::Top,
                1.0,
            );
            let width_color = if f.stereo.width > 1.0 { p.warn } else { p.text };
            o.text(
                &width_text,
                wb.max_x(),
                hy + 38.0,
                &val,
                width_color,
                HAlign::Right,
                base_line,
            );
            if !narrow {
                let tiny = fonts::mono(11.0, Weight::Regular);
                // The scale of the bar, at its ends.
                o.text(
                    "0",
                    wb.min_x() - 5.0,
                    wb.mid_y(),
                    &tiny,
                    p.text_faint,
                    HAlign::Right,
                    VAlign::Middle,
                );
                o.text(
                    "1",
                    wb.max_x() + 5.0,
                    wb.mid_y(),
                    &tiny,
                    p.text_faint,
                    HAlign::Left,
                    VAlign::Middle,
                );

                for i in 0..8 {
                    let (tc, tb) = self.band_tracks(self.band_cell(i));
                    let empty = silent || !f.stereo.band_active[i];
                    let color = if empty { p.text_faint } else { p.text_dim };
                    let corr = if empty {
                        fmt::DASH.to_string()
                    } else {
                        fmt::number(f.stereo.band_correlation[i], 2, true)
                    };
                    o.text(
                        &corr,
                        tc.max_x() + 44.0,
                        tc.mid_y(),
                        &tiny,
                        color,
                        HAlign::Right,
                        VAlign::Middle,
                    );
                    let bal = balance_text(f.stereo.band_balance[i], empty);
                    o.text(
                        &bal,
                        tb.max_x() + 44.0,
                        tb.mid_y(),
                        &tiny,
                        color,
                        HAlign::Right,
                        VAlign::Middle,
                    );
                }
            }
        } else if !self.key_cells.is_empty() {
            // The three key numbers: correlation, width, balance.
            let values = [text, width_text, balance_text(f.stereo.balance, silent)];
            let colors = [
                col,
                if f.stereo.width > 1.0 && !silent {
                    p.warn
                } else {
                    p.text
                },
                p.text,
            ];
            for i in 0..3 {
                let cell = self.key_cells[i];
                if self.arrangement == Arrangement::KeysBeside {
                    let font = if i == 0 {
                        fonts::ui(19.0, Weight::Light)
                    } else {
                        fonts::ui(16.0, Weight::Regular)
                    };
                    o.text(
                        &values[i],
                        cell.min_x(),
                        cell.min_y() + 33.0,
                        &font,
                        colors[i],
                        HAlign::Left,
                        base_line,
                    );
                } else {
                    let font = fonts::ui(13.0, Weight::Regular);
                    let (x, y) = (cell.max_x() - 8.0, cell.mid_y());
                    o.text(&values[i], x, y, &font, colors[i], HAlign::Right, VAlign::Middle);
                }
            }
        } else {
            let font = fonts::ui(15.0, Weight::Regular);
            let (x, y) = (self.corr_bar.max_x() + 54.0, self.corr_bar.mid_y());
            o.text(&text, x, y, &font, col, HAlign::Right, VAlign::Middle);
        }
        if let Some(legend) = self.pan_legend_rect().filter(|_| self.is_pan()) {
            let tiny = fonts::mono(10.0, Weight::Regular);
            let top = self.pan_top_db + 3.0;
            let bottom = (top - PAN_RANGE_DB).max(self.pan_field.gate_db);
            let lo = (bottom / 2.0).round() * 2.0;
            let hi = (top / 2.0).round() * 2.0;
            let y = legend.mid_y();
            let faint = p.text_faint;
            o.text(
                &fmt::number(lo, 0, false),
                legend.min_x() - 4.0,
                y,
                &tiny,
                faint,
                HAlign::Right,
                VAlign::Middle,
            );
            o.text(
                &fmt::number(hi, 0, false),
                legend.max_x() + 4.0,
                y,
                &tiny,
                faint,
                HAlign::Left,
                VAlign::Middle,
            );
            let middle = fmt::number(((lo + hi) / 2.0).round(), 0, false);
            o.text(
                &middle,
                legend.mid_x(),
                legend.max_y() + 4.0,
                &tiny,
                faint,
                HAlign::Center,
                VAlign::Top,
            );
        }
        self.draw_cursor_text(base, o);
        // No gain label over an empty scope: it says how the figure on screen was scaled, and there is none.
        if !self.is_pan()
            && (self.gain - 1.0).abs() > 0.05
            && !silent
            && !f.is_silent
            && self.held_level > 0.004
            && self.field.w >= 120.0
        {
            let font = fonts::mono(if self.field.w < 260.0 { 10.0 } else { 11.0 }, Weight::Regular);
            o.text(
                &format!("gain \u{00D7}{:.1}", self.gain),
                self.field.max_x() - 8.0,
                self.field.max_y() - 8.0,
                &font,
                p.text_faint,
                HAlign::Right,
                VAlign::Bottom,
            );
        }
    }

    // MARK: Linked cursor

    fn cursor_change_shows(&self, base: &PanelBase, old: Option<PanelCursor>) -> bool {
        if self.is_pan() {
            return true;
        }
        let band = |c: Option<PanelCursor>| c.and_then(|c| cursor::band(c.frequency_hz));
        band(old) != band(base.cursor)
    }

    fn combine_cursor_signature(&self, base: &PanelBase, c: &PanelCursor, h: &mut Signature) {
        if self.is_pan() {
            h.add_f32(c.frequency_hz)
                .add(c.seconds_ago.map(f64::to_bits))
                .add(c.is_pinned)
                .add(c.source)
                .add(self.shows_pointer_readout(base));
        } else {
            h.add(cursor::band(c.frequency_hz));
        }
    }

    fn cursor_at(&self, _base: &PanelBase, point: [f32; 2]) -> Option<PanelCursor> {
        let axis = self.pan_axis_rect();
        let inside = self.is_pan()
            && self.field.contains_point(point[0], point[1])
            && point[1] >= axis.min_y()
            && point[1] <= axis.max_y();
        inside.then(|| {
            PanelCursor::new(
                PAN_AXIS.frequency((axis.max_y() - point[1]) / axis.h),
                PanelKind::Vectorscope,
            )
        })
    }

    fn is_on_cursor(&self, base: &PanelBase, point: [f32; 2]) -> bool {
        match base.cursor {
            Some(c) if self.is_pan() && self.field.contains_point(point[0], point[1]) => {
                (self.pan_y(c.frequency_hz) - point[1]).abs() <= 5.0
            }
            _ => false,
        }
    }

    fn cursor_items(&mut self, base: &PanelBase) -> Vec<CursorReadoutItem> {
        let (Some(c), Some(f)) = (base.cursor, base.frame.as_ref()) else {
            return Vec::new();
        };
        if !self.is_pan() {
            let Some(b) = cursor::band(c.frequency_hz) else {
                return Vec::new();
            };
            let st = &f.stereo;
            let active = !no_signal(f) && st.band_active[b];
            let corr = if active {
                fmt::number(st.band_correlation[b], 2, true)
            } else {
                fmt::DASH.to_string()
            };
            return vec![
                CursorReadoutItem::new(
                    format!("{} {}", BandEnergy::NAMES[b], cursor::band_range(b)),
                    0,
                    Tone::Value,
                ),
                CursorReadoutItem::new(format!("corr {corr}"), 1, Tone::Value),
                CursorReadoutItem::new(
                    format!("bal {}", balance_text(st.band_balance[b], !active)),
                    2,
                    Tone::Value,
                ),
            ];
        }
        // In this panel the pan is the answer: it outranks the note.
        let mut items = vec![CursorReadoutItem::new(cursor::hz(c.frequency_hz), 0, Tone::Value)];
        if let Some(n) = cursor::note(c.frequency_hz) {
            items.push(CursorReadoutItem::new(n, 3, Tone::Dim));
        }
        let pan = self
            .cursor_pan(base)
            .map_or(fmt::DASH.to_string(), |pan| balance_text(pan, false));
        items.push(CursorReadoutItem::new(format!("pan {pan}"), 1, Tone::Value));
        if let Some(m) = cursor::level(&f.spectrum.mid, &f.spectrum.frequencies, c.frequency_hz) {
            items.push(CursorReadoutItem::new(
                format!("Mid {} dB", fmt::db(m, 1, false)),
                2,
                Tone::Dim,
            ));
        }
        items
    }

    /// Cursor readout of the pan spectrum: frequency and pan position under the cursor.
    fn hover_label(&mut self, base: &PanelBase) -> Option<(Vec<String>, [f32; 2])> {
        if !self.is_pan() {
            return None;
        }
        let hv = base.hover.filter(|hv| self.field.contains_point(hv[0], hv[1]))?;
        if base.cursor_linked {
            if !self.shows_pointer_readout(base) {
                return None;
            }
            let c = base.cursor?;
            let mut first = cursor::hz(c.frequency_hz);
            if let Some(n) = cursor::note(c.frequency_hz) {
                first += &format!("   {n}");
            }
            let rest: Vec<String> = self
                .cursor_items(base)
                .into_iter()
                .filter(|item| item.rank == 1 || item.rank == 2)
                .map(|item| item.text)
                .collect();
            return Some((vec![first, rest.join("   ")], hv));
        }
        let axis = self.pan_axis_rect();
        let hz = PAN_AXIS.frequency(((axis.max_y() - hv[1]) / axis.h).max(0.0).min(1.0));
        let pan = (hv[0] - self.pan_center_x()) / self.pan_half();
        let mut first = format!("cursor  {}", fmt::hz(hz));
        if let Some((name, _)) = fmt::note(hz) {
            first += &format!("   {name}");
        }
        Some((
            vec![first, format!("cursor  pan {}", balance_text(clamp1(pan), false))],
            hv,
        ))
    }

    fn accessibility_label(&self) -> String {
        if self.is_pan() {
            "Stereo placement by frequency".to_string()
        } else {
            "Stereo vectorscope".to_string()
        }
    }

    fn accessibility_value(&self, base: &PanelBase) -> String {
        let Some(f) = base.frame.as_ref().filter(|f| !no_signal(f)) else {
            return "No signal".to_string();
        };
        let b = f.stereo.balance;
        let bal = if b.abs() < 0.005 {
            "centered".to_string()
        } else {
            format!(
                "{} percent {}",
                (b.abs() * 100.0).round() as i32,
                if b < 0.0 { "left" } else { "right" }
            )
        };
        format!(
            "Correlation {:+.2}, width {:.2}, balance {bal}",
            f.stereo.correlation, f.stereo.width
        )
    }
}
