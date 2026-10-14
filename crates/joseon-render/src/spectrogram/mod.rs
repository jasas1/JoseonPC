//! Scrolling spectrogram. History lives in a GPU ring texture: each analysis
//! frame adds one column with a small write. The texture is never uploaded
//! again as a whole; the shader rotates it by the head index.
//!
//! The ring holds plain dB (0...1 = -120...0 dB). The color mapping happens in
//! the shader, so a change of the automatic top colors the whole history again
//! at once and the picture stays consistent.
//!
//! Port source: `Sources/JoseonRender/SpectrogramRenderer.swift`.
//!
//! The ring is a 16-bit float texture here (16-bit normalized is an optional
//! feature in wgpu). The CPU copy of the history, which the readouts use, is
//! 16-bit normalized as on macOS.

pub mod aligner;
pub mod waterfall;

use crate::axis::{LogAxis, ResampleTable};
use crate::contracts::{CursorHistorySlice, PanelCursor, PanelKind, SpectrogramMode};
use crate::cursor::{self, header, CursorReadoutItem, Tone};
use crate::fmt;
use crate::gpu::Color;
use crate::panel::{Panel, PanelBase, Signature};
use crate::text::fonts::{self, Weight};
use crate::text::{HAlign, OverlayContext, Rect, VAlign};
use aligner::{SpectrogramTimeAligner, SpectrumTiming};
use bytemuck::{Pod, Zeroable};
use joseon_core::contracts::{AnalysisFrame, SpectrumLayer};
use std::any::Any;

pub const COLUMNS: usize = 1200;
/// One row per display bin of the analyzer between 20 Hz and 20 kHz (about 890
/// of the 1024), and a few to spare: the full resolution of the frame.
pub const ROWS: usize = 1024;
const MAX_PENDING: usize = 16;
const MAX_LAYERS: usize = 4;
pub const FADE_IN_SECONDS: f64 = 0.2;

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct SpectrogramUniforms {
    rect: [f32; 4],
    head: f32,
    columns: f32,
    floor_v: f32,
    gamma: f32,
    top_v: f32,
    pad: [f32; 3],
}

/// One FFT resolution of the analyzer: its own delay line, its blend weights on
/// the rows, and the measured time between two of its transforms.
struct LayerState {
    aligner: SpectrogramTimeAligner,
    weights: Vec<f32>,
    composed: Vec<f32>,
    started: bool,
    hop: f64,
    last_digest: u64,
    last_change: f64,
    changes: u32,
}

impl LayerState {
    fn new() -> Self {
        Self {
            aligner: SpectrogramTimeAligner::new(ROWS, 64),
            weights: vec![0.0; ROWS],
            composed: vec![0.0; ROWS],
            started: false,
            hop: 0.0,
            last_digest: 0,
            last_change: -1.0,
            changes: 0,
        }
    }
}

struct Gpu {
    ring: wgpu::Texture,
    ring_view: wgpu::TextureView,
    /// The ring and the heat map, group 2 of the pipeline.
    group: Option<wgpu::BindGroup>,
}

pub struct SpectrogramPanel {
    /// The flat picture or the 3-D field. A switch keeps the history.
    mode: SpectrogramMode,
    pub waterfall: waterfall::WaterfallState,
    history_seconds: f64,
    /// At or under this level a cell is background: no navy haze over silence or the noise floor.
    pub floor_db: f32,
    /// Exponent of the color map position. 1 = the map is linear in dB.
    pub gamma: f32,

    axis: LogAxis,
    gpu: Option<Gpu>,
    heat_lut: Option<crate::context::Lut>,
    /// Next column to write on the GPU.
    head: usize,
    needs_clear: bool,
    plot: Rect,
    legend: Rect,
    /// Pixels per point of the last layout.
    plot_scale: f32,

    // Automatic top of the color scale.
    top_db: f32,
    top_target: f32,
    top_snapped: bool,
    lower_for: f64,
    higher_for: f64,

    // CPU staging, allocated once.
    row_scratch: Vec<f32>,
    composed: Vec<f32>,
    aligner: SpectrogramTimeAligner,
    /// History per resolution (`SpectrumReading::mid_layers`): every layer is delayed on its own, then the column is blended.
    layers: Vec<LayerState>,
    uses_layers: bool,
    weights_digest: u64,
    blend_scratch: Vec<f32>,
    /// Column time from which the history has data in every row.
    history_start: Option<f64>,
    valid_columns: u32,
    clock: f64,
    next_column_time: f64,
    /// Seconds between the newest analysis frame and the newest column: the wait for the window center of the lows.
    latency_seconds: f64,
    /// `MAX_PENDING` columns.
    pending: Vec<u16>,
    pending_count: usize,
    table: Option<ResampleTable>,
    /// CPU copy of the ring for the cursor readout.
    mirror: Vec<u16>,
    mirror_head: usize,
    emitted_columns: usize,
    /// Columns after the last one that held something over the floor. A blank history does not need to scroll.
    blank_columns: usize,
    row_frequencies: Vec<f32>,
}

impl Default for SpectrogramPanel {
    fn default() -> Self {
        Self::new()
    }
}

/// A value 0...1 as the bits of a 16-bit float.
fn half_bits(v: f32) -> u16 {
    let bits = v.clamp(0.0, 1.0).to_bits();
    if bits == 0 {
        return 0;
    }
    let exponent = ((bits >> 23) & 0xff) as i32 - 127 + 15;
    let mantissa = bits & 0x7f_ffff;
    if exponent <= 0 {
        // Under the smallest normal half: a subnormal, or zero.
        if exponent < -10 {
            return 0;
        }
        return ((mantissa | 0x80_0000) >> (14 - exponent)) as u16;
    }
    // To the nearest: the next bit under the kept ones.
    let half = ((exponent as u32) << 10) | (mantissa >> 13);
    (half + ((mantissa >> 12) & 1)) as u16
}

fn ring_value(db: f32) -> f32 {
    ((db + 120.0) / 120.0).max(0.0).min(1.0)
}

fn level_of(v: u16) -> f32 {
    v as f32 / 65535.0 * 120.0 - 120.0
}

fn sparse_digest(v: &[f32], step: usize) -> u64 {
    v.iter().step_by(step).fold(0xcbf2_9ce4_8422_2325, |h, x| {
        (h ^ x.to_bits() as u64).wrapping_mul(0x0100_0000_01b3)
    })
}

impl SpectrogramPanel {
    pub fn new() -> Self {
        let axis = LogAxis {
            min_hz: 20.0,
            max_hz: 20_000.0,
        };
        Self {
            mode: SpectrogramMode::Flat,
            waterfall: waterfall::WaterfallState::new(),
            history_seconds: 20.0,
            floor_db: -75.0,
            gamma: 1.0,
            axis,
            gpu: None,
            heat_lut: None,
            head: 0,
            needs_clear: true,
            plot: Rect::default(),
            legend: Rect::default(),
            plot_scale: 2.0,
            top_db: -12.0,
            top_target: -12.0,
            top_snapped: false,
            lower_for: 0.0,
            higher_for: 0.0,
            row_scratch: vec![0.0; ROWS],
            composed: vec![0.0; ROWS],
            aligner: SpectrogramTimeAligner::new(ROWS, 96),
            layers: Vec::new(),
            uses_layers: false,
            weights_digest: 0,
            blend_scratch: vec![0.0; ROWS],
            history_start: None,
            valid_columns: 0,
            clock: 0.0,
            next_column_time: 0.0,
            latency_seconds: 0.0,
            pending: vec![0; ROWS * MAX_PENDING],
            pending_count: 0,
            table: None,
            mirror: vec![0; ROWS * COLUMNS],
            mirror_head: 0,
            emitted_columns: 0,
            blank_columns: COLUMNS,
            row_frequencies: (0..ROWS)
                .map(|r| axis.frequency(r as f32 / (ROWS - 1) as f32))
                .collect(),
        }
    }

    pub fn history_seconds(&self) -> f64 {
        self.history_seconds
    }

    pub fn mode(&self) -> SpectrogramMode {
        self.mode
    }

    pub fn is_waterfall(&self) -> bool {
        self.mode == SpectrogramMode::Waterfall
    }

    /// The flat picture or the field. The history, the automatic top and the cursor stay.
    pub fn set_mode(&mut self, base: &mut PanelBase, mode: SpectrogramMode) {
        if mode != self.mode {
            self.mode = mode;
            base.needs_display = true;
            if base.width > 1.0 {
                self.layout_changed(base);
            }
        }
    }

    /// The scale of the last layout, for the field's pictures.
    fn plot_scale_of(base: &PanelBase) -> f32 {
        base.scale
    }

    pub fn set_history_seconds(&mut self, base: &mut PanelBase, seconds: f64) {
        if seconds != self.history_seconds {
            self.history_seconds = seconds;
            self.reset_history(base);
        }
    }

    pub fn top_db(&self) -> f32 {
        self.top_db
    }

    pub fn uses_layers(&self) -> bool {
        self.uses_layers
    }

    pub fn latency_seconds(&self) -> f64 {
        self.latency_seconds
    }

    pub(crate) fn compact(base: &PanelBase) -> bool {
        base.width < 380.0 || base.height < 170.0
    }

    fn reset_history(&mut self, base: &mut PanelBase) {
        self.needs_clear = true;
        self.head = 0;
        self.pending_count = 0;
        self.mirror_head = 0;
        self.emitted_columns = 0;
        self.blank_columns = COLUMNS;
        self.aligner.reset();
        self.next_column_time = 0.0;
        self.layers.clear();
        self.uses_layers = false;
        self.history_start = None;
        self.weights_digest = 0;
        self.valid_columns = 0;
        self.mirror.fill(0);
        base.needs_display = true;
    }

    // MARK: Layers

    fn push_layers(&mut self, provided: &[SpectrumLayer], dt: f64, hold: f64) {
        let Some(table) = self.table.as_ref() else { return };
        // Blend weights on the rows. They change only when the analyzer is configured again.
        let mut wd = provided.len() as u64;
        for l in provided {
            wd = wd.wrapping_mul(31).wrapping_add(sparse_digest(&l.weights, 29));
        }
        if wd != self.weights_digest {
            self.weights_digest = wd;
            for (k, l) in provided.iter().enumerate() {
                table.apply(&l.weights, 0.0, 1.0, 0.0, &mut self.layers[k].weights);
            }
            // Rows must sum to 1 after the resampling too. A row with no weight takes the last (finest) layer.
            let count = self.layers.len();
            for r in 0..ROWS {
                let sum: f32 = self.layers.iter().map(|l| l.weights[r]).sum();
                for (k, l) in self.layers.iter_mut().enumerate() {
                    if sum > 1e-4 {
                        l.weights[r] /= sum;
                    } else {
                        l.weights[r] = if k == count - 1 { 1.0 } else { 0.0 };
                    }
                }
            }
        }
        for (k, l) in provided.iter().enumerate() {
            let state = &mut self.layers[k];
            table.apply(&l.levels_db, -120.0, 0.0, 0.0, &mut self.row_scratch);
            if !state.started {
                let top = self.row_scratch.iter().copied().fold(f32::NEG_INFINITY, f32::max);
                if !(top > 0.004) {
                    continue; // nothing over the floor yet: this resolution has no data
                }
                state.started = true;
            }
            // Time between two transforms of this resolution, from the data: the levels stand still between transforms.
            let digest = sparse_digest(&l.levels_db, 37);
            if digest != state.last_digest {
                if state.last_change >= 0.0 {
                    let interval = (self.clock - state.last_change).min(0.25);
                    state.changes += 1;
                    state.hop += (interval - state.hop) / state.changes.min(8) as f64;
                }
                state.last_digest = digest;
                state.last_change = self.clock;
            }
            let latency = l.latency_seconds.max(0.0) as f64;
            let hop = (state.hop.max(dt).min(latency.max(dt)) * 500.0).round() / 500.0;
            // A frame shows the newest transform, which is half a hop old on average.
            state.aligner.configure_uniform(latency + hop / 2.0, hop);
            state.aligner.frame_hold += (hold - state.aligner.frame_hold) * 0.05;
            state.aligner.push(&self.row_scratch, self.clock);
        }
    }

    /// One column at `center_time`: every layer at its own delay, blended in the power domain with the layer weights.
    fn compose_layers(&mut self, center_time: f64, min_box: f64) {
        let acc = &mut self.blend_scratch;
        acc.fill(0.0);
        // Ring value v (0...1 = -120...0 dB) to power: exp(ln 10 * 12 * (v - 1)).
        let a = (std::f64::consts::LN_10 * 12.0) as f32;
        for l in &mut self.layers {
            l.aligner.compose(center_time, min_box, &mut l.composed);
            for r in 0..ROWS {
                acc[r] += (l.composed[r] * a - a).exp() * l.weights[r];
            }
        }
        let inverse = 1.0 / a;
        for r in 0..ROWS {
            self.composed[r] = acc[r].max(1e-13).ln() * inverse + 1.0;
        }
    }

    /// The top of the color scale follows the loudest partial: up fast, down slowly, on a 6 dB grid.
    fn update_top(&mut self, base: &mut PanelBase, peak_db: f32, dt: f64) {
        if !(peak_db > self.floor_db) {
            return;
        }
        let want = (((peak_db + 2.0) / 6.0).ceil() * 6.0).max(-42.0).min(0.0);
        if !self.top_snapped {
            self.top_snapped = true;
            self.top_db = want;
            self.top_target = want;
            return;
        }
        if want > self.top_target {
            self.higher_for += dt;
            self.lower_for = 0.0;
        } else if want < self.top_target {
            self.lower_for += dt;
            self.higher_for = 0.0;
        } else {
            self.higher_for = 0.0;
            self.lower_for = 0.0;
        }
        if self.higher_for > 0.4 || self.lower_for > 8.0 {
            self.top_target = want;
            self.higher_for = 0.0;
            self.lower_for = 0.0;
        }
        if self.top_db != self.top_target {
            self.top_db += (self.top_target - self.top_db) * (1.0 - (-dt / 0.5).exp()) as f32;
            if (self.top_target - self.top_db).abs() < 0.05 {
                self.top_db = self.top_target;
            }
            base.needs_display = true;
        }
    }

    // MARK: Geometry

    fn y_for(&self, hz: f32) -> f32 {
        self.plot.max_y() - self.axis.position(hz) * self.plot.h
    }

    fn time_step(&self, base: &PanelBase) -> f64 {
        let most = if Self::compact(base) { 4.0 } else { 8.0 };
        [1.0, 2.0, 5.0, 10.0, 15.0, 30.0, 60.0]
            .into_iter()
            .find(|s| self.history_seconds / s <= most)
            .unwrap_or(120.0)
    }

    /// Ticks on the color bar: multiples of 15 dB only, evenly spaced. The
    /// automatic top is not a tick of its own.
    pub fn legend_ticks(&self) -> Vec<f32> {
        let mut out = Vec::new();
        let mut db = (self.floor_db / 15.0).ceil() * 15.0;
        while db <= self.top_db + 0.01 && out.len() < 8 {
            out.push(db);
            db += 15.0;
        }
        out
    }

    fn legend_y(&self, db: f32) -> f32 {
        self.legend.max_y() - (db - self.floor_db) / (self.top_db - self.floor_db).max(1.0) * self.legend.h
    }

    /// x of the time of the cursor inside the plot, `None` with no time or outside the history shown.
    fn cursor_time_x(&self, c: &PanelCursor) -> Option<f32> {
        let ago = c.seconds_ago?;
        if !(self.plot.w > 1.0) {
            return None;
        }
        let x = self.plot.max_x() - ((ago - self.latency_seconds) / self.history_seconds) as f32 * self.plot.w;
        (x >= self.plot.min_x() - 0.5 && x <= self.plot.max_x() + 0.5)
            .then(|| x.max(self.plot.min_x()).min(self.plot.max_x()))
    }

    /// Seconds between two columns of the history.
    pub fn column_seconds(&self) -> f64 {
        self.history_seconds / COLUMNS as f64
    }

    /// Age of the newest column: a timed cursor cannot be newer.
    pub fn newest_seconds_ago(&self) -> f64 {
        self.latency_seconds
    }

    // MARK: History readouts

    /// Columns between the newest column and the one `seconds_ago` old. `None` outside the recorded history.
    fn columns_back(&self, seconds_ago: f64) -> Option<usize> {
        let back = ((seconds_ago - self.latency_seconds) / self.history_seconds * (COLUMNS - 1) as f64).round();
        (back >= 0.0 && (back as usize) < self.emitted_columns && (back as usize) < COLUMNS).then_some(back as usize)
    }

    fn mirror_column(&self, back: usize) -> &[u16] {
        let col = (self.mirror_head + COLUMNS * 2 - 1 - back) % COLUMNS;
        &self.mirror[col * ROWS..(col + 1) * ROWS]
    }

    /// The column under a time, from the CPU copy of the history, with its id.
    /// The id counts columns from the start: equal ids = the same column. The
    /// slice carries the ROWS of the ring (1024 rows uniform on the log axis,
    /// time-aligned and blended from the resolution layers).
    pub fn history_column(&self, seconds_ago: f64) -> Option<(usize, CursorHistorySlice)> {
        let back = self.columns_back(seconds_ago)?;
        let mid_db = self.mirror_column(back).iter().map(|&v| level_of(v)).collect();
        Some((
            self.emitted_columns - 1 - back,
            CursorHistorySlice {
                seconds_ago,
                frequencies: self.row_frequencies.clone(),
                mid_db,
            },
        ))
    }

    /// The id alone (no copy): the view asks every tick and copies only a new column.
    pub fn history_column_id(&self, seconds_ago: f64) -> Option<usize> {
        self.columns_back(seconds_ago)
            .map(|back| self.emitted_columns - 1 - back)
    }

    /// Mid level at the cursor and whether there is data: with a time, from the
    /// history column; with none, from the newest frame, like every other panel.
    fn cursor_level(&self, base: &PanelBase, c: &PanelCursor) -> (Option<f32>, bool) {
        let Some(ago) = c.seconds_ago else {
            let Some(f) = &base.frame else { return (None, false) };
            return (
                cursor::level(&f.spectrum.mid, &f.spectrum.frequencies, c.frequency_hz),
                true,
            );
        };
        let Some(back) = self.columns_back(ago) else {
            return (None, false);
        };
        let column = self.mirror_column(back);
        let pos = self.axis.position(c.frequency_hz) * (ROWS - 1) as f32;
        if !(pos >= 0.0 && pos <= (ROWS - 1) as f32) {
            return (None, true);
        }
        let i = (pos as usize).min(ROWS - 2);
        let t = pos - i as f32;
        (
            Some(level_of(column[i]) * (1.0 - t) + level_of(column[i + 1]) * t),
            true,
        )
    }

    fn level_text(&self, base: &PanelBase, c: &PanelCursor) -> String {
        match self.cursor_level(base, c) {
            (Some(db), true) if c.seconds_ago.is_some() && db <= self.floor_db => {
                format!("Mid under {} dB", fmt::number(self.floor_db, 0, false))
            }
            (Some(db), true) => format!("Mid {} dB", fmt::db(db, 1, false)),
            _ => "no data".to_string(),
        }
    }

    /// Level of the history cell under a point of the plot, `None` where nothing was recorded yet.
    pub fn history_level_at(&self, point: [f32; 2]) -> Option<f32> {
        if self.is_waterfall() {
            let c = self.waterfall_cursor_at(point)?;
            let back = self.columns_back(c.seconds_ago?)?;
            return self.history_level(back, c.frequency_hz);
        }
        if !(self.plot.contains_point(point[0], point[1]) && self.plot.w > 1.0) {
            return None;
        }
        let ago = ((self.plot.max_x() - point[0]) / self.plot.w) as f64 * (COLUMNS - 1) as f64;
        let back = ago.round() as usize;
        if !(back < self.emitted_columns && back < COLUMNS) {
            return None;
        }
        let column = self.mirror_column(back);
        let t = (self.plot.max_y() - point[1]) / self.plot.h;
        let row = ((t * (ROWS - 1) as f32).round() as i64).max(0).min(ROWS as i64 - 1) as usize;
        // A pixel of the view covers a few cells: report the strongest, like the eye does.
        let v = column[row.saturating_sub(1)..=(row + 1).min(ROWS - 1)]
            .iter()
            .copied()
            .max()
            .unwrap_or(0);
        Some(level_of(v))
    }

    /// Level in dB of a history cell: `back` = 0 is the newest column. `None` where nothing was recorded.
    pub fn history_level(&self, back: usize, hz: f32) -> Option<f32> {
        if !(back < self.emitted_columns && back < COLUMNS) {
            return None;
        }
        let row = ((self.axis.position(hz) * (ROWS - 1) as f32).round() as i64)
            .max(0)
            .min(ROWS as i64 - 1) as usize;
        Some(level_of(self.mirror_column(back)[row]))
    }

    /// The color a level gets with the current floor, top and gamma (same math as the shader), for tests.
    pub fn color_for(&self, base: &PanelBase, db: f32) -> [f32; 3] {
        let lut = base.palette.heat_lut(256);
        let t = if db <= self.floor_db {
            0.0
        } else {
            ((db - self.floor_db) / (self.top_db - self.floor_db).max(1e-3))
                .max(0.0)
                .min(1.0)
                .powf(self.gamma)
        };
        lut[((t * 255.0).round() as usize).min(255)]
    }

    /// Window-center time of the newest column on the clock of the panel (which starts at 0 and adds the frame steps).
    pub fn newest_column_time(&self) -> f64 {
        self.next_column_time - self.history_seconds / COLUMNS as f64
    }

    pub fn emitted_columns(&self) -> usize {
        self.emitted_columns
    }

    pub fn plot_rect(&self) -> Rect {
        self.plot
    }

    pub fn y_of(&self, hz: f32) -> f32 {
        self.y_for(hz)
    }

    pub fn x_of(&self, seconds_ago: f64) -> f32 {
        self.plot.max_x() - ((seconds_ago - self.latency_seconds) / self.history_seconds) as f32 * self.plot.w
    }
}

impl Panel for SpectrogramPanel {
    fn kind(&self) -> PanelKind {
        PanelKind::Spectrogram
    }

    fn as_any(&self) -> &dyn Any {
        self
    }

    fn as_any_mut(&mut self) -> &mut dyn Any {
        self
    }

    fn makes_pointer_cursors(&self) -> bool {
        true
    }

    fn needs_pre_pass_per_frame(&self) -> bool {
        true
    }

    /// The history scrolls with time, also while the source repeats one frame, until nothing visible is left. The
    /// camera of the waterfall moves on its own.
    fn is_animating(&self) -> bool {
        self.pending_count > 0 || self.waterfall_animating()
    }

    fn idle(&mut self, base: &mut PanelBase, dt: f64) {
        self.step_camera(base, dt);
    }

    fn palette_changed(&mut self, base: &mut PanelBase) {
        self.heat_lut = Some(base.ctx.make_lut(&base.palette.heat_lut(256)));
        if let Some(gpu) = &mut self.gpu {
            gpu.group = None;
        }
        self.waterfall_palette_changed(base);
    }

    fn layout_changed(&mut self, base: &mut PanelBase) {
        let compact = Self::compact(base);
        let left = if compact { 32.0 } else { 44.0 };
        let right = if compact { 10.0 } else { 58.0 };
        // A linked panel keeps a header row over the plot for the cursor readout: nothing of it ever covers the
        // history, and the plot does not jump when a cursor comes and goes.
        let top = match (base.cursor_linked, compact) {
            (true, true) => 19.0,
            (true, false) => 24.0,
            (false, true) => 8.0,
            (false, false) => 12.0,
        };
        let bottom = if compact { 20.0 } else { 26.0 };
        self.plot = Rect::new(
            left,
            top,
            (base.width - left - right).max(10.0),
            (base.height - top - bottom).max(10.0),
        );
        self.legend = Rect::new(self.plot.max_x() + 10.0, self.plot.min_y(), 14.0, self.plot.h);
        self.plot_scale = Self::plot_scale_of(base);
        self.waterfall_layout_changed(base);
    }

    fn static_signature(&mut self, base: &PanelBase) -> u64 {
        let mut h = Signature::new();
        h.add(base.static_signature())
            .add(self.history_seconds.to_bits())
            .add_f32(self.floor_db)
            .add((self.top_db * 4.0) as i64)
            .add((self.latency_seconds * 10.0) as i64)
            .add(self.mode == SpectrogramMode::Waterfall);
        if self.is_waterfall() {
            // The labels sit on the field: they move with the camera, laid out again when it turned a quarter degree.
            let w = &self.waterfall;
            h.add((w.yaw * 230.0) as i64)
                .add((w.elevation * 230.0) as i64)
                .add(w.depth)
                .add((w.zoom * 50.0) as i64);
        }
        h.finish()
    }

    fn dynamic_signature(&self, _base: &PanelBase) -> u64 {
        0
    }

    fn update(&mut self, base: &mut PanelBase, f: &AnalysisFrame, dt: f64) {
        // Row centers are uniform on the log axis from min to max. More rows than bins: interpolation, no smoothing.
        let stale = !self
            .table
            .as_ref()
            .is_some_and(|t| t.matches(&f.spectrum.frequencies, self.axis, ROWS));
        if stale {
            self.table = ResampleTable::new(&f.spectrum.frequencies, self.axis, ROWS);
        }
        match &self.table {
            Some(table) if f.spectrum.mid.len() >= table.source_count => {
                table.apply(&f.spectrum.mid, -120.0, 0.0, 0.0, &mut self.row_scratch);
            }
            _ => self.row_scratch.fill(0.0),
        }
        let peak = self.row_scratch.iter().copied().fold(f32::NEG_INFINITY, f32::max);
        self.update_top(base, peak * 120.0 - 120.0, dt);

        // The clock of the panel: host time steps, without the jumps of a paused source.
        self.clock += dt;
        let column_seconds = self.history_seconds / COLUMNS as f64;
        let min_box = column_seconds.max(1.0 / 60.0).min(0.1);
        let hold = dt.min(0.05) / 2.0;
        let sample_rate = f.stream.as_ref().map_or(48_000.0, |s| s.sample_rate);

        let provided = &f.spectrum.mid_layers;
        let source_count = self.table.as_ref().map(|t| t.source_count);
        let layered = !provided.is_empty()
            && provided.len() <= MAX_LAYERS
            && source_count.is_some_and(|n| provided.iter().all(|l| l.levels_db.len() >= n && l.weights.len() >= n));
        if layered != self.uses_layers || (layered && provided.len() != self.layers.len()) {
            // The source changed its kind of data: the delay lines start again. The picture that is drawn stays.
            self.uses_layers = layered;
            self.layers = if layered {
                provided.iter().map(|_| LayerState::new()).collect()
            } else {
                Vec::new()
            };
            self.aligner.reset();
            self.weights_digest = 0;
            if self.history_start.is_some() {
                self.history_start = Some(self.clock);
                self.valid_columns = 0;
            }
        }
        if layered {
            self.push_layers(provided, dt, hold);
            self.latency_seconds = self
                .layers
                .iter()
                .map(|l| l.aligner.latency(min_box))
                .fold(0.0, f64::max);
            // Every layer must have shown data once (the long window fills 0.7 s after the start): one left edge, no staircase.
            if self.history_start.is_none() && self.layers.iter().all(|l| l.started) {
                self.history_start = self
                    .layers
                    .iter()
                    .filter_map(|l| l.aligner.earliest_center(min_box))
                    .reduce(f64::max);
            }
        } else {
            self.aligner.configure(self.axis.min_hz, self.axis.max_hz, sample_rate);
            self.aligner.frame_hold += (hold - self.aligner.frame_hold) * 0.05;
            self.aligner.push(&self.row_scratch, self.clock);
            self.latency_seconds = self.aligner.latency(min_box);
            if let (None, true, Some(first)) = (self.history_start, peak > 0.004, self.aligner.first_time()) {
                // The analyzer shows its long window only when that has filled once: before that the lows have no data.
                let timing = SpectrumTiming::new(sample_rate);
                self.history_start = Some(
                    self.aligner
                        .earliest_center(min_box)
                        .unwrap_or(first)
                        .max(first + timing.window[0] / 2.0 + timing.hop[0]),
                );
            }
        }

        // Columns are composed at their own time, all rows on one window-center time.
        let ready = self.clock - self.latency_seconds;
        if self.next_column_time > ready + column_seconds * 2.0 || self.next_column_time < ready - column_seconds * 8.0
        {
            self.next_column_time = ready;
        }
        let floor_v = ring_value(self.floor_db);
        while self.next_column_time <= ready {
            let t = self.next_column_time;
            self.next_column_time += column_seconds;
            if self.history_start.is_some_and(|start| t >= start) {
                if layered {
                    self.compose_layers(t, min_box);
                } else {
                    self.aligner.compose(t, min_box, &mut self.composed);
                }
                // The fade counts the valid columns, not their time: while the layers still measure their hop the
                // latency grows and the column time steps back once. By count the picture rises once.
                self.valid_columns = (self.valid_columns + 1).min(1 << 30);
                let ramp = (self.valid_columns as f64 - 0.5) * column_seconds / FADE_IN_SECONDS;
                if ramp < 1.0 {
                    // Fade in from the floor: v = floor + (v - floor) * k
                    let c = ramp.max(0.0) as f32;
                    let k = c * c * (3.0 - 2.0 * c);
                    let offset = floor_v * 0.98 * (1.0 - k);
                    for v in &mut self.composed {
                        *v = *v * k + offset;
                    }
                }
            } else {
                self.composed.fill(0.0);
            }
            let lit = self.composed.iter().step_by(4).any(|v| *v > floor_v);
            self.blank_columns = if lit {
                0
            } else {
                (self.blank_columns + 1).min(COLUMNS * 2)
            };
            // A history with nothing over the floor looks the same after a scroll: skip the work.
            if self.blank_columns > COLUMNS {
                continue;
            }
            if self.pending_count == MAX_PENDING {
                // The GPU side is behind: drop the oldest staged column.
                self.pending.copy_within(ROWS.., 0);
                self.pending_count -= 1;
            }
            let start = self.pending_count * ROWS;
            for r in 0..ROWS {
                self.pending[start + r] = (self.composed[r].max(0.0).min(1.0) * 65535.0) as u16;
            }
            self.pending_count += 1;
            let at = self.mirror_head * ROWS;
            self.mirror[at..at + ROWS].copy_from_slice(&self.pending[start..start + ROWS]);
            self.mirror_head = (self.mirror_head + 1) % COLUMNS;
            self.emitted_columns += 1;
        }
        self.step_camera(base, dt);
    }

    fn encode_pre_pass(&mut self, base: &mut PanelBase, encoder: &mut wgpu::CommandEncoder) {
        let ctx = &base.ctx;
        let gpu = self.gpu.get_or_insert_with(|| {
            let ring = ctx.device.create_texture(&wgpu::TextureDescriptor {
                label: Some("Joseon spectrogram ring"),
                size: wgpu::Extent3d {
                    width: COLUMNS as u32,
                    height: ROWS as u32,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: wgpu::TextureFormat::R16Float,
                usage: wgpu::TextureUsages::TEXTURE_BINDING
                    | wgpu::TextureUsages::COPY_DST
                    | wgpu::TextureUsages::RENDER_ATTACHMENT,
                view_formats: &[],
            });
            let ring_view = ring.create_view(&Default::default());
            Gpu {
                ring,
                ring_view,
                group: None,
            }
        });
        if self.needs_clear {
            let _ = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("Joseon spectrogram clear"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &gpu.ring_view,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                ..Default::default()
            });
            self.needs_clear = false;
        }
        let mut column = [0u16; ROWS];
        for i in 0..self.pending_count {
            let source = &self.pending[i * ROWS..(i + 1) * ROWS];
            for (half, v) in column.iter_mut().zip(source) {
                *half = half_bits(*v as f32 / 65535.0);
            }
            ctx.queue.write_texture(
                wgpu::TexelCopyTextureInfo {
                    texture: &gpu.ring,
                    mip_level: 0,
                    origin: wgpu::Origin3d {
                        x: self.head as u32,
                        y: 0,
                        z: 0,
                    },
                    aspect: wgpu::TextureAspect::All,
                },
                bytemuck::cast_slice(&column),
                wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(2),
                    rows_per_image: None,
                },
                wgpu::Extent3d {
                    width: 1,
                    height: ROWS as u32,
                    depth_or_array_layers: 1,
                },
            );
            self.head = (self.head + 1) % COLUMNS;
        }
        self.pending_count = 0;
        // The field and its picture serve one frame: only when this encode draws (an offscreen feed of history does not).
        if self.is_waterfall() && base.drawing_this_frame {
            let ring_view = self.gpu.as_ref().map(|g| g.ring_view.clone());
            if let Some(ring_view) = ring_view {
                self.encode_waterfall(base, encoder, &ring_view);
            }
        }
    }

    fn draw(&mut self, base: &mut PanelBase, pass: &mut wgpu::RenderPass<'_>) {
        if self.is_waterfall() {
            self.draw_waterfall(base, pass);
            return;
        }
        let p = base.palette;
        let compact = Self::compact(base);
        let (px, py, pw, ph) = (self.plot.x, self.plot.y, self.plot.w, self.plot.h);
        let (Some(gpu), Some(lut)) = (&mut self.gpu, &self.heat_lut) else {
            return;
        };
        let ctx = base.ctx.clone();
        let group = gpu.group.get_or_insert_with(|| {
            let lut_view = lut.texture.create_view(&Default::default());
            ctx.device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("spectrogram history"),
                layout: &ctx.history_layout,
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: wgpu::BindingResource::TextureView(&gpu.ring_view),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: wgpu::BindingResource::Sampler(&ctx.ring_sampler),
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
        let uniforms = SpectrogramUniforms {
            rect: [px, py, pw, ph],
            head: self.head as f32,
            columns: COLUMNS as f32,
            floor_v: ring_value(self.floor_db),
            gamma: self.gamma,
            top_v: ring_value(self.top_db),
            pad: [0.0; 3],
        };
        if let Some(offset) = base.arena.push_uniform(&uniforms) {
            pass.set_pipeline(&ctx.spectrogram);
            pass.set_bind_group(0, &base.arena.globals_group, &[]);
            pass.set_bind_group(1, &base.arena.uniform_group, &[offset]);
            pass.set_bind_group(2, &*group, &[]);
            pass.draw(0..4, 0..1);
        }

        // Reading lines over the heat map: light enough to vanish behind bright content.
        let line: Color = [1.0, 1.0, 1.0, if p.high_contrast { 0.22 } else { 0.07 }];
        for f in LogAxis::LABELED {
            if f > self.axis.min_hz && f < self.axis.max_hz {
                base.hline(px, px + pw, self.y_for(f), line);
            }
        }
        let step = self.time_step(base);
        let mut t = step;
        while t < self.history_seconds - 0.001 {
            let tx = px + pw * (1.0 - (t - self.latency_seconds) / self.history_seconds) as f32;
            if tx < px + pw - 1.0 {
                base.vline(tx, py, py + ph, line);
            }
            t += step;
        }
        // Frame.
        base.batch.gradient_rect(
            &mut base.arena,
            [px - 0.5, py - 0.5, pw + 1.0, ph + 1.0],
            p.grid_major,
            p.grid_major,
            2.0,
            0.0,
            1.0,
        );

        if base.cursor_linked {
            // The shared cursor: the frequency as a horizontal hairline from the axis of this panel; the time, when it has one.
            if let Some(c) = base
                .cursor
                .filter(|c| c.frequency_hz >= self.axis.min_hz && c.frequency_hz <= self.axis.max_hz)
            {
                // Over a heat map a 1 px line of any one color is lost somewhere: a dark 3 px bed under it keeps it
                // readable on yellow and on blue.
                let bed: Color = [0.0, 0.0, 0.0, 0.38];
                let yy = self.y_for(c.frequency_hz);
                let tx = self.cursor_time_x(&c);
                if let Some(tx) = tx {
                    base.batch.vline(&mut base.arena, tx, py, py + ph, bed, 3.0);
                }
                base.batch.hline(&mut base.arena, px, px + pw, yy, bed, 3.0);
                if let Some(tx) = tx {
                    let color = base.cursor_line_color();
                    base.vline(tx, py, py + ph, color);
                }
                base.draw_cursor_horizontal(yy, px, px + pw);
            }
        } else if let Some(hv) = base.hover.filter(|hv| self.plot.contains_point(hv[0], hv[1])) {
            let c: Color = [1.0, 1.0, 1.0, 0.6];
            base.vline(hv[0], py, py + ph, c);
            base.hline(px, px + pw, hv[1], c);
        }
        let legend = self.legend;
        if !compact {
            // Ticks of the color bar.
            for db in self.legend_ticks() {
                base.hline(legend.max_x(), legend.max_x() + 4.0, self.legend_y(db), p.text_faint);
            }
            base.hline(legend.max_x(), legend.max_x() + 4.0, legend.min_y() + 0.5, p.text_faint);
        }
        base.flush(pass);

        if !compact {
            base.gradient_rect(
                [legend.x, legend.y, legend.w, legend.h],
                [1.0, 1.0, 1.0, 1.0],
                [self.gamma, 0.0, 0.0, 1.0],
                2.0,
            );
            if let Some(lut) = &self.heat_lut {
                base.batch
                    .flush(pass, &base.arena, &ctx.shape_lut, Some(&lut.bind_group));
            }
            base.batch.gradient_rect(
                &mut base.arena,
                [legend.x - 0.5, legend.y - 0.5, legend.w + 1.0, legend.h + 1.0],
                p.grid_major,
                p.grid_major,
                2.0,
                0.0,
                1.0,
            );
            base.flush(pass);
        }
    }

    fn draw_static(&mut self, base: &PanelBase, o: &mut OverlayContext) {
        if self.is_waterfall() {
            self.draw_waterfall_static(base, o);
            return;
        }
        let p = base.palette;
        let compact = Self::compact(base);
        let small = fonts::mono(if compact { 10.0 } else { 11.0 }, Weight::Regular);
        let plot = self.plot;
        let mut last_top = f32::MAX;
        for f in LogAxis::LABELED {
            if !(f >= self.axis.min_hz && f <= self.axis.max_hz) {
                continue;
            }
            let yy = self.y_for(f).max(plot.min_y() + 5.0).min(plot.max_y() - 5.0);
            if !(last_top - yy > 13.0) {
                continue;
            }
            o.text(
                &fmt::axis_hz(f),
                plot.min_x() - 7.0,
                yy,
                &small,
                p.text_dim,
                HAlign::Right,
                VAlign::Middle,
            );
            last_top = yy;
        }
        if !compact {
            o.text(
                "Hz",
                plot.min_x() - 7.0,
                plot.max_y() + 7.0,
                &small,
                p.text_faint,
                HAlign::Right,
                VAlign::Top,
            );
        }
        let step = self.time_step(base);
        let mut t = 0.0;
        while t < self.history_seconds + 0.001 {
            // The newest column is `latency_seconds` old: ticks are at true times.
            let now = t == 0.0;
            let xx = if now {
                plot.max_x()
            } else {
                plot.min_x() + plot.w * (1.0 - (t - self.latency_seconds) / self.history_seconds) as f32
            };
            let label = if now {
                "now".to_string()
            } else {
                format!("{}{} s", fmt::MINUS, t as i64)
            };
            // Not on top of the "Hz" corner label.
            if xx > plot.min_x() + 34.0 || now {
                o.text(
                    &label,
                    xx,
                    plot.max_y() + 7.0,
                    &small,
                    if now { p.text } else { p.text_dim },
                    if now { HAlign::Right } else { HAlign::Center },
                    VAlign::Top,
                );
            }
            t += step;
        }
        if !compact {
            let legend = self.legend;
            let lx = legend.max_x() + 7.0;
            // The top of the scale has its own label (the level of the brightest color). A 15 dB tick too close to it gives way.
            let top_y = legend.min_y() + 5.0;
            o.text(
                &fmt::number(self.top_db.round(), 0, false),
                lx,
                top_y,
                &small,
                p.text,
                HAlign::Left,
                VAlign::Middle,
            );
            for db in self.legend_ticks() {
                let yy = self.legend_y(db).max(legend.min_y() + 5.0).min(legend.max_y() - 5.0);
                if !(yy - top_y > 13.0) {
                    continue;
                }
                o.text(
                    &fmt::number(db, 0, false),
                    lx,
                    yy,
                    &small,
                    p.text_dim,
                    HAlign::Left,
                    VAlign::Middle,
                );
            }
            o.text(
                "dB",
                legend.min_x(),
                plot.max_y() + 7.0,
                &small,
                p.text_faint,
                HAlign::Left,
                VAlign::Top,
            );
        }
    }

    fn cursor_at(&self, _base: &PanelBase, point: [f32; 2]) -> Option<PanelCursor> {
        if self.is_waterfall() {
            return self.waterfall_cursor_at(point);
        }
        if !(self.plot.contains_point(point[0], point[1]) && self.plot.w > 1.0) {
            return None;
        }
        let hz = self.axis.frequency((self.plot.max_y() - point[1]) / self.plot.h);
        let ago = ((self.plot.max_x() - point[0]) / self.plot.w) as f64 * self.history_seconds + self.latency_seconds;
        Some(PanelCursor {
            frequency_hz: hz,
            seconds_ago: Some(ago),
            source: PanelKind::Spectrogram,
            is_pinned: false,
        })
    }

    fn is_on_cursor(&self, base: &PanelBase, point: [f32; 2]) -> bool {
        if self.is_waterfall() {
            return self.waterfall_is_on_cursor(base, point);
        }
        let Some(c) = base.cursor.filter(|_| self.plot.contains_point(point[0], point[1])) else {
            return false;
        };
        (self.y_for(c.frequency_hz) - point[1]).abs() <= 5.0
            || self.cursor_time_x(&c).is_some_and(|tx| (tx - point[0]).abs() <= 5.0)
    }

    fn cursor_items(&mut self, base: &PanelBase) -> Vec<CursorReadoutItem> {
        let Some(c) = base.cursor else { return Vec::new() };
        // In this panel the time is the second most important part: it is what the panel adds.
        let mut items = vec![CursorReadoutItem::new(cursor::hz(c.frequency_hz), 0, Tone::Value)];
        if let Some(n) = cursor::note(c.frequency_hz) {
            items.push(CursorReadoutItem::new(n, 3, Tone::Dim));
        }
        if let Some(ago) = c.seconds_ago {
            items.push(CursorReadoutItem::new(cursor::ago(ago), 1, Tone::Ghost));
        }
        items.push(CursorReadoutItem::new(self.level_text(base, &c), 2, Tone::Value));
        items
    }

    fn draw_dynamic(&mut self, base: &mut PanelBase, o: &mut OverlayContext) {
        if !self.shows_header_readout(base) {
            return;
        }
        let compact = Self::compact(base);
        let items = self.cursor_items(base);
        header::draw(
            o,
            &items,
            self.plot.max_x(),
            self.plot.min_x(),
            self.plot.min_y() - if compact { 9.0 } else { 11.0 },
            compact,
            base.cursor.is_some_and(|c| c.is_pinned),
            &base.palette,
        );
    }

    fn hover_label(&mut self, base: &PanelBase) -> Option<(Vec<String>, [f32; 2])> {
        let hv = base.hover.filter(|hv| self.plot.contains_point(hv[0], hv[1]))?;
        if self.is_waterfall() && self.field_cell(hv).is_none() {
            return None;
        }
        if base.cursor_linked {
            if !self.shows_pointer_readout(base) {
                return None;
            }
            let c = base.cursor?;
            let mut first = cursor::hz(c.frequency_hz);
            if let Some(n) = cursor::note(c.frequency_hz) {
                first += &format!("   {n}");
            }
            let time = c
                .seconds_ago
                .map_or(String::new(), |ago| format!("{}   ", cursor::ago(ago)));
            return Some((vec![first, time + &self.level_text(base, &c)], hv));
        }
        let (hz, ago) = match self.waterfall_cursor_at(hv).filter(|_| self.is_waterfall()) {
            Some(c) => (c.frequency_hz, c.seconds_ago.unwrap_or(self.latency_seconds)),
            None => (
                self.axis.frequency((self.plot.max_y() - hv[1]) / self.plot.h),
                ((self.plot.max_x() - hv[0]) / self.plot.w) as f64 * self.history_seconds + self.latency_seconds,
            ),
        };
        let mut first = format!("cursor  {}", fmt::hz(hz));
        if let Some((name, cents)) = fmt::note(hz) {
            first += &format!("   {name} {}", fmt::cents(cents));
        }
        let mut second = format!("time  {}", cursor::ago(ago));
        match self.history_level_at(hv) {
            Some(level) if level <= self.floor_db => {
                second += &format!("   Mid  under {} dB", fmt::number(self.floor_db, 0, false));
            }
            Some(level) => second += &format!("   Mid  {} dB", fmt::db(level, 1, false)),
            None => second += "   no data",
        }
        Some((vec![first, second], hv))
    }

    fn accessibility_label(&self) -> String {
        if self.is_waterfall() {
            "Spectrogram, 3-D waterfall".to_string()
        } else {
            "Spectrogram".to_string()
        }
    }

    fn accessibility_value(&self, base: &PanelBase) -> String {
        let seconds = self.history_seconds as i64;
        match base
            .frame
            .as_ref()
            .filter(|f| !f.is_silent && !fmt::is_floor(f.peak.level_db))
        {
            Some(f) => format!(
                "{seconds} seconds of history. Strongest now: {} at {:.1} dB",
                fmt::hz(f.peak.frequency_hz),
                f.peak.level_db as f64
            ),
            None => format!("{seconds} seconds of history. No signal"),
        }
    }
}
