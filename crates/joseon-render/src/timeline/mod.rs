//! The session timeline: the last minutes of listening as lanes over one time
//! axis.
//!
//! Lanes, top to bottom: Loudness (short-term as a soft band, the momentary
//! maximum as a thin line, the target dotted), Peak (true peak per second as
//! stems, 0 dBTP in red, overs red, clip ticks on the top edge), At the ear
//! (only with data), Tone (the 8 band energies as a hue strip, only on tall
//! panels). Under the lanes: the stress-flag span bars. Over them: a narrow row
//! for the track flags. Nothing is drawn over a trace except hairlines.
//!
//! X is AUDIO time (see `model`), newest at the right. Labels are wall-clock
//! times at round minutes, looked up from the samples; a jump of the wall clock
//! draws a gap mark on the axis.
//!
//! Cost: the picture depends on the snapshot, the layout and the cursor only.
//! Analysis frames are not read, so a display tick with the same snapshot
//! revision and the same cursor draws nothing.
//!
//! Port source: `Sources/JoseonRender/TimelineRenderer.swift`.

pub mod model;

use crate::arena::{FloatSlice, ShapeBatch};
use crate::contracts::{PanelCursor, PanelKind};
use crate::cursor::{header, hover_label, CursorReadoutItem, Tone};
use crate::fmt;
use crate::gpu::{Color, CurveUniforms};
use crate::palette::{mix, rgba, scaled_alpha, with_alpha, Palette, WHITE};
use crate::panel::{Panel, PanelBase, Signature};
use crate::text::fonts::{self, Font, Weight};
use crate::text::{HAlign, OverlayContext, Rect, VAlign};
use joseon_core::contracts::{BandEnergy, SessionEventKind, SessionSample, SessionSnapshot, SplReading};
use model::{flag_key, format, is_measured, unix_seconds, TimelineModel, MAX_SPAN_ROWS};
use std::any::Any;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum LaneKind {
    Loudness,
    Peak,
    Ear,
    Tone,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Lane {
    pub kind: LaneKind,
    pub rect: Rect,
    pub caption_y: f32,
}

/// A round minute on the axis.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct AxisMark {
    pub x: f32,
    pub unix: f64,
    pub labeled: bool,
}

/// Top and bottom of a scale.
type Range = (f32, f32);

/// +3 ... -12 dBTP: what matters of a true peak happens near 0. A quieter second keeps a cap on the lane's floor.
pub const PEAK_RANGE: Range = (3.0, -12.0);
pub const EAR_RANGE: Range = (110.0, 40.0);
pub const TONE_RANGE: Range = (-10.0, -58.0);
/// "S range −20…−9": the span of the short-term loudness in the window.
pub const SHORT_TERM_RANGE_CAPTION: &str = "S range";
/// Hues of the flag kinds: the warning amber first, then a yellow, an orange and a sand. All warm: a flag bar is
/// never taken for a trace (blue) or an over (red).
const SPAN_HUES: [Color; 4] = [
    [0.0, 0.0, 0.0, 0.0],
    [0.96, 0.87, 0.32, 1.0],
    [0.97, 0.50, 0.22, 1.0],
    [0.86, 0.74, 0.58, 1.0],
];

/// Seconds of the window that hold clipped samples (a clip event is one second with one or more clipped runs).
pub fn clip_count_text(seconds: usize) -> String {
    if seconds == 0 {
        "no clipped samples".to_string()
    } else {
        format!("{seconds} s with clipped samples")
    }
}

/// Seconds whose true peak went over 0 dBTP without a clipped sample.
pub fn over_count_text(n: usize) -> String {
    format!("{n} true-peak over{}", if n == 1 { "" } else { "s" })
}

fn unit(v: f32, r: Range) -> f32 {
    ((v - r.1) / (r.0 - r.1)).max(0.0).min(1.0)
}

fn y_of(v: f32, r: Range, rect: Rect) -> f32 {
    rect.max_y() - unit(v, r) * rect.h
}

#[derive(Clone, Copy, PartialEq)]
enum Swatch {
    None,
    Band,
    Line,
}

/// One stat line of a lane: an optional swatch (what the trace looks like), a caption and a value.
struct Stat {
    swatch: Swatch,
    caption: &'static str,
    value: String,
    color: Color,
}

pub struct TimelinePanel {
    // Options (the view and the offscreen renderer set them).
    window_seconds: i64,
    target_lufs: Option<f32>,
    /// The Tone lane is allowed. It shows from 260 pt of height.
    show_bands: bool,
    /// The "At the ear" lane is allowed (it still needs samples with a level). Off by default.
    show_level_at_ear: bool,
    /// Seconds east of UTC of the wall-clock labels. The app gives the offset of the local time zone.
    utc_offset_seconds: i32,
    /// False when the newest sample is older than "now" (the audio clock stands still): the right end of the axis
    /// then carries the time of that sample instead of "now".
    is_live: bool,
    /// The frequency a cursor made here starts with: the view sets it from the newest frame.
    pub fallback_frequency_hz: f32,

    model: TimelineModel,
    snapshot: SessionSnapshot,
    has_snapshot: bool,
    /// Counts what the picture depends on (snapshot, options). Part of the text signature.
    content_version: u64,
    /// Snapshots that were taken in (tests: an unchanged revision must not count).
    rebuild_count: u64,

    // Layout.
    size: (f32, f32),
    lanes: Vec<Lane>,
    /// x range of every lane.
    plot_x: (f32, f32),
    header_mid_y: f32,
    flag_row_y: f32,
    lanes_bottom: f32,
    spans_top: f32,
    axis_y: f32,
    loudness_range: Range,

    bulk: ShapeBatch,
    band_hues: [Color; 8],
}

impl Default for TimelinePanel {
    fn default() -> Self {
        Self::new()
    }
}

impl TimelinePanel {
    pub fn new() -> Self {
        let mut band_hues = [WHITE; 8];
        for (i, hue) in band_hues.iter_mut().enumerate() {
            let hz = (BandEnergy::EDGES_HZ[i] * BandEnergy::EDGES_HZ[i + 1]).sqrt();
            *hue = rgba(Palette::spectrum_color(hz), 1.0);
        }
        Self {
            window_seconds: 900,
            target_lufs: None,
            show_bands: true,
            show_level_at_ear: false,
            utc_offset_seconds: 0,
            is_live: true,
            fallback_frequency_hz: 1000.0,
            model: TimelineModel::new(&SessionSnapshot::default(), 900),
            snapshot: SessionSnapshot::default(),
            has_snapshot: false,
            content_version: 0,
            rebuild_count: 0,
            size: (1.0, 1.0),
            lanes: Vec::new(),
            plot_x: (0.0, 1.0),
            header_mid_y: 10.0,
            flag_row_y: 20.0,
            lanes_bottom: 0.0,
            spans_top: 0.0,
            axis_y: 0.0,
            loudness_range: (0.0, -36.0),
            bulk: ShapeBatch::with_capacity(8192),
            band_hues,
        }
    }

    // MARK: Options

    pub fn set_window_seconds(&mut self, base: &mut PanelBase, seconds: i64) {
        if seconds != self.window_seconds {
            self.window_seconds = seconds;
            self.rebuild(base);
        }
    }

    pub fn set_target_lufs(&mut self, base: &mut PanelBase, target: Option<f32>) {
        if target != self.target_lufs {
            self.target_lufs = target;
            self.rebuild(base);
        }
    }

    pub fn set_show_bands(&mut self, base: &mut PanelBase, show: bool) {
        if show != self.show_bands {
            self.show_bands = show;
            self.compute_layout(base);
            self.bump(base);
        }
    }

    pub fn set_show_level_at_ear(&mut self, base: &mut PanelBase, show: bool) {
        if show != self.show_level_at_ear {
            self.show_level_at_ear = show;
            self.compute_layout(base);
            self.bump(base);
        }
    }

    pub fn set_utc_offset_seconds(&mut self, base: &mut PanelBase, offset: i32) {
        if offset != self.utc_offset_seconds {
            self.utc_offset_seconds = offset;
            self.bump(base);
        }
    }

    pub fn set_live(&mut self, base: &mut PanelBase, live: bool) {
        if live != self.is_live {
            self.is_live = live;
            self.bump(base);
        }
    }

    /// Takes a snapshot in. Equal revision = nothing to do.
    pub fn set_snapshot(&mut self, base: &mut PanelBase, snapshot: SessionSnapshot) {
        if self.has_snapshot && snapshot.revision == self.snapshot.revision {
            return;
        }
        self.snapshot = snapshot;
        self.has_snapshot = true;
        self.rebuild(base);
    }

    fn rebuild(&mut self, base: &mut PanelBase) {
        let (rows_before, ear_before) = (self.model.span_rows, self.model.has_level_a);
        self.model = TimelineModel::new(&self.snapshot, self.window_seconds);
        self.rebuild_count = self.rebuild_count.wrapping_add(1);
        self.update_loudness_range();
        if self.model.span_rows != rows_before || self.model.has_level_a != ear_before {
            self.compute_layout(base);
        }
        self.bump(base);
    }

    fn bump(&mut self, base: &mut PanelBase) {
        self.content_version = self.content_version.wrapping_add(1);
        base.needs_display = true;
    }

    pub fn model(&self) -> &TimelineModel {
        &self.model
    }

    pub fn content_version(&self) -> u64 {
        self.content_version
    }

    pub fn rebuild_count(&self) -> u64 {
        self.rebuild_count
    }

    pub fn lanes(&self) -> &[Lane] {
        &self.lanes
    }

    pub fn plot_x(&self) -> (f32, f32) {
        self.plot_x
    }

    pub fn axis_y(&self) -> f32 {
        self.axis_y
    }

    pub fn spans_top(&self) -> f32 {
        self.spans_top
    }

    pub fn loudness_range(&self) -> (f32, f32) {
        self.loudness_range
    }

    // MARK: Layout

    /// Names and numbers stand in a column right of the lanes. Narrow panels put them over each lane.
    fn wide(&self) -> bool {
        self.size.0 >= 760.0
    }

    /// A low strip: span bars without titles.
    fn strip(&self) -> bool {
        self.size.1 < 180.0
    }

    fn span_row_height(&self) -> f32 {
        if self.strip() {
            5.0
        } else {
            15.0
        }
    }

    fn span_bar_height(&self) -> f32 {
        if self.strip() {
            3.0
        } else {
            13.0
        }
    }

    fn shows_tone(&self) -> bool {
        self.show_bands && self.size.1 >= 260.0
    }

    fn compute_layout(&mut self, base: &mut PanelBase) {
        let (wide, strip) = (self.wide(), self.strip());
        let (width, height) = self.size;
        let left = 36.0;
        let right = if wide { 122.0 } else { 12.0 };
        let header = if strip { 18.0 } else { 24.0 };
        let flag_row = 8.0;
        let axis = if strip { 18.0 } else { 22.0 };
        let rows = self.model.span_rows.max(1) as f32;
        let spans_height = rows * self.span_row_height() + 3.0;
        let caption = if wide { 0.0 } else { 15.0 };
        let gap = if wide && !strip { 5.0 } else { 3.0 };

        let mut kinds = vec![(LaneKind::Loudness, 1.0f32), (LaneKind::Peak, 0.5)];
        // A low strip shows loudness and peak only; the level at the ear stays in the readout.
        if self.model.has_level_a && self.show_level_at_ear && !strip {
            kinds.push((LaneKind::Ear, 0.5));
        }
        if self.shows_tone() {
            kinds.push((LaneKind::Tone, 0.42));
        }

        self.plot_x = (left, (width - right).max(left + 10.0));
        self.header_mid_y = header / 2.0 + 1.0;
        // The flag row stands directly over the first lane (under that lane's caption on narrow panels).
        self.flag_row_y = header + caption;
        let top = header;
        let bottom = height - axis - spans_height;
        let count = kinds.len() as f32;
        let free = (bottom - top - flag_row - count * caption - (count - 1.0) * gap).max(8.0);
        let weights: f32 = kinds.iter().map(|k| k.1).sum();
        let mut y = top;
        self.lanes.clear();
        for (i, (kind, w)) in kinds.into_iter().enumerate() {
            let h = (free * w / weights).floor();
            y += caption;
            let caption_y = y - 4.0;
            if i == 0 {
                y += flag_row;
            }
            self.lanes.push(Lane {
                kind,
                rect: Rect::new(self.plot_x.0, y, self.plot_x.1 - self.plot_x.0, h.max(4.0)),
                caption_y,
            });
            y += h + gap;
        }
        self.lanes_bottom = self.lanes.last().map_or(top, |l| l.rect.max_y());
        self.spans_top = self.lanes_bottom + 3.0;
        self.axis_y = height - axis;
        base.needs_display = true;
    }

    fn lane(&self, kind: LaneKind) -> Option<Lane> {
        self.lanes.iter().find(|l| l.kind == kind).copied()
    }

    fn plot_width(&self) -> f32 {
        self.plot_x.1 - self.plot_x.0
    }

    fn points_per_second(&self) -> f32 {
        self.plot_width() / self.model.window_seconds as f32
    }

    pub fn x_for_time(&self, t: f64) -> f32 {
        self.plot_x.1 - (self.model.newest_time - t) as f32 * self.points_per_second()
    }

    pub fn x_for_seconds_ago(&self, ago: f64) -> f32 {
        self.plot_x.1 - ago as f32 * self.points_per_second()
    }

    // MARK: Scales

    /// The LUFS window: the top follows the loudest momentary value on a 6 LU grid, the bottom the quiet passages,
    /// between 24 and 48 LU under it. Steps of 6 LU: the scale stays put while the music plays.
    fn update_loudness_range(&mut self) {
        let mut hi = f32::NEG_INFINITY;
        let mut values = Vec::new();
        for s in self.model.samples.iter().filter(|s| !s.is_silent) {
            if is_measured(s.momentary_max_lufs) {
                hi = hi.max(s.momentary_max_lufs);
            }
            if is_measured(s.short_term_lufs) {
                hi = hi.max(s.short_term_lufs);
                values.push(s.short_term_lufs);
            }
        }
        if !hi.is_finite() {
            self.loudness_range = (0.0, -36.0);
            return;
        }
        if let Some(t) = self.target_lufs {
            hi = hi.max(t);
        }
        let top = (((hi + 1.0) / 6.0).ceil() * 6.0).min(6.0);
        values.sort_by(f32::total_cmp);
        // 2nd percentile: a fade-out does not set the scale.
        let low = if values.is_empty() {
            top - 30.0
        } else {
            values[values.len() / 50]
        };
        let mut bottom = ((low - 3.0) / 6.0).floor() * 6.0;
        if let Some(t) = self.target_lufs {
            bottom = bottom.min(((t - 3.0) / 6.0).floor() * 6.0);
        }
        bottom = bottom.max(top - 48.0).min(top - 24.0);
        self.loudness_range = (top, bottom);
    }

    /// Scale values to label, most important first. The text pass places them top-down by this order and leaves out
    /// what would stand closer than 12 pt to one already placed.
    fn scale_values(&self, kind: LaneKind, height: f32) -> Vec<f32> {
        match kind {
            LaneKind::Loudness => {
                let r = self.loudness_range;
                let per_lu = height / (r.0 - r.1);
                let step = if per_lu * 6.0 >= 14.0 {
                    6.0
                } else if per_lu * 12.0 >= 14.0 {
                    12.0
                } else {
                    24.0
                };
                let mut out = Vec::new();
                let mut v = (r.0 / step).floor() * step;
                while v >= r.1 - 0.01 {
                    out.push(v);
                    v -= step;
                }
                out
            }
            LaneKind::Peak => vec![0.0, -12.0, -6.0, 3.0],
            LaneKind::Ear => vec![85.0, 55.0, 100.0, 70.0, 40.0],
            LaneKind::Tone => Vec::new(),
        }
    }

    // MARK: Focus (the second under the cursor)

    /// True when `p` is over the lanes (the flag row and the span rows included).
    fn over_lanes(&self, p: [f32; 2]) -> bool {
        p[0] >= self.plot_x.0 && p[0] <= self.plot_x.1 && p[1] >= self.flag_row_y && p[1] <= self.axis_y
    }

    /// Seconds before the newest sample that the cursor stands at. Linked: the shared cursor's `seconds_ago`. Not
    /// linked: the pointer.
    ///
    /// The spectrogram counts `seconds_ago` on the wall clock inside its short history; the timeline counts audio
    /// seconds back from the newest sample. The two agree while audio plays without a break; across a pause they
    /// differ by the length of the pause, and the timeline's audio axis is the one that has a place for every
    /// recorded second.
    fn focus_seconds_ago(&self, base: &PanelBase) -> Option<f64> {
        if base.cursor_linked {
            return base.cursor?.seconds_ago;
        }
        let hv = base.hover.filter(|hv| self.over_lanes(*hv) && !self.model.is_empty())?;
        Some(((self.plot_x.1 - hv[0]) / self.points_per_second()) as f64)
    }

    /// The sample the readout reads, `None` outside the record.
    pub fn focus_index(&self, base: &PanelBase) -> Option<usize> {
        let ago = self
            .focus_seconds_ago(base)
            .filter(|ago| *ago <= self.model.window_seconds + 0.5)?;
        self.model.sample_index(self.model.newest_time - ago, 2.0)
    }

    /// x of the hairline: the exact time of a cursor from another panel, the middle of the second for the timeline's
    /// own.
    fn focus_x(&self, base: &PanelBase) -> Option<f32> {
        let xx = self.x_for_seconds_ago(self.focus_seconds_ago(base)?);
        (xx >= self.plot_x.0 - 0.5 && xx <= self.plot_x.1 + 0.5).then(|| xx.max(self.plot_x.0).min(self.plot_x.1))
    }

    /// The readout follows the pointer: the rule of every panel.
    fn pointer_readout_of_the_base(&self, base: &PanelBase) -> bool {
        match (base.cursor_linked, base.hover, base.cursor) {
            (true, Some(hover), Some(c)) if !c.is_pinned && c.source == PanelKind::Timeline => {
                self.cursor_at(base, hover) == Some(c)
            }
            _ => false,
        }
    }

    fn readout_in_header(&self, base: &PanelBase) -> bool {
        if base.cursor_linked {
            self.shows_header_readout(base)
        } else {
            (self.strip() || self.box_too_tall(base)) && self.focus_index(base).is_some()
        }
    }

    /// A readout with more lines than the lanes are high goes into the header row.
    fn box_too_tall(&self, base: &PanelBase) -> bool {
        let Some(i) = self.focus_index(base) else {
            return false;
        };
        self.readout_lines(i).len() as f32 * hover_label::LINE_HEIGHT + hover_label::PADDING * 1.25 > self.axis_y - 6.0
    }

    // MARK: Shapes

    fn dotted(base: &mut PanelBase, x0: f32, x1: f32, y: f32, color: Color, on: f32, period: f32) {
        let mut x = x0;
        while x < x1 - 0.5 {
            base.hline(x, (x + on).min(x1), y, color);
            x += period;
        }
    }

    /// A vertical mark through the lanes: the flag row and every lane, never the caption rows between lanes (no line
    /// through a text).
    fn vertical(&self, base: &mut PanelBase, x: f32, color: Color, dashed: bool, to_axis: bool) {
        let first_top = self.lanes.first().map_or(self.flag_row_y, |l| l.rect.min_y());
        let mut parts = vec![(self.flag_row_y + 1.0, first_top)];
        for l in &self.lanes {
            parts.push((l.rect.min_y(), l.rect.max_y()));
        }
        if self.wide() {
            // Lanes stand close together here and nothing is written between them: one line.
            parts = vec![(self.flag_row_y + 1.0, self.lanes_bottom)];
        }
        if to_axis {
            parts.push((self.lanes_bottom, self.axis_y));
        }
        for (a, b) in parts {
            if !(b > a) {
                continue;
            }
            if dashed {
                let mut y = a;
                while y < b - 0.5 {
                    base.vline(x, y, (y + 3.0).min(b), color);
                    y += 6.0;
                }
            } else {
                base.vline(x, a, b, color);
            }
        }
    }

    /// Runs of neighbor samples that hold a value: a curve breaks at silence, at a missing second and at a gap.
    /// Gives the index of the first sample and the values of each run.
    fn runs(&self, value: impl Fn(&SessionSample) -> Option<f32>) -> Vec<(usize, Vec<f32>)> {
        let s = &self.model.samples;
        let mut out = Vec::new();
        let mut current: Vec<f32> = Vec::new();
        let mut first = 0;
        let mut close = |current: &mut Vec<f32>, first: usize| {
            if current.len() >= 2 {
                out.push((first, std::mem::take(current)));
            }
            current.clear();
        };
        for i in 0..s.len() {
            let v = if s[i].is_silent { None } else { value(&s[i]) };
            let broken = i > 0 && (self.model.gap_before(i).is_some() || s[i].time - s[i - 1].time > 1.5);
            if broken {
                close(&mut current, first);
            }
            match v {
                Some(v) => {
                    if current.is_empty() {
                        first = i;
                    }
                    current.push(v);
                }
                None => close(&mut current, first),
            }
        }
        close(&mut current, first);
        out
    }

    fn set_scissor(&self, base: &PanelBase, pass: &mut wgpu::RenderPass<'_>, r: Option<Rect>) {
        let sc = base.scale;
        let (width, height) = (base.width, base.height);
        let r = r.unwrap_or(Rect::new(0.0, 0.0, width, height));
        // The part of the rectangle inside the panel.
        let (x0, y0) = (r.min_x().max(0.0), r.min_y().max(0.0));
        let (x1, y1) = (r.max_x().min(width), r.max_y().min(height));
        if !(x1 - x0 >= 1.0 && y1 - y0 >= 1.0) {
            return;
        }
        let (x, y) = ((x0 * sc).round() as i64, (y0 * sc).round() as i64);
        let (max_w, max_h) = ((width * sc) as i64, (height * sc) as i64);
        pass.set_scissor_rect(
            x.min((max_w - 1).max(0)) as u32,
            y.min((max_h - 1).max(0)) as u32,
            (((x1 - x0) * sc).round() as i64).min(max_w - x).max(1) as u32,
            (((y1 - y0) * sc).round() as i64).min(max_h - y).max(1) as u32,
        );
    }

    /// Columns of at least `min_width` points: `k` samples per column, anchored to the audio clock so the grouping
    /// does not change while the record scrolls. Gives the sample range and the x range of each column.
    fn columns(&self, min_width: f32, pps: f32) -> Vec<(std::ops::Range<usize>, f32, f32)> {
        let samples = &self.model.samples;
        let k = ((min_width / pps.max(1e-4)).ceil() as i64).max(1);
        let (left, right) = self.plot_x;
        let group = |s: &SessionSample| (s.time - 0.5).floor() as i64 / k;
        let mut out = Vec::new();
        let mut i = 0;
        while i < samples.len() {
            let g = group(&samples[i]);
            let mut j = i + 1;
            while j < samples.len() && group(&samples[j]) == g && samples[j].time - samples[j - 1].time < 1.5 {
                j += 1;
            }
            let a = self.x_for_time(samples[i].time - 1.0);
            let b = self.x_for_time(samples[j - 1].time);
            if b > left && a < right {
                out.push((i..j, a.max(left), b.min(right)));
            }
            i = j;
        }
        out
    }

    fn draw_peak_stems(&mut self, base: &mut PanelBase, r: Rect, pps: f32) {
        let p = base.palette;
        let px = 1.0 / base.scale;
        let zero_y = y_of(0.0, PEAK_RANGE, r);
        let floor = r.max_y();
        let body = mix(p.accent, WHITE, 0.30);
        let gap = if pps >= 3.0 { px } else { 0.0 };
        for (range, a, b) in self.columns(px * 2.0, pps) {
            let mut tp = f32::NEG_INFINITY;
            for s in self.model.samples[range].iter().filter(|s| !s.is_silent) {
                tp = tp.max(s.true_peak_dbtp);
            }
            if !is_measured(tp) {
                continue;
            }
            let xa = self.bulk.snap(a);
            let w = (self.bulk.snap(b) - xa - gap).max(px);
            // Under the lane's floor: the cap stands on the floor ("there was sound, far from the top").
            let top = y_of(tp, PEAK_RANGE, r).min(floor - 1.5);
            // A calm body with a bright cap: the caps read as a step line, the body says "up to here". Only an over
            // is red: a modern master stands at -0.2 dBTP all day, and that is not an alarm.
            let body_top = top.max(zero_y);
            let arena = &mut base.arena;
            self.bulk.gradient_rect(
                arena,
                [xa, body_top, w, floor - body_top],
                with_alpha(body, 0.34),
                with_alpha(body, 0.10),
                0.0,
                0.0,
                0.0,
            );
            self.bulk.rect(arena, [xa, body_top, w, 1.5], with_alpha(body, 0.95));
            if tp > 0.0 {
                self.bulk
                    .rect(arena, [xa - 0.5, top, w + 1.0, zero_y - top], with_alpha(p.danger, 1.0));
            }
        }
    }

    fn draw_tone(&mut self, base: &mut PanelBase, r: Rect, pps: f32) {
        let px = 1.0 / base.scale;
        let row_h = r.h / 8.0;
        for (range, a, b) in self.columns(1.5, pps) {
            let mut sum = [0.0f32; 8];
            let mut n = 0.0f32;
            for s in self.model.samples[range].iter().filter(|s| !s.is_silent) {
                for k in 0..8 {
                    sum[k] += s.bands[k];
                }
                n += 1.0;
            }
            if !(n > 0.0) {
                continue;
            }
            let xa = self.bulk.snap(a);
            let w = (self.bulk.snap(b) - xa).max(px);
            for k in 0..8 {
                let t = unit(sum[k] / n, TONE_RANGE);
                if !(t > 0.01) {
                    continue;
                }
                // Hue = the band's place on the frequency axis (the color system of every panel), brightness = level.
                let light = 0.05 + 0.95 * t.powf(2.2);
                let hue = self.band_hues[k];
                let yy = r.max_y() - (k + 1) as f32 * row_h;
                self.bulk.rect(
                    &mut base.arena,
                    [xa, yy, w, row_h],
                    [hue[0] * light, hue[1] * light, hue[2] * light, 1.0],
                );
            }
        }
    }

    // MARK: Wall-clock marks

    /// Round minutes on the axis. Every minute gets a tick when there is room; labels stand at a step that keeps
    /// them apart.
    pub fn minute_marks(&self) -> Vec<AxisMark> {
        if self.model.is_empty() {
            return Vec::new();
        }
        let pps = self.points_per_second();
        let fits = |minutes: i64, room: f32| (minutes * 60) as f32 * pps >= room;
        let label_step = [1, 2, 5, 10, 15, 30].into_iter().find(|m| fits(*m, 80.0)).unwrap_or(30);
        let tick_step = [1, 5, 10].into_iter().find(|m| fits(*m, 7.0)).unwrap_or(label_step);
        let step = tick_step.min(label_step);
        self.model
            .minute_marks(step, self.utc_offset_seconds)
            .into_iter()
            .map(|m| {
                let local = m.unix.round() as i64 + self.utc_offset_seconds as i64;
                AxisMark {
                    x: self.x_for_time(m.time),
                    unix: m.unix,
                    labeled: (local / 60) % label_step == 0,
                }
            })
            .collect()
    }

    // MARK: Text

    fn number_font() -> Font {
        fonts::mono(10.0, Weight::Regular)
    }

    fn cap_font() -> Font {
        fonts::ui(10.0, Weight::Semibold)
    }

    fn stat_font() -> Font {
        fonts::ui(10.5, Weight::Medium)
    }

    fn lane_unit(kind: LaneKind) -> &'static str {
        match kind {
            LaneKind::Loudness => "LUFS",
            LaneKind::Peak => "dBTP",
            LaneKind::Ear => "dB(A)",
            LaneKind::Tone => "",
        }
    }

    fn lane_name(kind: LaneKind) -> &'static str {
        match kind {
            LaneKind::Loudness => "LOUDNESS",
            LaneKind::Peak => "TRUE PEAK",
            LaneKind::Ear => "AT THE EAR",
            LaneKind::Tone => "TONE",
        }
    }

    fn stats(&self, base: &PanelBase, kind: LaneKind) -> Vec<Stat> {
        let (p, st) = (&base.palette, &self.model.stats);
        match kind {
            LaneKind::Loudness => {
                let mut out = Vec::new();
                if let (Some(lo), Some(hi)) = (st.short_term_low, st.short_term_high) {
                    out.push(Stat {
                        swatch: Swatch::Band,
                        caption: SHORT_TERM_RANGE_CAPTION,
                        value: format!("{}\u{2026}{}", fmt::number(lo, 0, false), fmt::number(hi, 0, false)),
                        color: p.text,
                    });
                }
                if let Some(m) = st.momentary_max {
                    out.push(Stat {
                        swatch: Swatch::Line,
                        caption: "M max",
                        value: fmt::number(m, 1, false),
                        color: p.text_dim,
                    });
                }
                out
            }
            LaneKind::Peak => st.true_peak_max.map_or(Vec::new(), |tp| {
                vec![Stat {
                    swatch: Swatch::None,
                    caption: "max",
                    value: fmt::number(tp, 1, true),
                    color: if tp > 0.0 { p.danger } else { p.text },
                }]
            }),
            LaneKind::Ear => st.level_a_max.map_or(Vec::new(), |a| {
                vec![Stat {
                    swatch: Swatch::None,
                    caption: "max \u{2248}",
                    value: fmt::number(a, 0, false),
                    color: if a >= 85.0 { p.warn } else { p.text },
                }]
            }),
            LaneKind::Tone => Vec::new(),
        }
    }

    /// Gives the width used.
    fn draw_swatch(p: &Palette, o: &mut OverlayContext, s: Swatch, x: f32, mid_y: f32) -> f32 {
        match s {
            Swatch::None => return 0.0,
            Swatch::Band => {
                o.fill_rect(Rect::new(x, mid_y - 2.5, 10.0, 6.0), with_alpha(p.accent, 0.40), 1.0);
                let y = mid_y - 2.5;
                o.line([x, y], [x + 10.0, y], mix(p.accent, WHITE, 0.72), 1.5, &[]);
            }
            Swatch::Line => {
                let color = with_alpha(mix(p.accent, WHITE, 0.15), 0.9);
                o.line([x, mid_y], [x + 10.0, mid_y], color, 1.0, &[]);
            }
        }
        14.0
    }

    fn draw_header(&mut self, base: &PanelBase, o: &mut OverlayContext) {
        let p = &base.palette;
        let y = self.header_mid_y;
        let (cap, stat) = (Self::cap_font(), Self::stat_font());
        let (left, middle) = (HAlign::Left, VAlign::Middle);
        let mut x = self.plot_x.0;
        x += o.text_tracked("SESSION", x, y, &cap, p.text_faint, left, middle, 1.0) + 8.0;
        let window = self.model.window_seconds;
        let minutes = if window >= 60.0 {
            format!("{} min", (window / 60.0).round() as i64)
        } else {
            format!("{} s", window as i64)
        };
        x += o.text(&minutes, x, y, &stat, p.text_dim, left, middle) + 18.0;
        let right = if self.wide() { self.size.0 - 12.0 } else { self.plot_x.1 };

        if self.readout_in_header(base) {
            let items = self.cursor_items(base);
            if !items.is_empty() {
                let compact = self.strip() || !self.wide();
                let pinned = base.cursor.is_some_and(|c| c.is_pinned);
                header::draw(o, &items, right, x, y, compact, pinned, p);
                return;
            }
        }
        if self.model.is_empty() {
            return;
        }
        // What the window holds, each count with the mark that stands for it in the lanes: the legend of the event
        // marks.
        let st = self.model.stats;
        #[derive(Clone, Copy)]
        enum Glyph {
            Clip,
            Over,
            Flag,
            Track,
            Pause,
        }
        let plural = |n: usize| if n == 1 { "" } else { "s" };
        let alarm = mix(p.danger, p.text, 0.35);
        let mut parts = vec![(
            Glyph::Clip,
            clip_count_text(st.clip_events),
            if st.clip_events == 0 { p.text_dim } else { alarm },
        )];
        if st.overs > 0 {
            parts.push((Glyph::Over, over_count_text(st.overs), alarm));
        }
        if st.flags > 0 {
            let text = format!("{} flag{}", st.flags, plural(st.flags));
            parts.push((Glyph::Flag, text, mix(p.warn, p.text, 0.25)));
        }
        if st.tracks > 0 {
            let text = format!("{} track start{}", st.tracks, plural(st.tracks));
            parts.push((Glyph::Track, text, p.text_dim));
        }
        let pauses = self.model.gaps.len();
        if pauses > 0 {
            parts.push((Glyph::Pause, format!("{pauses} pause{}", plural(pauses)), p.text_dim));
        }
        for (glyph, text, color) in parts {
            let w = o.measure(&text, &stat, 0.0);
            if !(x + 12.0 + w <= right) {
                break;
            }
            match glyph {
                Glyph::Clip => {
                    let color = if st.clip_events == 0 { p.text_faint } else { p.danger };
                    o.fill_rect(Rect::new(x + 3.0, y - 4.5, 2.0, 9.0), color, 0.0);
                }
                Glyph::Over => o.arc([x + 4.0, y], 3.0, 0.0, 2.0 * std::f32::consts::PI, p.danger, 1.2),
                Glyph::Flag => o.fill_rect(Rect::new(x, y - 2.0, 8.0, 4.0), with_alpha(p.warn, 0.9), 1.5),
                Glyph::Track => {
                    o.line(
                        [x + 1.0, y - 5.0],
                        [x + 1.0, y + 5.0],
                        with_alpha(p.text, 0.6),
                        1.0,
                        &[],
                    );
                    o.fill_rect(Rect::new(x + 1.5, y - 5.0, 6.0, 4.5), with_alpha(p.text, 0.85), 1.0);
                }
                Glyph::Pause => {
                    let c = p.text_dim;
                    o.line([x, y], [x + 2.5, y - 3.5], c, 1.2, &[]);
                    o.line([x + 2.5, y - 3.5], [x + 6.5, y + 3.5], c, 1.2, &[]);
                    o.line([x + 6.5, y + 3.5], [x + 9.0, y], c, 1.2, &[]);
                }
            }
            x += 13.0;
            x += o.text(&text, x, y, &stat, color, left, middle) + 18.0;
        }
    }

    // MARK: Spans and the axis

    /// The color of a kind of flag. The kinds of flag in the window, in alphabetical order of their ids: the index
    /// picks the hue.
    pub fn span_color(&self, base: &PanelBase, flag_id: &str) -> Color {
        let mut kinds: Vec<&str> = self.model.spans.iter().map(|s| s.flag_id.as_str()).collect();
        kinds.sort_unstable();
        kinds.dedup();
        let k = kinds.iter().position(|id| *id == flag_id).unwrap_or(0) % SPAN_HUES.len();
        if k == 0 {
            with_alpha(base.palette.warn, 1.0)
        } else {
            SPAN_HUES[k]
        }
    }

    fn draw_span_titles(&self, base: &PanelBase, o: &mut OverlayContext) {
        if self.strip() {
            return;
        }
        let font = fonts::ui(10.0, Weight::Medium);
        let (row_height, bar_height) = (self.span_row_height(), self.span_bar_height());
        for s in &self.model.spans {
            let sx0 = self.x_for_time(s.start).max(self.plot_x.0);
            let sx1 = self.x_for_time(s.end).min(self.plot_x.1);
            let room = sx1 - sx0 - 12.0;
            if !(room > 24.0) {
                continue;
            }
            let mid_y = self.spans_top + s.row as f32 * row_height + bar_height / 2.0;
            // The longest form of the title that fits in its own bar: the full title, or its first words.
            let mut words: Vec<&str> = s.title.split(' ').filter(|w| !w.is_empty()).collect();
            let mut text = s.title.clone();
            while !words.is_empty() && o.measure(&text, &font, 0.0) > room {
                words.pop();
                text = words.join(" ") + "\u{2026}";
            }
            if words.is_empty() {
                continue;
            }
            let color = mix(self.span_color(base, &s.flag_id), WHITE, 0.45);
            o.text(&text, sx0 + 7.0, mid_y, &font, color, HAlign::Left, VAlign::Middle);
        }
        if self.model.hidden_spans > 0 {
            let text = format!("+{}", self.model.hidden_spans);
            let yy = self.spans_top + (MAX_SPAN_ROWS - 1) as f32 * row_height + bar_height / 2.0;
            let (xx, h) = if self.wide() {
                (self.plot_x.1 + 12.0, HAlign::Left)
            } else {
                (self.plot_x.1 - 4.0, HAlign::Right)
            };
            let r = o.text_bounds(&text, xx, yy, &font, h, VAlign::Middle, 0.0);
            if o.is_free(r, 2.0, &[]) {
                o.text(&text, xx, yy, &font, base.palette.warn, h, VAlign::Middle);
            }
        }
    }

    fn draw_time_labels(&self, base: &PanelBase, o: &mut OverlayContext) {
        let p = &base.palette;
        let font = Self::number_font();
        let ly = self.axis_y + 8.0;
        let offset = self.utc_offset_seconds;
        // The right end: "now" while the record is live, else the time of the newest sample.
        let right_text = match self.model.samples.last() {
            Some(last) if !self.is_live => format::clock(unix_seconds(last.date), true, offset),
            _ => "now".to_string(),
        };
        let x1 = self.plot_x.1;
        let right_rect = o.text_bounds(&right_text, x1, ly, &font, HAlign::Right, VAlign::Top, 0.0);
        let color = if self.is_live { p.text } else { p.text_dim };
        o.text(&right_text, x1, ly, &font, color, HAlign::Right, VAlign::Top);
        let mut taken = vec![right_rect];
        // Newest first: near "now" the labels matter most, so they win the room.
        for m in self.minute_marks().into_iter().rev().filter(|m| m.labeled) {
            let text = format::clock(m.unix, false, offset);
            let r = o.text_bounds(&text, m.x, ly, &font, HAlign::Center, VAlign::Top, 0.0);
            let free = r.min_x() >= self.plot_x.0 - 14.0
                && r.max_x() <= x1
                && !taken.iter().any(|t| t.inset(-10.0, 0.0).intersects(&r));
            if !free {
                continue;
            }
            o.text(&text, m.x, ly, &font, p.text_dim, HAlign::Center, VAlign::Top);
            taken.push(r);
        }
    }

    // MARK: Readout

    /// The second under the cursor, line by line: time, loudness, peak and correlation, the level at the ear, the
    /// events.
    pub fn readout_lines(&self, i: usize) -> Vec<String> {
        let s = &self.model.samples[i];
        let clock = format::clock(unix_seconds(s.date), true, self.utc_offset_seconds);
        let mut lines = vec![format!("{clock}   {}", format::ago(self.model.newest_time - s.time))];
        if s.is_silent {
            lines.push("digital silence".to_string());
        } else {
            lines.push(format!(
                "S {}   M max {} LUFS",
                fmt::db(s.short_term_lufs, 1, false),
                fmt::db(s.momentary_max_lufs, 1, false)
            ));
            lines.push(format!(
                "TP {} dBTP   corr {}",
                fmt::db(s.true_peak_dbtp, 1, true),
                fmt::number(s.correlation, 2, true)
            ));
            if let Some(a) = s.level_a.filter(|a| *a > SplReading::FLOOR_DB + 1.0) {
                lines.push(format!("at the ear \u{2248} {} dB(A)", fmt::number(a, 0, false)));
            }
        }
        lines.extend(self.event_texts(i));
        lines
    }

    /// The flags that an event of sample `i` raised or cleared.
    fn changed_flags(&self, i: usize) -> Vec<&str> {
        self.model
            .events_of_sample(i)
            .into_iter()
            .filter(|e| {
                matches!(
                    e.kind,
                    SessionEventKind::StressFlagRaised | SessionEventKind::StressFlagCleared
                )
            })
            .map(flag_key)
            .collect()
    }

    /// What happened in the second of sample `i`, in words.
    pub fn event_texts(&self, i: usize) -> Vec<String> {
        let mut out = Vec::new();
        if let Some(g) = self.model.gap_before(i) {
            out.push(format!("after a pause of {}", format::duration(g.skipped_seconds)));
        }
        for e in self.model.events_of_sample(i) {
            out.push(match e.kind {
                SessionEventKind::TrackStart => "track start".to_string(),
                SessionEventKind::Clip => format!("clipped \u{00D7}{}", (e.value.round() as i64).max(1)),
                SessionEventKind::InterSampleOver => {
                    format!("true peak over {} dBTP", fmt::number(e.value, 1, true))
                }
                SessionEventKind::StressFlagRaised => format!("flag raised: {}", e.label),
                SessionEventKind::StressFlagCleared => format!("flag cleared: {}", e.label),
                SessionEventKind::SilenceStart => "silence starts".to_string(),
                SessionEventKind::SilenceEnd => "silence ends".to_string(),
            });
        }
        let changed = self.changed_flags(i);
        for s in self.model.spans_at_sample(i) {
            if !changed.contains(&s.flag_id.as_str()) {
                out.push(format!("flag up: {}", s.title));
            }
        }
        out
    }

    /// The bar hue of every line of `event_texts(i)`: `None` for what is not a flag.
    pub fn event_hues(&self, base: &PanelBase, i: usize) -> Vec<Option<Color>> {
        let mut out = Vec::new();
        if self.model.gap_before(i).is_some() {
            out.push(None);
        }
        for e in self.model.events_of_sample(i) {
            let is_flag = matches!(
                e.kind,
                SessionEventKind::StressFlagRaised | SessionEventKind::StressFlagCleared
            );
            out.push(is_flag.then(|| self.span_color(base, flag_key(e))));
        }
        let changed = self.changed_flags(i);
        for s in self.model.spans_at_sample(i) {
            if !changed.contains(&s.flag_id.as_str()) {
                out.push(Some(self.span_color(base, &s.flag_id)));
            }
        }
        out
    }
}

impl Panel for TimelinePanel {
    fn kind(&self) -> PanelKind {
        PanelKind::Timeline
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

    fn uses_analysis_frames(&self) -> bool {
        false
    }

    fn cursor_readout_follows_frames(&self) -> bool {
        false
    }

    fn accessibility_label(&self) -> String {
        "Session timeline".to_string()
    }

    fn accessibility_value(&self, _base: &PanelBase) -> String {
        self.model.accessibility_value()
    }

    fn static_signature(&mut self, base: &PanelBase) -> u64 {
        let mut h = Signature::new();
        h.add(base.static_signature())
            .add(self.model.has_level_a)
            .add(self.show_level_at_ear)
            .add(self.show_bands)
            .add(self.model.span_rows);
        h.finish()
    }

    fn dynamic_signature(&self, base: &PanelBase) -> u64 {
        let mut h = Signature::new();
        h.add(self.content_version).add(self.focus_index(base));
        h.finish()
    }

    fn layout_changed(&mut self, base: &mut PanelBase) {
        self.size = (base.width, base.height);
        self.compute_layout(base);
    }

    // MARK: Cursor

    fn cursor_at(&self, base: &PanelBase, point: [f32; 2]) -> Option<PanelCursor> {
        if !(self.over_lanes(point) && !self.model.is_empty() && self.plot_width() > 1.0) {
            return None;
        }
        let t = self.model.newest_time - ((self.plot_x.1 - point[0]) / self.points_per_second()) as f64;
        let i = self.model.sample_index(t, 2.0)?;
        // The cursor keeps the frequency it has (the timeline has no frequency axis); without one: the peak of the
        // frame.
        Some(PanelCursor {
            frequency_hz: base.cursor.map_or(self.fallback_frequency_hz, |c| c.frequency_hz),
            seconds_ago: Some((self.model.newest_time - self.model.samples[i].time).max(0.0)),
            source: PanelKind::Timeline,
            is_pinned: false,
        })
    }

    fn is_on_cursor(&self, base: &PanelBase, point: [f32; 2]) -> bool {
        match self.focus_x(base) {
            Some(fx) if self.over_lanes(point) => (point[0] - fx).abs() <= 5.0,
            _ => false,
        }
    }

    /// Only the time and the pin show here: a cursor that moves along a frequency axis elsewhere repaints nothing.
    fn cursor_change_shows(&self, base: &PanelBase, old: Option<PanelCursor>) -> bool {
        let shown = |c: Option<PanelCursor>| {
            let c = c?;
            Some((c.seconds_ago?.to_bits(), c.is_pinned, c.source == PanelKind::Timeline))
        };
        shown(old) != shown(base.cursor)
    }

    fn cursor_shows_in_text(&self, c: &PanelCursor) -> bool {
        c.seconds_ago.is_some()
    }

    fn combine_cursor_signature(&self, base: &PanelBase, c: &PanelCursor, h: &mut Signature) {
        h.add(c.seconds_ago.map(f64::to_bits))
            .add(c.seconds_ago.is_some() && c.is_pinned)
            .add(self.shows_pointer_readout(base));
    }

    /// A low strip has no room for a box at the pointer that does not cover the lanes: its readout is always the
    /// header row.
    fn shows_pointer_readout(&self, base: &PanelBase) -> bool {
        !(self.strip() || self.box_too_tall(base)) && self.pointer_readout_of_the_base(base)
    }

    /// The box at the pointer stays over the time axis: it never covers the axis labels.
    fn hover_label_floor(&self, _base: &PanelBase) -> Option<f32> {
        Some(self.axis_y - 2.0)
    }

    fn hover_label(&mut self, base: &PanelBase) -> Option<(Vec<String>, [f32; 2])> {
        if self.strip() || self.box_too_tall(base) {
            return None;
        }
        let (hv, i, fx) = (base.hover?, self.focus_index(base)?, self.focus_x(base)?);
        let shows = if base.cursor_linked {
            self.shows_pointer_readout(base)
        } else {
            self.over_lanes(hv)
        };
        shows.then(|| (self.readout_lines(i), [fx, hv[1]]))
    }

    fn cursor_items(&mut self, base: &PanelBase) -> Vec<CursorReadoutItem> {
        let Some(ago) = self.focus_seconds_ago(base) else {
            return Vec::new();
        };
        let Some(i) = self.focus_index(base) else {
            return vec![
                CursorReadoutItem::new(format::ago(ago), 0, Tone::Ghost),
                CursorReadoutItem::new("outside the record", 1, Tone::Dim),
            ];
        };
        let s = &self.model.samples[i];
        let clock = format::clock(unix_seconds(s.date), true, self.utc_offset_seconds);
        let mut items = vec![
            CursorReadoutItem::new(clock, 0, Tone::Value),
            CursorReadoutItem::new(format::ago(self.model.newest_time - s.time), 5, Tone::Ghost),
        ];
        if s.is_silent {
            items.push(CursorReadoutItem::new("digital silence", 1, Tone::Dim));
        } else {
            let value = |text: String, rank: u32| CursorReadoutItem::new(text, rank, Tone::Value);
            items.push(value(format!("S {} LUFS", fmt::db(s.short_term_lufs, 1, false)), 1));
            items.push(value(format!("M max {}", fmt::db(s.momentary_max_lufs, 1, false)), 4));
            items.push(value(format!("TP {} dBTP", fmt::db(s.true_peak_dbtp, 1, true)), 2));
            items.push(value(format!("corr {}", fmt::number(s.correlation, 2, true)), 7));
            if let Some(a) = s.level_a.filter(|a| *a > SplReading::FLOOR_DB + 1.0) {
                items.push(value(
                    format!("at the ear \u{2248} {} dB(A)", fmt::number(a, 0, false)),
                    6,
                ));
            }
        }
        // A flag's words carry the hue of its bar: on a low strip this row is the legend of the bars.
        let hues = self.event_hues(base, i);
        for (k, text) in self.event_texts(i).into_iter().enumerate() {
            items.push(CursorReadoutItem {
                swatch: hues[k],
                ..CursorReadoutItem::new(text, 3 + k as u32 * 5, Tone::Accent)
            });
        }
        items
    }

    // MARK: Shapes

    fn draw(&mut self, base: &mut PanelBase, pass: &mut wgpu::RenderPass<'_>) {
        let p = base.palette;
        let ctx = base.ctx.clone();
        let high_contrast = base.high_contrast();
        self.bulk.begin_frame(&mut base.arena, base.scale);
        let (x0, x1) = self.plot_x;
        let pps = self.points_per_second();
        let marks = self.minute_marks();
        let range = self.loudness_range;
        let lanes = self.lanes.clone();

        // ---- Lane beds, grids, silence.
        for l in &lanes {
            let r = l.rect;
            let (ry, rh) = (r.min_y(), r.h);
            base.rounded_rect([x0, ry, x1 - x0, rh], p.plot, 2.0);
            match l.kind {
                LaneKind::Loudness => {
                    let mut v = (range.0 / 6.0).floor() * 6.0;
                    while v > range.1 + 0.01 {
                        if v < range.0 - 0.01 {
                            let color = if v as i64 % 12 == 0 {
                                scaled_alpha(p.grid_major, 0.7)
                            } else {
                                p.grid_minor
                            };
                            base.hline(x0, x1, y_of(v, range, r), color);
                        }
                        v -= 6.0;
                    }
                }
                LaneKind::Peak => {
                    let zero_y = y_of(0.0, PEAK_RANGE, r);
                    base.rect([x0, ry, x1 - x0, zero_y - ry], with_alpha(p.danger, 0.13));
                    for v in [-6.0, -3.0, -9.0] {
                        let color = if v == -6.0 {
                            scaled_alpha(p.grid_major, 0.7)
                        } else {
                            p.grid_minor
                        };
                        base.hline(x0, x1, y_of(v, PEAK_RANGE, r), color);
                    }
                }
                LaneKind::Ear => {
                    for v in [55.0, 70.0, 100.0] {
                        base.hline(x0, x1, y_of(v, EAR_RANGE, r), p.grid_minor);
                    }
                }
                LaneKind::Tone => {}
            }
            for m in marks.iter().filter(|m| m.labeled) {
                if m.x > x0 + 1.0 && m.x < x1 - 1.0 {
                    base.vline(m.x, ry, ry + rh, p.grid_minor);
                }
            }
            // Digital silence: a neutral wash, so "no sound" does not look like "no record".
            for (from, to) in &self.model.silences {
                let (sx0, sx1) = (self.x_for_time(*from).max(x0), self.x_for_time(*to).min(x1));
                if sx1 > sx0 {
                    let wash = [1.0, 1.0, 1.0, if high_contrast { 0.11 } else { 0.06 }];
                    base.rect([sx0, ry, sx1 - sx0, rh], wash);
                }
            }
        }
        base.flush(pass);

        // ---- Tone cells and peak stems: many small quads, in their own batch.
        let has_samples = !self.model.samples.is_empty();
        if let Some(tone) = self.lane(LaneKind::Tone).filter(|_| has_samples) {
            self.draw_tone(base, tone.rect, pps);
        }
        if let Some(peak) = self.lane(LaneKind::Peak).filter(|_| has_samples) {
            self.draw_peak_stems(base, peak.rect, pps);
        }
        self.bulk.flush(pass, &base.arena, &ctx.shape_over, None);

        // ---- Curves.
        let samples = &self.model.samples;
        let many = samples.len() >= 2;
        let push = |base: &mut PanelBase, values: &[f32], range: Range| -> Option<FloatSlice> {
            let slice = base.arena.allocate_floats(values.len())?;
            for (out, v) in base.arena.floats_mut(slice).iter_mut().zip(values) {
                *out = unit(*v, range);
            }
            Some(slice)
        };
        let run_rect = |first: usize, n: usize, inner: Rect| {
            let a = self.x_for_time(samples[first].time);
            let b = self.x_for_time(samples[first + n - 1].time);
            [a, inner.min_y(), b - a, inner.h]
        };
        if let Some(l) = self.lane(LaneKind::Loudness).filter(|_| many) {
            self.set_scissor(base, pass, Some(l.rect));
            let inner = l.rect.inset(0.0, 1.0);
            let mut u = CurveUniforms::default();
            let short_term = |s: &SessionSample| is_measured(s.short_term_lufs).then_some(s.short_term_lufs);
            let momentary = |s: &SessionSample| is_measured(s.momentary_max_lufs).then_some(s.momentary_max_lufs);
            for (first, values) in self.runs(short_term) {
                let Some(b) = push(base, &values, range) else { continue };
                u.rect = run_rect(first, values.len(), inner);
                u.color = with_alpha(p.accent, if high_contrast { 0.62 } else { 0.50 });
                u.fill_top = 1.0;
                u.fill_bottom = 0.35;
                base.draw_curve_fill(pass, b, (1.0, 1.0), u, None);
            }
            for (first, values) in self.runs(momentary) {
                let Some(b) = push(base, &values, range) else { continue };
                u.rect = run_rect(first, values.len(), inner);
                u.color = with_alpha(mix(p.accent, WHITE, 0.15), 0.62);
                u.half_width = 0.5;
                base.draw_curve_line(pass, b, u, false, None);
            }
            for (first, values) in self.runs(short_term) {
                let Some(b) = push(base, &values, range) else { continue };
                u.rect = run_rect(first, values.len(), inner);
                u.color = mix(p.accent, WHITE, 0.72);
                u.half_width = 0.8;
                base.draw_curve_line(pass, b, u, false, None);
            }
        }
        if let Some(l) = self.lane(LaneKind::Ear).filter(|_| many) {
            let inner = l.rect.inset(0.0, 1.0);
            let level = |s: &SessionSample| s.level_a.filter(|a| *a > SplReading::FLOOR_DB + 1.0);
            let line85 = y_of(85.0, EAR_RANGE, l.rect);
            let mut u = CurveUniforms::default();
            for (first, values) in self.runs(level) {
                let Some(b) = push(base, &values, EAR_RANGE) else {
                    continue;
                };
                u.rect = run_rect(first, values.len(), inner);
                // Time over 85 dB(A): the part of the area over the line, in the warning color.
                let over = Rect::new(l.rect.min_x(), l.rect.min_y(), l.rect.w, line85 - l.rect.min_y());
                self.set_scissor(base, pass, Some(over));
                u.color = with_alpha(p.warn, 0.55);
                u.fill_top = 1.0;
                u.fill_bottom = 1.0;
                base.draw_curve_fill(pass, b, (1.0, 1.0), u, None);
                self.set_scissor(base, pass, Some(l.rect));
                u.color = with_alpha(p.spl_band, 0.13);
                u.fill_top = 1.0;
                u.fill_bottom = 0.4;
                base.draw_curve_fill(pass, b, (1.0, 1.0), u, None);
                u.color = with_alpha(p.spl_band, 0.95);
                u.half_width = 0.7;
                base.draw_curve_line(pass, b, u, false, None);
            }
        }
        self.set_scissor(base, pass, None);

        // ---- Reference lines over the traces.
        if let (Some(l), Some(t)) = (self.lane(LaneKind::Loudness), self.target_lufs) {
            if t > range.1 && t < range.0 {
                Self::dotted(base, x0, x1, y_of(t, range, l.rect), with_alpha(p.good, 0.9), 3.0, 7.0);
            }
        }
        if let Some(l) = self.lane(LaneKind::Peak) {
            base.hline(x0, x1, y_of(0.0, PEAK_RANGE, l.rect), with_alpha(p.danger, 0.95));
        }
        // Solid and quiet: the trace runs close to this line for hours, and dots on a line read as noise.
        if let Some(l) = self.lane(LaneKind::Ear) {
            let alpha = if high_contrast { 0.9 } else { 0.6 };
            base.hline(x0, x1, y_of(85.0, EAR_RANGE, l.rect), with_alpha(p.warn, alpha));
        }
        for l in &lanes {
            let rect = [x0 - 0.5, l.rect.min_y() - 0.5, x1 - x0 + 1.0, l.rect.h + 1.0];
            base.batch
                .gradient_rect(&mut base.arena, rect, p.grid_major, p.grid_major, 2.0, 0.0, 1.0);
        }

        // ---- Events.
        let peak_lane = self.lane(LaneKind::Peak);
        for e in &self.model.events {
            let ex = self.x_for_time(e.time);
            if !(ex >= x0 - 0.5 && ex <= x1 + 0.5) {
                continue;
            }
            match (e.kind, peak_lane) {
                (SessionEventKind::TrackStart, _) => {
                    // A pole through every lane and a small flag over them.
                    let top = self.flag_row_y + 1.0;
                    let alpha = if high_contrast { 0.8 } else { 0.5 };
                    self.vertical(base, ex, with_alpha(p.text_faint, alpha), false, false);
                    let fx = base.batch.snap(ex) + 0.5;
                    base.rounded_rect([fx, top, 6.0, 4.5], with_alpha(p.text, 0.85), 1.0);
                }
                (SessionEventKind::Clip, Some(l)) => {
                    // A tick that stands on the top edge of the Peak lane: visible over the red zone and over a
                    // full-height stem.
                    let w = (2.0 / base.scale).max(pps.min(2.0));
                    let tx = base.batch.snap(ex - w / 2.0);
                    base.rect([tx, l.rect.min_y() - 3.0, w, 8.0], with_alpha(p.danger, 1.0));
                }
                (SessionEventKind::InterSampleOver, Some(l)) => {
                    base.circle([ex, l.rect.min_y() + 0.5], 3.0, with_alpha(p.danger, 1.0), 1.2, 0.0);
                }
                _ => {}
            }
        }

        // Stress spans: bars under the lanes. The contract carries no severity, so every span is of the amber family;
        // each kind of flag has its own hue of it, so two kinds can be told apart where the bars have no room for a
        // title.
        let (strip, bar_height) = (self.strip(), self.span_bar_height());
        for s in &self.model.spans {
            let hue = self.span_color(base, &s.flag_id);
            let (sx0, sx1) = (self.x_for_time(s.start).max(x0), self.x_for_time(s.end).min(x1));
            if !(sx1 > sx0) {
                continue;
            }
            let sy = self.spans_top + s.row as f32 * self.span_row_height();
            let w = (sx1 - sx0).max(2.0);
            if strip {
                base.rounded_rect([sx0, sy, w, bar_height], with_alpha(hue, 0.95), 1.5);
            } else {
                let alpha = if high_contrast { 0.34 } else { 0.22 };
                base.rounded_rect([sx0, sy, w, bar_height], with_alpha(hue, alpha), 2.0);
                base.rounded_rect([sx0, sy, 2.0, bar_height], with_alpha(hue, 1.0), 1.0);
                if !s.is_open {
                    base.rect([sx1 - 1.0, sy, 1.0, bar_height], with_alpha(hue, 0.7));
                }
            }
        }

        // ---- Time axis: a base line with ticks at the round minutes; a zig-zag where the wall clock jumped.
        let ay = self.axis_y;
        let mut from = x0;
        let mut gap_xs: Vec<f32> = self
            .model
            .gaps
            .iter()
            .map(|g| self.x_for_time(g.time))
            .filter(|x| *x > x0 + 6.0 && *x < x1 - 6.0)
            .collect();
        gap_xs.sort_by(f32::total_cmp);
        for gx in &gap_xs {
            let gx = *gx;
            base.hline(from, gx - 5.0, ay, p.grid_strong);
            from = gx + 5.0;
            let c = p.text_dim;
            let mut zig = |a: [f32; 2], b: [f32; 2]| base.batch.line(&mut base.arena, a, b, 1.2, c, None, 0.0);
            zig([gx - 5.0, ay], [gx - 2.5, ay - 3.5]);
            zig([gx - 2.5, ay - 3.5], [gx + 2.5, ay + 3.5]);
            zig([gx + 2.5, ay + 3.5], [gx + 5.0, ay]);
            self.vertical(base, gx, with_alpha(p.text_faint, 0.55), true, false);
        }
        base.hline(from, x1, ay, p.grid_strong);
        for m in &marks {
            let near_gap = gap_xs.iter().any(|gx| (gx - m.x).abs() < 6.0);
            if !(m.x >= x0 - 0.5 && m.x <= x1 + 0.5) || near_gap {
                continue;
            }
            let (length, color) = if m.labeled {
                (5.0, p.text_faint)
            } else {
                (3.0, p.grid_strong)
            };
            base.vline(m.x, ay, ay + length, color);
        }
        base.vline(x1, ay, ay + 5.0, p.text_faint);

        // ---- Cursor: a hairline through every lane, with a dot on each trace at that second.
        if let Some(xx) = self.focus_x(base) {
            let top = self.flag_row_y + 1.0;
            let color = if base.cursor_linked {
                base.cursor_line_color()
            } else {
                [1.0, 1.0, 1.0, 0.5]
            };
            self.vertical(base, xx, color, false, true);
            if base.cursor_linked && base.cursor.is_some_and(|c| c.is_pinned) {
                base.draw_pin_head(xx, top + 3.5);
            }
            if let Some(s) = self.focus_index(base).map(|i| &samples[i]).filter(|s| !s.is_silent) {
                let sx = self.x_for_time(s.time);
                let mut dot = |yy: f32, c: Color| {
                    base.circle([sx, yy], 3.0, with_alpha(p.plot, 1.0), 0.0, 0.0);
                    base.circle([sx, yy], 3.0, c, 1.5, 0.0);
                };
                if let Some(l) = self.lane(LaneKind::Loudness).filter(|_| is_measured(s.short_term_lufs)) {
                    dot(y_of(s.short_term_lufs, range, l.rect), mix(p.accent, WHITE, 0.72));
                }
                if let Some(l) = self.lane(LaneKind::Peak).filter(|_| is_measured(s.true_peak_dbtp)) {
                    let color = if s.true_peak_dbtp > 0.0 { p.danger } else { p.text };
                    dot(y_of(s.true_peak_dbtp, PEAK_RANGE, l.rect), color);
                }
                if let (Some(l), Some(a)) = (self.lane(LaneKind::Ear), s.level_a) {
                    if a > SplReading::FLOOR_DB + 1.0 {
                        dot(y_of(a, EAR_RANGE, l.rect), p.spl_band);
                    }
                }
            }
        }
        base.flush(pass);
    }

    // MARK: Text

    fn draw_static(&mut self, base: &PanelBase, o: &mut OverlayContext) {
        let p = &base.palette;
        let (cap, stat, number) = (Self::cap_font(), Self::stat_font(), Self::number_font());
        let (x0, x1) = self.plot_x;
        for l in &self.lanes {
            let (name, unit_text) = (Self::lane_name(l.kind), Self::lane_unit(l.kind));
            if self.wide() {
                let (x, y) = (x1 + 12.0, l.rect.min_y() + 1.0);
                let w = o.text_tracked(name, x, y, &cap, p.text_faint, HAlign::Left, VAlign::Top, 1.0);
                o.text(
                    unit_text,
                    x + w + 6.0,
                    y,
                    &stat,
                    p.text_faint,
                    HAlign::Left,
                    VAlign::Top,
                );
            } else {
                let y = l.caption_y;
                let w = o.text_tracked(name, x0, y, &cap, p.text_faint, HAlign::Left, VAlign::Bottom, 1.0);
                o.text(
                    unit_text,
                    x0 + w + 6.0,
                    y,
                    &stat,
                    p.text_faint,
                    HAlign::Left,
                    VAlign::Bottom,
                );
            }
            if l.kind == LaneKind::Tone && l.rect.h >= 30.0 {
                let x = x0 - 6.0;
                o.text(
                    "Air",
                    x,
                    l.rect.min_y() + 5.0,
                    &number,
                    p.text_faint,
                    HAlign::Right,
                    VAlign::Middle,
                );
                o.text(
                    "Sub",
                    x,
                    l.rect.max_y() - 5.0,
                    &number,
                    p.text_faint,
                    HAlign::Right,
                    VAlign::Middle,
                );
            }
        }
    }

    fn draw_dynamic(&mut self, base: &mut PanelBase, o: &mut OverlayContext) {
        let base: &PanelBase = base;
        let p = &base.palette;
        let (cap, stat, small) = (Self::cap_font(), Self::stat_font(), Self::number_font());
        let (x0, x1) = self.plot_x;

        // ---- Scales in the left gutter.
        for l in &self.lanes {
            let range = match l.kind {
                LaneKind::Loudness => self.loudness_range,
                LaneKind::Peak => PEAK_RANGE,
                LaneKind::Ear => EAR_RANGE,
                LaneKind::Tone => continue,
            };
            let mut placed: Vec<f32> = Vec::new();
            for v in self.scale_values(l.kind, l.rect.h) {
                let yy = y_of(v, range, l.rect)
                    .max(l.rect.min_y() + 4.0)
                    .min(l.rect.max_y() - 4.0);
                if placed.iter().any(|y| (y - yy).abs() < 12.0) {
                    continue;
                }
                placed.push(yy);
                let color = if l.kind == LaneKind::Peak && v == 0.0 {
                    mix(p.danger, p.text, 0.35)
                } else if l.kind == LaneKind::Ear && v == 85.0 {
                    mix(p.warn, p.text, 0.25)
                } else {
                    p.text_dim
                };
                o.text(
                    &fmt::number(v, 0, false),
                    x0 - 6.0,
                    yy,
                    &small,
                    color,
                    HAlign::Right,
                    VAlign::Middle,
                );
            }
        }

        // ---- Lane numbers: what the window held. Right column on wide panels, the caption row on narrow ones.
        for l in &self.lanes {
            let list = self.stats(base, l.kind);
            if self.wide() {
                let lx = x1 + 12.0;
                for (k, s) in list.iter().enumerate() {
                    let yy = l.rect.min_y() + 1.0 + (k + 1) as f32 * 14.0;
                    if !(yy + 10.0 <= l.rect.max_y() + 3.0) {
                        break;
                    }
                    let mut xx = lx;
                    xx += Self::draw_swatch(p, o, s.swatch, xx, yy + 4.5);
                    let caption = format!("{} ", s.caption);
                    xx += o.text(&caption, xx, yy, &stat, p.text_faint, HAlign::Left, VAlign::Top);
                    o.text(&s.value, xx, yy, &stat, s.color, HAlign::Left, VAlign::Top);
                }
            } else {
                let mut right = x1;
                let name_end = x0
                    + o.measure(Self::lane_name(l.kind), &cap, 1.0)
                    + o.measure(Self::lane_unit(l.kind), &stat, 0.0)
                    + 20.0;
                for s in list.iter().rev() {
                    let caption = format!("{} ", s.caption);
                    let w = o.measure(&s.value, &stat, 0.0)
                        + o.measure(&caption, &stat, 0.0)
                        + if s.swatch == Swatch::None { 0.0 } else { 14.0 };
                    if !(right - w >= name_end) {
                        continue;
                    }
                    let y = l.caption_y;
                    right -= o.text(&s.value, right, y, &stat, s.color, HAlign::Right, VAlign::Bottom);
                    right -= o.text(&caption, right, y, &stat, p.text_faint, HAlign::Right, VAlign::Bottom);
                    if s.swatch != Swatch::None {
                        right -= 14.0;
                        Self::draw_swatch(p, o, s.swatch, right, y - 4.0);
                    }
                    right -= 12.0;
                }
            }
        }

        self.draw_header(base, o);
        self.draw_span_titles(base, o);
        self.draw_time_labels(base, o);

        if let Some(first) = self.lanes.first().filter(|_| self.model.is_empty()) {
            let text = "The timeline fills as you listen";
            let font = fonts::ui(if self.strip() { 12.0 } else { 13.0 }, Weight::Medium);
            let cx = (x0 + x1) / 2.0;
            let cy = if self.lanes.len() > 1 && first.rect.h < 40.0 {
                (first.rect.min_y() + self.lanes_bottom) / 2.0
            } else {
                first.rect.mid_y()
            };
            let w = o.measure(text, &font, 0.0);
            o.fill_rect(Rect::new(cx - w / 2.0 - 10.0, cy - 10.0, w + 20.0, 20.0), p.plot, 4.0);
            o.text(text, cx, cy, &font, p.text_dim, HAlign::Center, VAlign::Middle);
        }
    }
}
