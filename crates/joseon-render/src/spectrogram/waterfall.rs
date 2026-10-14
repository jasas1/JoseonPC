//! The spectrogram as a 3-D field: frequency across, time into the depth, level as height. A second
//! mode of the spectrogram panel; the same ring texture, the same dB scale, the same cursor.
//!
//! Port source: `Sources/JoseonRender/SpectrogramRenderer+Waterfall.swift` of the macOS repo.
//!
//! The ring is folded into a small field texture (max over the rows a column covers, mean over the
//! columns a row covers), then one box per cell is drawn, instanced, with depth and four samples per
//! pixel, into a picture of the plot's size. The main pass puts that picture into the plot. The camera
//! sways at rest, tilts a little toward the pointer, orbits freely with a drag, zooms, and resets.

use super::{ring_value, SpectrogramPanel, COLUMNS, ROWS};
use crate::axis::LogAxis;
use crate::context::{Lut, RenderContext, FIELD_FORMAT, TARGET_FORMAT, WATERFALL_DEPTH_FORMAT, WATERFALL_SAMPLES};
use crate::contracts::{PanelCursor, PanelKind};
use crate::fmt;
use crate::gpu::Color;
use crate::panel::PanelBase;
use crate::text::fonts::{self, Weight};
use crate::text::{HAlign, OverlayContext, Rect, VAlign};
use bytemuck::{Pod, Zeroable};
use std::f32::consts::PI;

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct WaterfallSampleUniforms {
    head: f32,
    columns: f32,
    rows: f32,
    bins: f32,
    depth: f32,
    columns_per_row: f32,
    pad: [f32; 2],
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct WaterfallUniforms {
    view_proj: [[f32; 4]; 4],
    fog: [f32; 4],
    floor_v: f32,
    top_v: f32,
    bins: f32,
    depth: f32,
    height: f32,
    depth_step: f32,
    hover_bin: f32,
    hover_row: f32,
    hue_lo: f32,
    hue_hi: f32,
    fog_start: f32,
    contrast: f32,
}

/// The GPU side of the field: the folded texture and the pictures.
struct Gpu {
    field: wgpu::Texture,
    field_view: wgpu::TextureView,
    /// bins x depth of the field texture.
    field_size: (u32, u32),
    color: wgpu::Texture,
    color_view: wgpu::TextureView,
    msaa_color_view: wgpu::TextureView,
    msaa_depth_view: wgpu::TextureView,
    pixel_size: (u32, u32),
    /// The ring for the fold pass (group 2).
    fold_group: wgpu::BindGroup,
    /// The field and the hue LUT for the bars (group 2).
    bars_group: wgpu::BindGroup,
    /// The picture for the blit into the plot (group 2 of the blit pipeline).
    blit_group: wgpu::BindGroup,
}

/// Everything the waterfall adds to the spectrogram panel.
pub struct WaterfallState {
    /// Frequency columns of the field.
    pub bins: usize,
    /// Time rows of the field: the whole history, newest in front.
    pub depth: usize,
    gpu: Option<Gpu>,
    hue_lut: Option<Lut>,
    /// The camera: a slow sway, the pointer tilts it a little, a drag orbits it, a scroll zooms it.
    pub yaw: f32,
    pub elevation: f32,
    tilt: [f32; 2],
    tilt_target: [f32; 2],
    clock: f64,
    /// The view the user turned to. `None` = the resting view with its sway.
    pub user_view: Option<(f32, f32)>,
    pub zoom: f32,
    /// False when the dB scale's numbers would land on other labels (a view from the side).
    scale_shown: bool,
    view_proj: Mat4,
    inv_view_proj: Mat4,
    /// World units between two time rows; the cells are one unit wide.
    pub depth_step: f32,
    /// World units of a bar at the top of the scale. A low plot gets shorter bars.
    pub height: f32,
    /// The camera's angle over the floor at rest: flatter for a low, wide plot.
    base_elevation: f32,
    distance: f32,
    pub reduce_motion: bool,
}

impl WaterfallState {
    pub const FIELD_OF_VIEW: f32 = 0.72;
    pub const TALL_ELEVATION: f32 = 0.56;
    pub const LOW_ELEVATION: f32 = 0.40;
    pub const SWAY_AMPLITUDE: f32 = 3.5 * PI / 180.0;
    pub const SWAY_PERIOD: f64 = 60.0;
    pub const TILT_YAW: f32 = 12.0 * PI / 180.0;
    pub const TILT_ELEVATION: f32 = 4.0 * PI / 180.0;
    pub const REST_YAW: f32 = -14.0 * PI / 180.0;
    pub const ELEVATION_RANGE: (f32, f32) = (0.10, 1.48);
    pub const ZOOM_RANGE: (f32, f32) = (0.6, 2.6);

    pub fn new() -> Self {
        Self {
            bins: 160,
            depth: 160,
            gpu: None,
            hue_lut: None,
            yaw: 0.0,
            elevation: Self::TALL_ELEVATION,
            tilt: [0.0; 2],
            tilt_target: [0.0; 2],
            clock: 0.0,
            user_view: None,
            zoom: 1.0,
            scale_shown: true,
            view_proj: Mat4::IDENTITY,
            inv_view_proj: Mat4::IDENTITY,
            depth_step: 0.8,
            height: 20.0,
            base_elevation: Self::TALL_ELEVATION,
            distance: 120.0,
            reduce_motion: false,
        }
    }

    /// True when the user turned or zoomed the view.
    pub fn has_custom_view(&self) -> bool {
        self.user_view.is_some() || self.zoom != 1.0
    }
}

impl Default for WaterfallState {
    fn default() -> Self {
        Self::new()
    }
}

// MARK: - A small column-major matrix

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Mat4(pub [[f32; 4]; 4]);

impl Mat4 {
    pub const IDENTITY: Mat4 = Mat4([
        [1.0, 0.0, 0.0, 0.0],
        [0.0, 1.0, 0.0, 0.0],
        [0.0, 0.0, 1.0, 0.0],
        [0.0, 0.0, 0.0, 1.0],
    ]);

    /// Clip z from 0 (near) to 1 (far), as in wgpu and Metal.
    pub fn perspective(fov_y: f32, aspect: f32, near: f32, far: f32) -> Mat4 {
        let f = 1.0 / (fov_y / 2.0).tan();
        Mat4([
            [f / aspect, 0.0, 0.0, 0.0],
            [0.0, f, 0.0, 0.0],
            [0.0, 0.0, far / (near - far), -1.0],
            [0.0, 0.0, near * far / (near - far), 0.0],
        ])
    }

    pub fn look_at(eye: [f32; 3], at: [f32; 3], up: [f32; 3]) -> Mat4 {
        let z = normalize(sub(eye, at));
        let x = normalize(cross(up, z));
        let y = cross(z, x);
        Mat4([
            [x[0], y[0], z[0], 0.0],
            [x[1], y[1], z[1], 0.0],
            [x[2], y[2], z[2], 0.0],
            [-dot(x, eye), -dot(y, eye), -dot(z, eye), 1.0],
        ])
    }

    pub fn mul(&self, b: &Mat4) -> Mat4 {
        let a = &self.0;
        let mut o = [[0.0f32; 4]; 4];
        for (c, column) in o.iter_mut().enumerate() {
            for (r, value) in column.iter_mut().enumerate() {
                *value = a[0][r] * b.0[c][0] + a[1][r] * b.0[c][1] + a[2][r] * b.0[c][2] + a[3][r] * b.0[c][3];
            }
        }
        Mat4(o)
    }

    pub fn transform(&self, v: [f32; 4]) -> [f32; 4] {
        let m = &self.0;
        let mut o = [0.0f32; 4];
        for (r, value) in o.iter_mut().enumerate() {
            *value = m[0][r] * v[0] + m[1][r] * v[1] + m[2][r] * v[2] + m[3][r] * v[3];
        }
        o
    }

    /// The inverse, or the identity for a singular matrix.
    pub fn inverse(&self) -> Mat4 {
        let m: Vec<f32> = self.0.iter().flatten().copied().collect();
        let (a00, a01, a02, a03) = (m[0], m[1], m[2], m[3]);
        let (a10, a11, a12, a13) = (m[4], m[5], m[6], m[7]);
        let (a20, a21, a22, a23) = (m[8], m[9], m[10], m[11]);
        let (a30, a31, a32, a33) = (m[12], m[13], m[14], m[15]);
        let b00 = a00 * a11 - a01 * a10;
        let b01 = a00 * a12 - a02 * a10;
        let b02 = a00 * a13 - a03 * a10;
        let b03 = a01 * a12 - a02 * a11;
        let b04 = a01 * a13 - a03 * a11;
        let b05 = a02 * a13 - a03 * a12;
        let b06 = a20 * a31 - a21 * a30;
        let b07 = a20 * a32 - a22 * a30;
        let b08 = a20 * a33 - a23 * a30;
        let b09 = a21 * a32 - a22 * a31;
        let b10 = a21 * a33 - a23 * a31;
        let b11 = a22 * a33 - a23 * a32;
        let det = b00 * b11 - b01 * b10 + b02 * b09 + b03 * b08 - b04 * b07 + b05 * b06;
        if det.abs() < 1e-12 {
            return Mat4::IDENTITY;
        }
        let d = 1.0 / det;
        let o = [
            (a11 * b11 - a12 * b10 + a13 * b09) * d,
            (a02 * b10 - a01 * b11 - a03 * b09) * d,
            (a31 * b05 - a32 * b04 + a33 * b03) * d,
            (a22 * b04 - a21 * b05 - a23 * b03) * d,
            (a12 * b08 - a10 * b11 - a13 * b07) * d,
            (a00 * b11 - a02 * b08 + a03 * b07) * d,
            (a32 * b02 - a30 * b05 - a33 * b01) * d,
            (a20 * b05 - a22 * b02 + a23 * b01) * d,
            (a10 * b10 - a11 * b08 + a13 * b06) * d,
            (a01 * b08 - a00 * b10 - a03 * b06) * d,
            (a30 * b04 - a31 * b02 + a33 * b00) * d,
            (a21 * b02 - a20 * b04 - a23 * b00) * d,
            (a11 * b07 - a10 * b09 - a12 * b06) * d,
            (a00 * b09 - a01 * b07 + a02 * b06) * d,
            (a31 * b01 - a30 * b03 - a32 * b00) * d,
            (a20 * b03 - a21 * b01 + a22 * b00) * d,
        ];
        Mat4([
            [o[0], o[1], o[2], o[3]],
            [o[4], o[5], o[6], o[7]],
            [o[8], o[9], o[10], o[11]],
            [o[12], o[13], o[14], o[15]],
        ])
    }
}

fn sub(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}

fn dot(a: [f32; 3], b: [f32; 3]) -> f32 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

fn cross(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ]
}

fn normalize(a: [f32; 3]) -> [f32; 3] {
    let l = dot(a, a).sqrt().max(1e-9);
    [a[0] / l, a[1] / l, a[2] / l]
}

// MARK: - The camera

impl SpectrogramPanel {
    /// Moves the camera: the sway with the clock, the tilt toward the pointer with a 150 ms time constant.
    pub(super) fn step_camera(&mut self, base: &mut PanelBase, dt: f64) {
        if !self.is_waterfall() {
            return;
        }
        let w = &mut self.waterfall;
        let before = (w.yaw, w.elevation);
        w.clock += dt.max(0.0);
        let plot = self.plot;
        if let (Some(hv), true) = (base.hover, plot.w > 1.0 && plot.h > 1.0 && !w.reduce_motion) {
            let x = (hv[0] - plot.mid_x()) / (plot.w / 2.0);
            let y = (hv[1] - plot.mid_y()) / (plot.h / 2.0);
            w.tilt_target = [x.clamp(-1.0, 1.0), y.clamp(-1.0, 1.0)];
        } else {
            w.tilt_target = [0.0; 2];
        }
        let k = if w.reduce_motion {
            1.0
        } else {
            1.0 - (-dt / 0.15).exp() as f32
        };
        for i in 0..2 {
            w.tilt[i] += (w.tilt_target[i] - w.tilt[i]) * k;
        }
        let gap = ((w.tilt[0] - w.tilt_target[0]).powi(2) + (w.tilt[1] - w.tilt_target[1]).powi(2)).sqrt();
        if gap < 0.002 {
            w.tilt = w.tilt_target;
        }
        let sway = if w.reduce_motion || w.user_view.is_some() {
            0.0
        } else {
            (w.clock * 2.0 * std::f64::consts::PI / WaterfallState::SWAY_PERIOD).sin() as f32
                * WaterfallState::SWAY_AMPLITUDE
        };
        let (base_yaw, base_elevation) = w.user_view.unwrap_or((WaterfallState::REST_YAW, w.base_elevation));
        w.yaw = base_yaw + sway + w.tilt[0] * WaterfallState::TILT_YAW;
        w.elevation = (base_elevation - w.tilt[1] * WaterfallState::TILT_ELEVATION).clamp(0.06, 1.52);
        self.update_camera();
        if before != (self.waterfall.yaw, self.waterfall.elevation) {
            base.needs_display = true;
        }
    }

    /// True while the camera moves on its own or still eases toward the pointer.
    pub(super) fn waterfall_animating(&self) -> bool {
        let w = &self.waterfall;
        self.is_waterfall() && ((!w.reduce_motion && w.user_view.is_none()) || w.tilt != w.tilt_target)
    }

    /// Turns the view by the angles (radians). The elevation stays over the floor; the yaw is free.
    pub fn orbit(&mut self, base: &mut PanelBase, d_yaw: f32, d_elevation: f32) {
        if !self.is_waterfall() {
            return;
        }
        let w = &mut self.waterfall;
        let current = w.user_view.unwrap_or((
            w.yaw - w.tilt[0] * WaterfallState::TILT_YAW,
            w.elevation + w.tilt[1] * WaterfallState::TILT_ELEVATION,
        ));
        let mut yaw = (current.0 + d_yaw) % (2.0 * PI);
        if yaw > PI {
            yaw -= 2.0 * PI;
        } else if yaw < -PI {
            yaw += 2.0 * PI;
        }
        let (lo, hi) = WaterfallState::ELEVATION_RANGE;
        w.user_view = Some((yaw, (current.1 + d_elevation).clamp(lo, hi)));
        self.step_camera(base, 0.0);
        base.needs_display = true;
    }

    /// Multiplies the zoom (2 = twice as close).
    pub fn zoom(&mut self, base: &mut PanelBase, factor: f32) {
        if !self.is_waterfall() || !factor.is_finite() || factor <= 0.0 {
            return;
        }
        let (lo, hi) = WaterfallState::ZOOM_RANGE;
        self.waterfall.zoom = (self.waterfall.zoom * factor).clamp(lo, hi);
        self.step_camera(base, 0.0);
        base.needs_display = true;
    }

    /// Back to the resting view, with its sway.
    pub fn reset_view(&mut self, base: &mut PanelBase) {
        self.waterfall.user_view = None;
        self.waterfall.zoom = 1.0;
        self.step_camera(base, 0.0);
        base.needs_display = true;
    }

    /// True when the user turned or zoomed the view.
    pub fn has_custom_view(&self) -> bool {
        self.waterfall.has_custom_view()
    }

    /// Sets the view outright (offscreen pictures and tests). `None` = the resting view.
    pub fn set_view(&mut self, base: &mut PanelBase, view: Option<(f32, f32)>, zoom: f32) {
        let (lo, hi) = WaterfallState::ZOOM_RANGE;
        self.waterfall.user_view = view;
        self.waterfall.zoom = zoom.clamp(lo, hi);
        self.step_camera(base, 0.0);
    }

    pub fn set_reduce_motion(&mut self, base: &mut PanelBase, on: bool) {
        if self.waterfall.reduce_motion != on {
            self.waterfall.reduce_motion = on;
            base.needs_display = true;
        }
    }

    fn update_camera(&mut self) {
        let w = &self.waterfall;
        let distance = self.fitted_distance(w.yaw, w.elevation) / w.zoom;
        let view_proj = self.camera_matrix(w.yaw, w.elevation, distance);
        let w = &mut self.waterfall;
        w.distance = distance;
        w.view_proj = view_proj;
        w.inv_view_proj = view_proj.inverse();
    }

    /// The view-projection of a camera on the orbit: `distance` from the aim point, which sits a little over the
    /// floor in the middle of the depth.
    fn camera_matrix(&self, yaw: f32, elevation: f32, distance: f32) -> Mat4 {
        let w = &self.waterfall;
        let aspect = self.plot.w.max(1.0) / self.plot.h.max(1.0);
        let at = [0.0, w.height * 0.12, -(w.depth as f32) * w.depth_step * 0.5];
        let eye = [
            at[0] + yaw.sin() * distance * elevation.cos(),
            at[1] + elevation.sin() * distance,
            at[2] + yaw.cos() * distance * elevation.cos(),
        ];
        let far = distance * 4.0 + w.depth as f32 * w.depth_step;
        Mat4::perspective(WaterfallState::FIELD_OF_VIEW, aspect, 1.0, far).mul(&Mat4::look_at(eye, at, [0.0, 1.0, 0.0]))
    }

    /// The nearest camera distance at which the field fits the plot from this angle: the near row with its labels
    /// and a bar of full height stay inside; the far corners may leave the top a little. A card too narrow for the
    /// whole width shows the middle of the field (the distance is capped).
    fn fitted_distance(&self, yaw: f32, elevation: f32) -> f32 {
        let w = &self.waterfall;
        let bins = w.bins as f32;
        let far = -(w.depth as f32 + 1.0) * w.depth_step;
        let near = [
            [-bins * 0.5 - 4.0, 0.0, 2.2],
            [bins * 0.5 + 4.0, 0.0, 2.2],
            [0.0, w.height, 0.0],
            [bins * 0.5 + 3.0, w.height, 0.3],
            [-bins * 0.5 - 3.0, w.height, 0.3],
        ];
        let far_corners = [[-bins * 0.5 - 2.0, 0.0, far], [bins * 0.5 + 2.0, 0.0, far]];
        // The row of frequency labels under the near edge needs 26 pt of the plot, whatever its height.
        let bottom = -1.0 + 2.0 * 26.0 / self.plot.h.max(60.0);
        let checks: Vec<([f32; 3], f32, f32)> = near
            .iter()
            .map(|p| (*p, 0.93, 0.97))
            .chain(far_corners.iter().map(|p| (*p, 1.0, 1.12)))
            .collect();
        let fits = |d: f32| {
            let m = self.camera_matrix(yaw, elevation, d);
            checks.iter().all(|(p, x_bound, y_top)| {
                let c = m.transform([p[0], p[1], p[2], 1.0]);
                if c[3] <= 1e-4 {
                    return false;
                }
                let (nx, ny) = (c[0] / c[3], c[1] / c[3]);
                nx.abs() <= *x_bound && ny >= bottom && ny <= *y_top
            })
        };
        let (mut lo, mut hi) = (bins * 0.3, bins * 3.0);
        if fits(lo) {
            return lo;
        }
        if !fits(hi) {
            return hi;
        }
        for _ in 0..22 {
            let mid = (lo + hi) / 2.0;
            if fits(mid) {
                hi = mid;
            } else {
                lo = mid;
            }
        }
        hi.min(bins * 1.8)
    }

    /// A world point of the field in panel points (top-left origin). `None` behind the camera.
    pub(super) fn project(&self, p: [f32; 3]) -> Option<[f32; 2]> {
        let c = self.waterfall.view_proj.transform([p[0], p[1], p[2], 1.0]);
        if c[3] <= 1e-4 {
            return None;
        }
        let (nx, ny) = (c[0] / c[3], c[1] / c[3]);
        Some([
            self.plot.min_x() + (nx + 1.0) / 2.0 * self.plot.w,
            self.plot.min_y() + (1.0 - ny) / 2.0 * self.plot.h,
        ])
    }

    /// The field cell under a point of the plot: the ray through the point meets the plane of the bar tops at 40 %
    /// of the scale. `None` outside the field.
    pub(super) fn field_cell(&self, point: [f32; 2]) -> Option<(usize, usize)> {
        let plot = self.plot;
        if !(plot.contains_point(point[0], point[1]) && plot.w > 1.0 && plot.h > 1.0) {
            return None;
        }
        let w = &self.waterfall;
        let nx = (point[0] - plot.min_x()) / plot.w * 2.0 - 1.0;
        let ny = 1.0 - (point[1] - plot.min_y()) / plot.h * 2.0;
        let a = w.inv_view_proj.transform([nx, ny, 0.0, 1.0]);
        let b = w.inv_view_proj.transform([nx, ny, 1.0, 1.0]);
        if a[3].abs() <= 1e-6 || b[3].abs() <= 1e-6 {
            return None;
        }
        let p0 = [a[0] / a[3], a[1] / a[3], a[2] / a[3]];
        let p1 = [b[0] / b[3], b[1] / b[3], b[2] / b[3]];
        let d = sub(p1, p0);
        if d[1].abs() <= 1e-6 {
            return None;
        }
        let t = (w.height * 0.4 - p0[1]) / d[1];
        if t <= 0.0 {
            return None;
        }
        let x = p0[0] + d[0] * t;
        let z = p0[2] + d[2] * t;
        let bin = (x + w.bins as f32 * 0.5).floor();
        let row = (-z / w.depth_step + 0.5).floor();
        if bin < 0.0 || bin >= w.bins as f32 || row < 0.0 || row >= w.depth as f32 {
            return None;
        }
        Some((bin as usize, row as usize))
    }

    /// World x of a frequency, world z of a time, on the field's own axes.
    pub(super) fn field_x(&self, hz: f32) -> f32 {
        self.axis.position(hz) * self.waterfall.bins as f32 - self.waterfall.bins as f32 * 0.5
    }

    pub(super) fn field_row(&self, seconds_ago: f64) -> f32 {
        ((seconds_ago - self.latency_seconds) / self.history_seconds) as f32 * self.waterfall.depth as f32 - 0.5
    }

    pub(super) fn field_z(&self, seconds_ago: f64) -> f32 {
        -self.field_row(seconds_ago) * self.waterfall.depth_step
    }

    pub(super) fn field_bin(&self, hz: f32) -> f32 {
        self.axis.position(hz) * self.waterfall.bins as f32 - 0.5
    }

    /// The point of a frequency and time on the plot, at `height` (0 = the floor, 1 = the top of the scale). For tests.
    pub fn project_cell(&self, hz: f32, seconds_ago: f64, height: f32) -> Option<[f32; 2]> {
        self.project([
            self.field_x(hz),
            self.waterfall.height * height,
            self.field_z(seconds_ago),
        ])
    }

    // MARK: Textures

    pub(super) fn waterfall_layout_changed(&mut self, base: &mut PanelBase) {
        if !self.is_waterfall() {
            return;
        }
        let plot = self.plot;
        let wanted = if plot.w > 1200.0 { 240 } else { 160 };
        let w = &mut self.waterfall;
        if w.depth != wanted {
            w.depth = wanted;
            w.gpu = None;
        }
        // A low plot: shorter bars and a flatter camera, so the field spreads across the width instead of shrinking.
        let aspect = plot.h / plot.w.max(1.0);
        w.height = 20.0 * (plot.h / 420.0).clamp(0.5, 1.0);
        let k = ((aspect - 0.2) / 0.2).clamp(0.0, 1.0);
        w.base_elevation =
            WaterfallState::LOW_ELEVATION + (WaterfallState::TALL_ELEVATION - WaterfallState::LOW_ELEVATION) * k;
        let (pw, ph) = (
            ((plot.w * base.scale).round() as u32).max(1),
            ((plot.h * base.scale).round() as u32).max(1),
        );
        if w.gpu.as_ref().is_some_and(|g| g.pixel_size != (pw, ph)) {
            w.gpu = None;
        }
        if self.waterfall.hue_lut.is_none() {
            self.waterfall_palette_changed(base);
        }
        self.step_camera(base, 0.0);
    }

    pub(super) fn waterfall_palette_changed(&mut self, base: &mut PanelBase) {
        self.waterfall.hue_lut = Some(base.ctx.make_lut(&base.palette.spectrum_lut(256)));
        self.waterfall.gpu = None;
    }

    /// Hue LUT positions of the field's lowest and highest frequency (the LUT runs 10 Hz ... 24 kHz).
    fn hue_positions(&self) -> (f32, f32) {
        let lo = (self.axis.min_hz / 10.0).ln() / 2400f32.ln();
        let hi = (self.axis.max_hz / 10.0).ln() / 2400f32.ln();
        (lo.clamp(0.0, 1.0), hi.clamp(0.0, 1.0))
    }

    fn make_gpu(&self, ctx: &RenderContext, ring_view: &wgpu::TextureView) -> Option<Gpu> {
        let w = &self.waterfall;
        let hue = w.hue_lut.as_ref()?;
        let (pw, ph) = (
            ((self.plot.w * self.plot_scale).round() as u32).max(1),
            ((self.plot.h * self.plot_scale).round() as u32).max(1),
        );
        let texture = |label, width, height, format, samples, usage| {
            ctx.device.create_texture(&wgpu::TextureDescriptor {
                label: Some(label),
                size: wgpu::Extent3d {
                    width,
                    height,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: samples,
                dimension: wgpu::TextureDimension::D2,
                format,
                usage,
                view_formats: &[],
            })
        };
        let field = texture(
            "Joseon waterfall field",
            w.bins as u32,
            w.depth as u32,
            FIELD_FORMAT,
            1,
            wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::RENDER_ATTACHMENT,
        );
        let color = texture(
            "Joseon waterfall",
            pw,
            ph,
            TARGET_FORMAT,
            1,
            wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::RENDER_ATTACHMENT,
        );
        let msaa_color = texture(
            "Joseon waterfall msaa",
            pw,
            ph,
            TARGET_FORMAT,
            WATERFALL_SAMPLES,
            wgpu::TextureUsages::RENDER_ATTACHMENT,
        );
        let msaa_depth = texture(
            "Joseon waterfall depth",
            pw,
            ph,
            WATERFALL_DEPTH_FORMAT,
            WATERFALL_SAMPLES,
            wgpu::TextureUsages::RENDER_ATTACHMENT,
        );
        let field_view = field.create_view(&Default::default());
        let color_view = color.create_view(&Default::default());
        let hue_view = hue.texture.create_view(&Default::default());
        let white_view = ctx.white_lut.texture.create_view(&Default::default());
        let group = |label, a: &wgpu::TextureView, b: &wgpu::TextureView| {
            ctx.device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some(label),
                layout: &ctx.history_layout,
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: wgpu::BindingResource::TextureView(a),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: wgpu::BindingResource::Sampler(&ctx.linear_sampler),
                    },
                    wgpu::BindGroupEntry {
                        binding: 2,
                        resource: wgpu::BindingResource::TextureView(b),
                    },
                    wgpu::BindGroupEntry {
                        binding: 3,
                        resource: wgpu::BindingResource::Sampler(&ctx.linear_sampler),
                    },
                ],
            })
        };
        let fold_group = group("waterfall fold", ring_view, &white_view);
        let bars_group = group("waterfall bars", &field_view, &hue_view);
        let blit_group = ctx.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("waterfall blit"),
            layout: &ctx.lut_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&color_view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(&ctx.linear_sampler),
                },
            ],
        });
        Some(Gpu {
            field,
            field_view,
            field_size: (w.bins as u32, w.depth as u32),
            color,
            color_view,
            msaa_color_view: msaa_color.create_view(&Default::default()),
            msaa_depth_view: msaa_depth.create_view(&Default::default()),
            pixel_size: (pw, ph),
            fold_group,
            bars_group,
            blit_group,
        })
    }

    // MARK: Passes

    /// The lit column and row: the shared cursor when there is one, else the pointer.
    fn lit_cell(&self, base: &PanelBase) -> (f32, f32) {
        if base.cursor_linked {
            let Some(c) = base
                .cursor
                .filter(|c| c.frequency_hz >= self.axis.min_hz && c.frequency_hz <= self.axis.max_hz)
            else {
                return (-100.0, -1.0);
            };
            let row = c.seconds_ago.map_or(-1.0, |ago| self.field_row(ago));
            let row = if row >= 0.0 && row < self.waterfall.depth as f32 {
                row
            } else {
                -1.0
            };
            return (self.field_bin(c.frequency_hz), row);
        }
        match base.hover.and_then(|hv| self.field_cell(hv)) {
            Some((bin, row)) => (bin as f32, row as f32),
            None => (-100.0, -1.0),
        }
    }

    /// Folds the ring into the field texture, then draws the bars into the multisampled picture.
    pub(super) fn encode_waterfall(
        &mut self,
        base: &mut PanelBase,
        encoder: &mut wgpu::CommandEncoder,
        ring_view: &wgpu::TextureView,
    ) {
        let ctx = base.ctx.clone();
        if self.waterfall.gpu.is_none() {
            self.waterfall.gpu = self.make_gpu(&ctx, ring_view);
        }
        let lit = self.lit_cell(base);
        let hues = self.hue_positions();
        let bg = base.palette.plot;
        let w = &self.waterfall;
        let Some(gpu) = &w.gpu else { return };
        let (bins, depth) = (w.bins as f32, w.depth as f32);

        // 1. The field: one fragment per cell.
        let fold = WaterfallSampleUniforms {
            head: self.head as f32,
            columns: COLUMNS as f32,
            rows: ROWS as f32,
            bins,
            depth,
            columns_per_row: COLUMNS as f32 / depth,
            pad: [0.0; 2],
        };
        if let Some(offset) = base.arena.push_uniform(&fold) {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("Joseon waterfall field"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &gpu.field_view,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                ..Default::default()
            });
            pass.set_pipeline(&ctx.waterfall_fold);
            pass.set_bind_group(0, &base.arena.globals_group, &[]);
            pass.set_bind_group(1, &base.arena.uniform_group, &[offset]);
            pass.set_bind_group(2, &gpu.fold_group, &[]);
            pass.draw(0..4, 0..1);
        }

        // 2. The floor and the bars, with depth, four samples per pixel, resolved into the picture.
        let uniforms = WaterfallUniforms {
            view_proj: w.view_proj.0,
            fog: [bg[0], bg[1], bg[2], 1.0],
            floor_v: ring_value(self.floor_db),
            top_v: ring_value(self.top_db),
            bins,
            depth,
            height: w.height,
            depth_step: w.depth_step,
            hover_bin: lit.0,
            hover_row: lit.1,
            hue_lo: hues.0,
            hue_hi: hues.1,
            fog_start: 0.45,
            contrast: if base.high_contrast() { 1.0 } else { 0.0 },
        };
        let Some(offset) = base.arena.push_uniform(&uniforms) else {
            return;
        };
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("Joseon waterfall bars"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: &gpu.msaa_color_view,
                depth_slice: None,
                resolve_target: Some(&gpu.color_view),
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(wgpu::Color {
                        r: bg[0] as f64,
                        g: bg[1] as f64,
                        b: bg[2] as f64,
                        a: 1.0,
                    }),
                    store: wgpu::StoreOp::Discard,
                },
            })],
            depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                view: &gpu.msaa_depth_view,
                depth_ops: Some(wgpu::Operations {
                    load: wgpu::LoadOp::Clear(1.0),
                    store: wgpu::StoreOp::Discard,
                }),
                stencil_ops: None,
            }),
            ..Default::default()
        });
        pass.set_bind_group(0, &base.arena.globals_group, &[]);
        pass.set_bind_group(1, &base.arena.uniform_group, &[offset]);
        pass.set_bind_group(2, &gpu.bars_group, &[]);
        pass.set_pipeline(&ctx.waterfall_floor);
        pass.draw(0..4, 0..1);
        pass.set_pipeline(&ctx.waterfall_bars);
        pass.draw(0..30, 0..(w.bins * w.depth) as u32);
        let _ = (&gpu.field, &gpu.color, gpu.field_size);
    }

    // MARK: Main pass

    /// The picture into the plot, the dB scale at the near right corner, the frame.
    pub(super) fn draw_waterfall(&mut self, base: &mut PanelBase, pass: &mut wgpu::RenderPass<'_>) {
        let p = base.palette;
        let plot = self.plot;
        if let Some(gpu) = &self.waterfall.gpu {
            let blit = crate::gpu::BlitUniforms {
                rect: [plot.x, plot.y, plot.w, plot.h],
                radius: 0.0,
                alpha: 1.0,
                pad: [0.0; 2],
            };
            if let Some(offset) = base.arena.push_uniform(&blit) {
                let ctx = base.ctx.clone();
                pass.set_pipeline(&ctx.blit);
                pass.set_bind_group(0, &base.arena.globals_group, &[]);
                pass.set_bind_group(1, &base.arena.uniform_group, &[offset]);
                pass.set_bind_group(2, &gpu.blit_group, &[]);
                pass.draw(0..4, 0..1);
            }
        }
        if let (Some((bottom, top)), true) = (self.db_scale_ends(), self.waterfall.scale_shown) {
            let c = p.text_faint;
            base.batch.line(
                &mut base.arena,
                [bottom[0], bottom[1]],
                [top[0], top[1]],
                1.0,
                c,
                None,
                0.0,
            );
            base.hline(top[0] - 3.0, top[0] + 3.0, top[1], c);
            base.hline(bottom[0] - 3.0, bottom[0] + 3.0, bottom[1], c);
        }
        base.batch.gradient_rect(
            &mut base.arena,
            [plot.x - 0.5, plot.y - 0.5, plot.w + 1.0, plot.h + 1.0],
            p.grid_major,
            p.grid_major,
            2.0,
            0.0,
            1.0,
        );
        base.flush(pass);
    }

    /// The dB scale: a vertical line at the near right corner of the field, floor at its foot, the top at its head.
    /// `None` when the corner projects outside the plot (a narrow card).
    fn db_scale_ends(&self) -> Option<([f32; 2], [f32; 2])> {
        let w = &self.waterfall;
        let x = w.bins as f32 * 0.5 + 2.5;
        let bottom = self.project([x, 0.0, 0.3])?;
        let top = self.project([x, w.height, 0.3])?;
        let inside = self.plot.inset(2.0, 2.0);
        (inside.contains_point(bottom[0], bottom[1]) && inside.contains_point(top[0], top[1])).then_some((bottom, top))
    }

    // MARK: Text

    pub(super) fn draw_waterfall_static(&mut self, base: &PanelBase, o: &mut OverlayContext) {
        let p = base.palette;
        let compact = Self::compact(base);
        let small = fonts::mono(if compact { 10.0 } else { 11.0 }, Weight::Regular);
        let inside = self.plot.inset(1.0, 1.0);
        // Frequency decades along the near edge; a label that would touch another gives way.
        let mut placed: Vec<Rect> = Vec::new();
        for f in LogAxis::LABELED {
            if !(f >= self.axis.min_hz && f <= self.axis.max_hz) {
                continue;
            }
            let Some(pt) = self.project([self.field_x(f), 0.0, 1.6]) else {
                continue;
            };
            let text = fmt::axis_hz(f);
            let width = o.measure(&text, &small, 0.0);
            let (left, y) = (pt[0] - width / 2.0, pt[1] + 3.0);
            if !(left >= inside.min_x()
                && left + width <= inside.max_x()
                && y >= inside.min_y()
                && y + 12.0 <= inside.max_y())
            {
                continue;
            }
            let boxed = o
                .text_bounds(&text, pt[0], y, &small, HAlign::Center, VAlign::Top, 0.0)
                .inset(-3.0, -2.0);
            if placed.iter().any(|r| r.intersects(&boxed)) {
                continue;
            }
            o.text(&text, pt[0], y, &small, p.text_dim, HAlign::Center, VAlign::Top);
            placed.push(boxed);
        }
        // Time into the depth, at the left edge of the field. "now" at the near corner. Never on a frequency label.
        let step = self.time_step(base);
        let mut t = 0.0;
        let mut last_top = f32::MAX;
        let left_x = -(self.waterfall.bins as f32) * 0.5 - 2.5;
        while t < self.history_seconds + 0.001 {
            let z = if t == 0.0 { 0.3 } else { self.field_z(t) };
            if let Some(pt) = self.project([left_x, 0.0, z]) {
                let text = if t == 0.0 {
                    "now".to_string()
                } else {
                    format!("{}{} s", fmt::MINUS, t as i64)
                };
                let width = o.measure(&text, &small, 0.0);
                let x = pt[0] - 4.0;
                let boxed = o.text_bounds(&text, x, pt[1], &small, HAlign::Right, VAlign::Middle, 0.0);
                if last_top - pt[1] > 13.0
                    && x - width >= inside.min_x()
                    && x <= inside.max_x()
                    && pt[1] - 7.0 >= inside.min_y()
                    && pt[1] + 7.0 <= inside.max_y()
                    && !placed.iter().any(|r| r.intersects(&boxed))
                {
                    let color = if t == 0.0 { p.text } else { p.text_dim };
                    o.text(&text, x, pt[1], &small, color, HAlign::Right, VAlign::Middle);
                    placed.push(boxed);
                    last_top = pt[1];
                }
            }
            t += step;
        }
        // The dB scale: top value at the head of the line, the floor at its foot. From the side its corner meets the
        // frequency labels: then the scale is left out, line and all.
        self.waterfall.scale_shown = false;
        if let (Some((bottom, top)), false) = (self.db_scale_ends(), compact) {
            let top_text = fmt::number(self.top_db.round(), 0, false);
            let floor_text = fmt::number(self.floor_db, 0, false);
            let x = top[0] + 6.0;
            let widest = o
                .measure(&top_text, &small, 0.0)
                .max(o.measure(&floor_text, &small, 0.0));
            let top_box = o
                .text_bounds(&top_text, x, top[1], &small, HAlign::Left, VAlign::Middle, 0.0)
                .inset(-3.0, -2.0);
            let floor_box = o
                .text_bounds(&floor_text, x, bottom[1], &small, HAlign::Left, VAlign::Middle, 0.0)
                .inset(-3.0, -2.0);
            let line_box = Rect::new(
                top[0].min(bottom[0]) - 4.0,
                top[1].min(bottom[1]),
                (top[0] - bottom[0]).abs() + 8.0,
                (top[1] - bottom[1]).abs(),
            );
            let free = !placed
                .iter()
                .any(|r| r.intersects(&top_box) || r.intersects(&floor_box) || r.intersects(&line_box));
            if x + widest <= inside.max_x() && bottom[1] - top[1] > 26.0 && free {
                self.waterfall.scale_shown = true;
                o.text(&top_text, x, top[1], &small, p.text, HAlign::Left, VAlign::Middle);
                o.text(
                    &floor_text,
                    x,
                    bottom[1],
                    &small,
                    p.text_dim,
                    HAlign::Left,
                    VAlign::Middle,
                );
                if bottom[1] - top[1] > 44.0 {
                    o.text(
                        "dB",
                        x,
                        (top[1] + bottom[1]) / 2.0,
                        &small,
                        p.text_faint,
                        HAlign::Left,
                        VAlign::Middle,
                    );
                }
            }
        }
    }

    // MARK: Cursor

    pub(super) fn waterfall_cursor_at(&self, point: [f32; 2]) -> Option<PanelCursor> {
        let (bin, row) = self.field_cell(point)?;
        let hz = self.axis.frequency((bin as f32 + 0.5) / self.waterfall.bins as f32);
        let ago = (row as f64 + 0.5) / self.waterfall.depth as f64 * self.history_seconds + self.latency_seconds;
        Some(PanelCursor {
            frequency_hz: hz,
            seconds_ago: Some(ago),
            source: PanelKind::Spectrogram,
            is_pinned: false,
        })
    }

    pub(super) fn waterfall_is_on_cursor(&self, base: &PanelBase, point: [f32; 2]) -> bool {
        let (Some(c), Some((bin, row))) = (base.cursor, self.field_cell(point)) else {
            return false;
        };
        if (bin as f32 - self.field_bin(c.frequency_hz)).abs() > 1.5 {
            return false;
        }
        match c.seconds_ago {
            Some(ago) => (row as f32 - self.field_row(ago)).abs() <= 1.5,
            None => true,
        }
    }
}

/// The color of the text and lines of the field, for tests.
pub fn text_color(base: &PanelBase) -> Color {
    base.palette.text
}
