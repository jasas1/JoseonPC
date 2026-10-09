//! The spectrum panel.
//!
//! Port source: `Sources/JoseonRender/SpectrumRenderer.swift`.
//!
//! Not ported yet: the A/B comparison (reference trace and difference lane of
//! `SpectrumRenderer+Comparison.swift`). It arrives with port step 14.

mod labels;
mod smooth;

use crate::arena::FloatSlice;
use crate::axis::{LogAxis, ResampleTable};
use crate::context::Lut;
use crate::contracts::{CursorHistorySlice, LevelAxis, PanelCursor, PanelKind, SpectrumViewOptions};
use crate::cursor;
use crate::fmt;
use crate::gpu::{Color, CurveUniforms};
use crate::palette::{mix, rgba, scaled_alpha, with_alpha, Palette, WHITE};
use crate::panel::{Panel, PanelBase, Signature};
use crate::text::fonts::{self, Font, Weight};
use crate::text::Rect;
use joseon_core::contracts::{AnalysisFrame, HeadphoneReading, PeakReading, SplReading, StressFlag, StressSeverity};
use smooth::{box_blur, smooth_highs, SmoothContext};
use std::any::Any;

/// Points of a curve, at most.
pub const MAX_POINTS: usize = 2048;

/// The curves resampled to the plot.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Slot {
    Mid,
    Side,
    Average,
    Left,
    Right,
    PeakHold,
    AtEar,
    Target,
    Response,
    Ghost,
}

impl Slot {
    pub const ALL: [Slot; 10] = [
        Slot::Mid,
        Slot::Side,
        Slot::Average,
        Slot::Left,
        Slot::Right,
        Slot::PeakHold,
        Slot::AtEar,
        Slot::Target,
        Slot::Response,
        Slot::Ghost,
    ];

    /// The name the tests ask a curve by. Same strings as the macOS app.
    pub fn name(self) -> &'static str {
        match self {
            Slot::Mid => "mid",
            Slot::Side => "side",
            Slot::Average => "average",
            Slot::Left => "left",
            Slot::Right => "right",
            Slot::PeakHold => "peakHold",
            Slot::AtEar => "atEar",
            Slot::Target => "target",
            Slot::Response => "response",
            Slot::Ghost => "ghost",
        }
    }
}

/// Where a note label went: its dot, its ink box and whether a leader line ties it to the dot. For tests.
#[derive(Debug, Clone, PartialEq)]
pub struct PeakLabelPlacement {
    pub name: String,
    pub dot: [f32; 2],
    pub ink: Rect,
    pub leader: bool,
}

/// The values of the newest frame at the cursor frequency.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct CursorValues {
    pub mid: Option<f32>,
    pub left: Option<f32>,
    pub right: Option<f32>,
    pub side: Option<f32>,
    pub at_ear: Option<f32>,
    pub at_ear_delta: Option<f32>,
    /// The center of the third-octave band and its level at the eardrum.
    pub spl: Option<(f32, f32)>,
    pub then: Option<f32>,
}

pub struct SpectrumPanel {
    options: SpectrumViewOptions,
    auto_range: bool,
    show_stress_bands: bool,
    level_axis: LevelAxis,
    /// True while the SPL layer is drawn: `DbSpl`, and the frame carries an SPL reading with its third-octave bands.
    spl_shown: bool,
    /// dB SPL = dBFS + this, on the right-hand axis. Whole dB, from the reading.
    spl_offset: f32,
    auto_declutter: bool,
    /// What the legend row named when the text was last laid out. The GPU pass
    /// reads it: a trace with no name is not drawn.
    legend_named: Vec<Slot>,

    /// The headphone overlay is wanted but not drawn: the panel has no room for the legend row of the band.
    hp_hidden: bool,
    hp_shown: bool,
    /// The "At ear" trace is drawn: the band is shown and the legend row has room for the name of the trace.
    at_ear_shown: bool,
    hp_legend_fit_key: String,
    hp_legend_fit: (bool, bool),

    // The plot starts at 20 Hz: below that the analyzer has one or two bins.
    axis: LogAxis,
    hue_lut: Option<Lut>,
    plot: Rect,
    point_count: usize,

    // Auto range state.
    /// Shown top, animated.
    range_top: f32,
    /// Where the top is going.
    range_target: f32,
    range_snapped: bool,
    lower_for: f64,
    higher_for: f64,

    // Curves resampled to the plot, made once per analysis frame (not per display tick). Allocated once.
    store: Vec<f32>,
    /// Four work rows of `MAX_POINTS`.
    scratch: Vec<f32>,
    /// The reference line of the fill (plot heights at the left and the right end).
    fill_reference: (f32, f32),
    fill_reference_snapped: bool,
    slot_valid: [bool; Slot::ALL.len()],
    curves_valid: bool,
    table: Option<ResampleTable>,
    /// The ghost trace has its own bins (the rows of the spectrogram history), so its own table.
    ghost_table: Option<ResampleTable>,
    /// At ear = Mid + (Response - Target) per bin, kept between frames.
    at_ear_source: Vec<f32>,
    spl_scratch: [f32; MAX_BANDS],

    secondary_peak_cache: Vec<PeakReading>,
    stress_span_cache: Vec<(StressFlag, f32, f32)>,
    annotations_valid: bool,
    peak_label_placements: Vec<PeakLabelPlacement>,
}

const MAX_BANDS: usize = 48;

impl Default for SpectrumPanel {
    fn default() -> Self {
        Self::new()
    }
}

impl SpectrumPanel {
    pub const AUTO_SPAN_DB: f32 = 72.0;
    // Headphone curves: their own band, the top 25 % of the plot, with their own scale (+-14 dB over the band, ticks to +-12).
    pub const HP_BAND_FRACTION: f32 = 0.25;
    const HP_AXIS_DB: f32 = 12.0;
    pub const HP_BAND_HALF_SPAN_DB: f32 = 14.0;
    /// Response, target and error are drawn at 60 % of their colors: reference curves, not data of the music.
    pub const HP_BAND_ALPHA: f32 = 0.60;
    const HEADPHONE_ROW_HEIGHT: f32 = 18.0;
    /// The ghost trace: dashed 6 on, 4 off (period, duty).
    pub const GHOST_DASH: (f32, f32) = (10.0, 0.6);
    /// The spectrogram history holds nothing under its floor.
    pub const GHOST_FLOOR_DB: f32 = -75.0;
    pub const LEFT_RIGHT_SMOOTHING_HINT: &'static str = "L/R 1/6 oct";
    pub const AT_EAR_VS_TARGET_LEGEND: &'static str = "At ear vs target";
    pub const AT_EARDRUM_LEGEND: &'static str = "At eardrum (incl. ear gain)";
    /// Legend name of the `average` curve: a long-term average, slower than the peak hold decay.
    pub const AVERAGE_LEGEND: &'static str = "Long-term";
    pub const SPL_LEGENDS: [&'static str; 4] = [
        "dB SPL at eardrum \u{00B7} 1/3 oct \u{00B7} \u{2248}",
        "SPL at eardrum \u{00B7} 1/3 oct \u{00B7} \u{2248}",
        "SPL 1/3 oct \u{2248}",
        "SPL \u{2248}",
    ];
    pub const NOT_CALIBRATED_NOTES: [&'static str; 3] =
        ["dB SPL: not calibrated", "SPL: not calibrated", "not calibrated"];

    pub fn new() -> Self {
        Self {
            options: SpectrumViewOptions::default(),
            auto_range: true,
            show_stress_bands: true,
            level_axis: LevelAxis::DbFs,
            spl_shown: false,
            spl_offset: 100.0,
            auto_declutter: true,
            legend_named: Vec::new(),
            hp_hidden: false,
            hp_shown: false,
            at_ear_shown: false,
            hp_legend_fit_key: String::new(),
            hp_legend_fit: (false, false),
            axis: LogAxis {
                min_hz: 20.0,
                max_hz: 20_000.0,
            },
            hue_lut: None,
            plot: Rect::default(),
            point_count: 2,
            range_top: 0.0,
            range_target: 0.0,
            range_snapped: false,
            lower_for: 0.0,
            higher_for: 0.0,
            store: vec![0.0; MAX_POINTS * Slot::ALL.len()],
            scratch: vec![0.0; MAX_POINTS * 4],
            fill_reference: (0.6, 0.3),
            fill_reference_snapped: false,
            slot_valid: [false; Slot::ALL.len()],
            curves_valid: false,
            table: None,
            ghost_table: None,
            at_ear_source: Vec::new(),
            spl_scratch: [0.0; MAX_BANDS],
            secondary_peak_cache: Vec::new(),
            stress_span_cache: Vec::new(),
            annotations_valid: false,
            peak_label_placements: Vec::new(),
        }
    }

    // MARK: Options

    pub fn options(&self) -> SpectrumViewOptions {
        self.options
    }

    pub fn set_options(&mut self, base: &mut PanelBase, options: SpectrumViewOptions) {
        if options != self.options {
            self.options = options;
            self.curves_valid = false;
            base.needs_display = true;
        }
    }

    /// Slow automatic dB range: a 72 dB window (or the span of the options when
    /// that is smaller) whose top follows the music in 6 dB steps with
    /// hysteresis. Off = exactly `options.min_db ... options.max_db`.
    pub fn set_auto_range(&mut self, base: &mut PanelBase, on: bool) {
        if on != self.auto_range {
            self.auto_range = on;
            self.range_snapped = false;
            self.curves_valid = false;
            base.needs_display = true;
        }
    }

    /// Shades the frequency span of every headphone stress flag that has one, with its plot label.
    pub fn set_show_stress_bands(&mut self, base: &mut PanelBase, on: bool) {
        if on != self.show_stress_bands {
            self.show_stress_bands = on;
            self.annotations_valid = false;
            base.needs_display = true;
        }
    }

    /// `DbSpl`: the third-octave levels at the eardrum as a stepped bar layer
    /// behind the curves, on a right-hand dB SPL axis. The curves stay in dBFS.
    pub fn set_level_axis(&mut self, base: &mut PanelBase, axis: LevelAxis) {
        if axis != self.level_axis {
            self.level_axis = axis;
            self.spl_shown = false;
            self.layout(base);
            base.needs_display = true;
        }
    }

    /// Takes away the traces the question on screen does not need: Peak hold
    /// and Long-term while a timed cursor shows its ghost trace, and any of
    /// L / R, Side, Peak hold and Long-term that the legend row had no room to
    /// name. Off = every trace of `options` is drawn, named or not.
    pub fn set_auto_declutter(&mut self, base: &mut PanelBase, on: bool) {
        if on != self.auto_declutter {
            self.auto_declutter = on;
            self.curves_valid = false;
            base.needs_display = true;
        }
    }

    pub fn spl_shown(&self) -> bool {
        self.spl_shown
    }

    pub fn spl_offset(&self) -> f32 {
        self.spl_offset
    }

    pub fn headphone_band_hidden(&self) -> bool {
        self.hp_hidden
    }

    fn is_named(&self, slot: Slot) -> bool {
        !self.auto_declutter || self.legend_named.contains(&slot)
    }

    /// L and R are wanted: the option is on.
    fn wants_left_right(&self) -> bool {
        self.options.show_left_right
    }

    /// Peak hold and Long-term are wanted: not while the ghost trace asks "then against now".
    fn wants_peak_hold(&self, base: &PanelBase) -> bool {
        self.options.show_peak_hold && !(self.auto_declutter && self.ghost_slice(base).is_some())
    }

    fn wants_long_term(&self, base: &PanelBase) -> bool {
        self.options.show_average && !(self.auto_declutter && self.ghost_slice(base).is_some())
    }

    // MARK: Layout

    /// Compact mode uses smaller type and a slimmer header (small cards).
    fn compact(base: &PanelBase) -> bool {
        base.width < 460.0 || base.height < 200.0
    }

    /// The headphone legend is a second header row over the plot (never a plate over the data). Hidden in compact mode.
    fn shows_headphone_legend(&self, base: &PanelBase) -> bool {
        !Self::compact(base) && self.hp_shown
    }

    /// Legends and readouts are in this row over the plot, at every size.
    fn header_height(base: &PanelBase) -> f32 {
        if Self::compact(base) {
            17.0
        } else {
            22.0
        }
    }

    fn axis_font(base: &PanelBase) -> Font {
        fonts::mono(if Self::compact(base) { 10.0 } else { 11.0 }, Weight::Regular)
    }

    fn caption_font(base: &PanelBase) -> Font {
        fonts::ui(if Self::compact(base) { 10.0 } else { 11.0 }, Weight::Semibold)
    }

    /// Who gets the room. Gives true when something changed.
    fn resolve_bands(&mut self, base: &PanelBase) -> bool {
        let reading = base.frame.as_ref().and_then(|f| f.headphone.as_ref());
        let wants_hp = self.options.show_headphone_overlay && reading.is_some();
        // The headphone band is drawn only with its words: the legend row over the plot and the `dB rel` axis beside
        // it. A compact card has neither; a narrow panel may have no room for the entries of the row.
        let fit = match reading {
            Some(reading) if wants_hp => self.headphone_legend_fit(base, reading),
            _ => (false, false),
        };
        let hp = wants_hp && fit.0;
        let hidden = wants_hp && !hp;
        let at_ear = hp && fit.1;
        let changed = hidden != self.hp_hidden || hp != self.hp_shown || at_ear != self.at_ear_shown;
        self.hp_hidden = hidden;
        self.hp_shown = hp;
        self.at_ear_shown = at_ear;
        changed
    }

    /// Does the headphone legend row have room for the entries of the band
    /// (name, Response, Target, Error), and for the at-ear entry after them?
    /// Measured once per headphone and width.
    fn headphone_legend_fit(&mut self, base: &PanelBase, hp: &HeadphoneReading) -> (bool, bool) {
        if Self::compact(base) {
            return (false, false);
        }
        let key = format!("{}|{}||{}", hp.model_name, hp.has_target, base.width);
        if key == self.hp_legend_fit_key {
            return self.hp_legend_fit;
        }
        let entries = Self::headphone_legend_entries(hp);
        let limit = base.width - 6.0 - Self::axis_font(base).measure("dB rel", 0.0) - 10.0;
        let mut x = 44.0 + 2.0;
        let (mut band, mut at_ear) = (true, true);
        for (i, (label, bold, swatch)) in entries.iter().enumerate() {
            let font = fonts::ui(11.0, if *bold { Weight::Semibold } else { Weight::Regular });
            let w = if *swatch { 21.0 } else { 0.0 } + font.measure(label, 0.0);
            if x + w > limit {
                if i == entries.len() - 1 {
                    at_ear = false;
                } else {
                    band = false;
                }
            }
            x += w + 14.0;
        }
        self.hp_legend_fit_key = key;
        self.hp_legend_fit = (band, band && at_ear);
        self.hp_legend_fit
    }

    /// The entries of the headphone legend row, in order: label, bold, with a swatch. The last one is the at-ear trace.
    fn headphone_legend_entries(hp: &HeadphoneReading) -> Vec<(String, bool, bool)> {
        let mut out = vec![
            (hp.model_name.clone(), true, false),
            ("Response".to_string(), false, true),
        ];
        if hp.has_target {
            out.push(("Target".to_string(), false, true));
            out.push(("Error".to_string(), false, true));
        }
        let at_ear = if hp.has_target {
            Self::AT_EAR_VS_TARGET_LEGEND
        } else {
            Self::AT_EARDRUM_LEGEND
        };
        out.push((at_ear.to_string(), false, true));
        out
    }

    fn layout(&mut self, base: &mut PanelBase) {
        self.resolve_bands(base);
        let compact = Self::compact(base);
        let (w, h) = (base.width, base.height);
        let left = if compact { 32.0 } else { 44.0 };
        let right = if self.spl_shown {
            if compact {
                27.0
            } else {
                48.0
            }
        } else if compact {
            8.0
        } else if self.hp_shown {
            40.0
        } else {
            14.0
        };
        let top = if compact { 5.0 } else { 8.0 }
            + Self::header_height(base)
            + if self.shows_headphone_legend(base) {
                Self::HEADPHONE_ROW_HEIGHT
            } else {
                0.0
            };
        let bottom = if compact { 20.0 } else { 26.0 };
        self.plot = Rect::new(left, top, (w - left - right).max(10.0), (h - top - bottom).max(10.0));
        self.point_count = ((self.plot.w * base.scale / 2.0) as usize).max(64).min(MAX_POINTS - 1);
        self.curves_valid = false;
        self.annotations_valid = false;
    }

    /// Range of the music scale in dB: (min, max).
    pub fn music_range(&self) -> (f32, f32) {
        if !self.auto_range {
            return (self.options.min_db, self.options.max_db);
        }
        let span = (self.options.max_db - self.options.min_db).min(Self::AUTO_SPAN_DB);
        (self.range_top - span, self.range_top)
    }

    /// Range of the whole plot in dB. With the headphone overlay the top 25 %
    /// of the plot is the band of the headphone curves: the music scale keeps
    /// its range and takes the lower 75 %, so the two never share a pixel row.
    pub fn shown_range(&self) -> (f32, f32) {
        let m = self.music_range();
        if !self.hp_shown {
            return m;
        }
        (
            m.0,
            m.1 + (m.1 - m.0) * Self::HP_BAND_FRACTION / (1.0 - Self::HP_BAND_FRACTION),
        )
    }

    fn lo(&self) -> f32 {
        self.shown_range().0
    }

    fn hi(&self) -> f32 {
        self.shown_range().1
    }

    fn music_top(&self) -> f32 {
        self.music_range().1
    }

    fn hp_band(&self) -> Rect {
        Rect::new(
            self.plot.min_x(),
            self.plot.min_y(),
            self.plot.w,
            self.plot.h * Self::HP_BAND_FRACTION,
        )
    }

    /// y of a relative level (dB re 1 kHz) inside the headphone band.
    fn hp_y(&self, d: f32) -> f32 {
        let band = self.hp_band();
        band.min_y() + band.h * (Self::HP_BAND_HALF_SPAN_DB - d) / (2.0 * Self::HP_BAND_HALF_SPAN_DB)
    }

    /// Ticks of the right axis that the band has room for.
    fn hp_ticks(&self) -> Vec<f32> {
        let h = self.hp_band().h;
        let a = Self::HP_AXIS_DB;
        if h >= 84.0 {
            vec![a, a / 2.0, 0.0, -a / 2.0, -a]
        } else if h >= 44.0 {
            vec![a, 0.0, -a]
        } else {
            vec![0.0]
        }
    }

    /// The bottom of the frequency axis.
    fn axis_bottom(&self) -> f32 {
        self.plot.max_y()
    }

    // MARK: State

    /// The right-hand axis: dB SPL = dBFS + `spl_offset`. The offset is what
    /// the reading itself says about a 1 kHz tone: band level at the eardrum
    /// minus band level in dBFS RMS (median over the bands with signal), minus
    /// 3.01 dB, because the curves read a full-scale sine as 0 dBFS and the
    /// bands read it as -3.01 dBFS RMS. Whole dB, and it moves only with the
    /// calibration: the axis does not follow the music.
    fn update_spl_axis(&mut self, base: &mut PanelBase, f: &AnalysisFrame) {
        let mut on = false;
        if let (LevelAxis::DbSpl, Some(s), Some(t)) = (self.level_axis, &f.spl, &f.third_octave) {
            let n = s
                .band_levels_eardrum
                .len()
                .min(t.left.len())
                .min(t.right.len())
                .min(t.centers_hz.len());
            on = n >= 3;
            let mut count = 0;
            for b in 0..n.min(MAX_BANDS) {
                let level = t.left[b].max(t.right[b]);
                if level > -100.0 && s.band_levels_eardrum[b] > SplReading::FLOOR_DB + 1.0 {
                    self.spl_scratch[count] = s.band_levels_eardrum[b] - level;
                    count += 1;
                }
            }
            if count >= 3 {
                self.spl_scratch[..count].sort_by(f32::total_cmp);
                // Hysteresis: a median that stands at x.5 must not move the axis (and its labels) with every frame.
                let offset = self.spl_scratch[count / 2] - 3.01;
                if (offset - self.spl_offset).abs() > 0.75 {
                    self.spl_offset = offset.round();
                    base.needs_display = true;
                }
            }
        }
        if on != self.spl_shown {
            self.spl_shown = on;
            self.layout(base);
            base.needs_display = true;
        }
    }

    fn update_auto_range(&mut self, f: &AnalysisFrame, dt: f64) {
        if !self.auto_range {
            return;
        }
        let s = &f.spectrum;
        let n = s.peak_hold.len().min(s.frequencies.len()).min(s.mid.len());
        let mut peak = -1000f32;
        let mut i = 0;
        while i < n {
            if s.frequencies[i] >= self.axis.min_hz && s.frequencies[i] <= self.axis.max_hz {
                peak = peak.max(s.peak_hold[i].max(s.mid[i]));
            }
            i += 2;
        }
        let ceiling = self.options.max_db.min(0.0);
        if !(peak > -110.0) {
            if !self.range_snapped {
                self.range_top = (-6f32).min(ceiling);
                self.range_target = self.range_top;
            }
            return;
        }
        // 6 dB of air over the highest peak, on the 6 dB grid.
        let want = (((peak + 6.0) / 6.0).ceil() * 6.0).max(-48.0).min(ceiling);
        if !self.range_snapped {
            self.range_snapped = true;
            self.range_top = want;
            self.range_target = want;
            return;
        }
        // Up fast (the curve must not leave the plot), down only after the music stayed lower for a while.
        if want > self.range_target {
            self.higher_for += dt;
            self.lower_for = 0.0;
        } else if want < self.range_target {
            self.lower_for += dt;
            self.higher_for = 0.0;
        } else {
            self.higher_for = 0.0;
            self.lower_for = 0.0;
        }
        if self.higher_for > 0.25 || self.lower_for > 5.0 {
            self.range_target = want;
            self.higher_for = 0.0;
            self.lower_for = 0.0;
        }
        if self.range_top != self.range_target {
            let k = (1.0 - (-dt / 0.22).exp()) as f32;
            self.range_top += (self.range_target - self.range_top) * k;
            if (self.range_target - self.range_top).abs() < 0.04 {
                self.range_top = self.range_target;
            }
        }
    }

    // MARK: Mapping

    pub fn x_for(&self, hz: f32) -> f32 {
        self.plot.min_x() + self.axis.position(hz) * self.plot.w
    }

    fn y_for(&self, db: f32) -> f32 {
        let (lo, hi) = self.shown_range();
        self.plot.max_y() - (db - lo) / (hi - lo) * self.plot.h
    }

    fn db_step(&self, base: &PanelBase) -> f32 {
        let range = self.hi() - self.lo();
        let min_gap = if Self::compact(base) { 22.0 } else { 34.0 };
        [6.0, 12.0, 24.0, 48.0]
            .into_iter()
            .find(|step| self.plot.h * step / range >= min_gap)
            .unwrap_or(48.0)
    }

    // MARK: Curves

    fn slot(&self, slot: Slot) -> &[f32] {
        let start = slot as usize * MAX_POINTS;
        &self.store[start..start + MAX_POINTS]
    }

    fn slot_mut(&mut self, slot: Slot) -> &mut [f32] {
        let start = slot as usize * MAX_POINTS;
        &mut self.store[start..start + MAX_POINTS]
    }

    fn valid(&self, slot: Slot) -> bool {
        self.slot_valid[slot as usize]
    }

    fn build(&mut self, slot: Slot, src: &[f32], min_db: f32, max_db: f32) {
        let Some(table) = self.table.take() else { return };
        if src.len() >= table.source_count {
            table.apply(src, min_db, max_db, 0.0, self.slot_mut(slot));
            self.slot_valid[slot as usize] = true;
        }
        self.table = Some(table);
    }

    /// One slot to change and another slot to read, both in the store.
    fn two_slots(store: &mut [f32], change: Slot, read: Slot) -> (&mut [f32], &[f32]) {
        let (a, b) = (change as usize * MAX_POINTS, read as usize * MAX_POINTS);
        if a < b {
            let (first, second) = store.split_at_mut(b);
            (&mut first[a..a + MAX_POINTS], &second[..MAX_POINTS])
        } else {
            let (first, second) = store.split_at_mut(a);
            (&mut second[..MAX_POINTS], &first[b..b + MAX_POINTS])
        }
    }

    fn rebuild_curves(&mut self, base: &PanelBase, f: &AnalysisFrame) {
        let s = &f.spectrum;
        let n = self.point_count;
        self.slot_valid = [false; Slot::ALL.len()];
        let stale = !self
            .table
            .as_ref()
            .is_some_and(|t| t.matches(&s.frequencies, self.axis, n));
        if stale {
            self.table = ResampleTable::new(&s.frequencies, self.axis, n);
        }
        if self.table.is_none() {
            self.curves_valid = true;
            return;
        }
        let (lo, hi) = self.shown_range();
        let smoothing = SmoothContext {
            axis: self.axis,
            range_db: hi - lo,
        };
        if self.options.show_mid {
            self.build(Slot::Mid, &s.mid, lo, hi);
            self.update_fill_reference(base, n);
        }
        if self.options.show_side {
            self.build(Slot::Side, &s.side, lo, hi);
            let start = Slot::Side as usize * MAX_POINTS;
            smooth_highs(
                &mut self.store[start..start + MAX_POINTS],
                n,
                None,
                &mut self.scratch,
                smoothing,
            );
        }
        if self.wants_long_term(base) {
            self.build(Slot::Average, &s.average, lo, hi);
        }
        if self.wants_left_right() {
            self.build(Slot::Left, &s.left, lo, hi);
            self.build(Slot::Right, &s.right, lo, hi);
            // Each channel finds its tones on its OWN curve, and with the other channel as the reference: a quiet
            // tone in one channel only is a few dB over the noise of that channel (too little to call tonal on one
            // curve), but it stands clear over the other channel at the same frequency.
            for (change, read) in [(Slot::Left, Slot::Right), (Slot::Right, Slot::Left)] {
                let (v, other) = Self::two_slots(&mut self.store, change, read);
                smooth_highs(v, n, Some(other), &mut self.scratch, smoothing);
            }
        }
        if self.wants_peak_hold(base) {
            self.build(Slot::PeakHold, &s.peak_hold, lo, hi);
            // Display-side clamp: the long-term average holds old partials longer than the peak hold decays. A reader
            // cannot accept "average over peak", so peak hold is drawn as max(peak hold, average) in every column.
            if self.valid(Slot::PeakHold) && self.valid(Slot::Average) {
                let (pk, av) = Self::two_slots(&mut self.store, Slot::PeakHold, Slot::Average);
                for i in 0..n {
                    if av[i] > pk[i] {
                        pk[i] = av[i];
                    }
                }
            }
        }
        if let (true, Some(hp)) = (self.options.show_headphone_overlay, &f.headphone) {
            if hp.has_target && hp.response_db.len() >= s.mid.len() && hp.target_db.len() >= s.mid.len() {
                // Against the target: the ear gain that every good headphone has (the 3 kHz rise) is in both curves
                // and drops out. What is left is what this headphone adds to or takes from the music.
                let mut source = std::mem::take(&mut self.at_ear_source);
                source.resize(s.mid.len(), 0.0);
                for i in 0..s.mid.len() {
                    source[i] = s.mid[i] + hp.response_db[i] - hp.target_db[i];
                }
                self.build(Slot::AtEar, &source, lo, hi);
                self.at_ear_source = source;
            } else {
                self.build(Slot::AtEar, &hp.predicted_at_ear_db, lo, hi);
            }
            // Display smoothing (about 1/6 octave), so the prediction reads as tonal balance and does not go
            // again over every partial of the mid curve.
            let r = (n / 110).max(1);
            let start = Slot::AtEar as usize * MAX_POINTS;
            let v = &mut self.store[start..start + MAX_POINTS];
            box_blur(v, &mut self.scratch, n, r);
            box_blur(&self.scratch, v, n, r);
            // The scale of the band: -14 dB at 75 % of the plot height, +14 dB at the top.
            let half = Self::HP_BAND_HALF_SPAN_DB;
            let full = 2.0 * half / Self::HP_BAND_FRACTION;
            if hp.has_target {
                self.build(Slot::Target, &hp.target_db, half - full, half);
            }
            self.build(Slot::Response, &hp.response_db, half - full, half);
        }
        if let Some(slice) = self.ghost_slice(base) {
            let stale = !self
                .ghost_table
                .as_ref()
                .is_some_and(|t| t.matches(&slice.frequencies, self.axis, n));
            if stale {
                self.ghost_table = ResampleTable::new(&slice.frequencies, self.axis, n);
            }
            if let Some(table) = self.ghost_table.take() {
                if slice.mid_db.len() >= table.source_count {
                    table.apply(&slice.mid_db, lo, hi, 0.0, self.slot_mut(Slot::Ghost));
                    self.slot_valid[Slot::Ghost as usize] = true;
                }
                self.ghost_table = Some(table);
            }
        }
        self.curves_valid = true;
    }

    /// The past spectrum of a timed cursor, while it is drawn: the panel is
    /// linked, the cursor has a time, the spectrogram published the column, and
    /// Mid (whose past it is) is shown.
    fn ghost_slice<'a>(&self, base: &'a PanelBase) -> Option<&'a CursorHistorySlice> {
        if !(base.cursor_linked && self.options.show_mid && base.cursor.is_some_and(|c| c.seconds_ago.is_some())) {
            return None;
        }
        base.cursor_slice.as_ref().filter(|s| s.frequencies.len() >= 2)
    }

    /// The reference line of the fill: a least-squares line through the mid
    /// curve over x, so it cannot dip or rise beside a peak. It moves with a
    /// 0.8 s time constant. The fill is fully lit at and over this line and
    /// fades to the floor under it.
    fn update_fill_reference(&mut self, base: &PanelBase, n: usize) {
        if n < 8 {
            return;
        }
        let v = self.slot(Slot::Mid);
        let (mut sy, mut sxy) = (0f32, 0f32);
        for i in 0..n {
            let x = i as f32 / (n - 1) as f32 - 0.5;
            sy += v[i];
            sxy += x * v[i];
        }
        // The sum of x^2 for x uniform in -0.5...0.5 is about n / 12.
        let mean = sy / n as f32;
        let slope = sxy / (n as f32 / 12.0);
        let want = (
            (mean - slope * 0.5).max(0.18).min(1.0),
            (mean + slope * 0.5).max(0.18).min(1.0),
        );
        if !self.fill_reference_snapped {
            self.fill_reference_snapped = true;
            self.fill_reference = want;
            return;
        }
        let k = (1.0 - (-base.frame_delta / 0.8).exp()) as f32;
        self.fill_reference.0 += (want.0 - self.fill_reference.0) * k;
        self.fill_reference.1 += (want.1 - self.fill_reference.1) * k;
    }

    /// True when the L / R curves go through the display smoothing at this plot size: the legend says so.
    fn left_right_smoothed(&self) -> bool {
        self.options.show_left_right && smooth::applies(self.axis, self.point_count)
    }

    /// Copies a cached curve into the GPU arena of this frame.
    fn upload(&self, base: &mut PanelBase, slot: Slot) -> Option<FloatSlice> {
        if !self.valid(slot) {
            return None;
        }
        base.arena.push_floats(&self.slot(slot)[..self.point_count])
    }

    // MARK: Annotations

    fn rebuild_annotations(&mut self, f: &AnalysisFrame) {
        self.annotations_valid = true;
        self.secondary_peak_cache.clear();
        // Tonal entries only: an empty note name means "broad, not tonal" (a hump gets no ring and no note). The
        // entry that is `frame.peak` has its own marker: it is found by its frequency, wherever it stands in the
        // list. As many as the width can carry (about one label per 70 pt), not a fixed count.
        let cap = ((self.plot.w / 70.0) as usize).max(1);
        let lo = self.lo();
        for pk in &f.top_peaks {
            if self.secondary_peak_cache.len() < cap
                && !pk.note_name.is_empty()
                && !is_same_peak(pk, &f.peak)
                && !fmt::is_floor(pk.level_db)
                && pk.frequency_hz > self.axis.min_hz
                && pk.frequency_hz < self.axis.max_hz
                && pk.level_db > lo
            {
                self.secondary_peak_cache.push(pk.clone());
            }
        }
        self.stress_span_cache.clear();
        let Some(hp) = f.headphone.as_ref().filter(|_| self.show_stress_bands) else {
            return;
        };
        for flag in &hp.stress_flags {
            let Some(range) = &flag.frequency_range_hz else {
                continue;
            };
            if !(*range.end() > self.axis.min_hz && *range.start() < self.axis.max_hz) {
                continue;
            }
            let x0 = self.x_for(range.start().max(self.axis.min_hz));
            let x1 = self.x_for(range.end().min(self.axis.max_hz));
            self.stress_span_cache.push((flag.clone(), x0, x1.max(x0 + 3.0)));
        }
    }

    fn ensure_annotations(&mut self, f: &AnalysisFrame) {
        if !self.annotations_valid {
            self.rebuild_annotations(f);
        }
    }

    fn severity_color(p: &Palette, s: StressSeverity) -> Color {
        match s {
            StressSeverity::Info => p.accent,
            StressSeverity::Watch => p.warn,
            StressSeverity::High => p.danger,
        }
    }

    /// The part of the panel with a frequency axis under a point.
    fn pointer_in_plot(&self, p: [f32; 2]) -> bool {
        self.plot.contains_point(p[0], p[1])
    }

    // MARK: GPU

    fn uniforms(&self, base: &PanelBase) -> CurveUniforms {
        CurveUniforms {
            rect: [self.plot.x, self.plot.y, self.plot.w, self.plot.h],
            // No vertical wall where the data ends: every curve fades over the last points of the plot.
            fade_right: if Self::compact(base) { 8.0 } else { 16.0 },
            ..Default::default()
        }
    }

    fn plot_scissor(&self, base: &PanelBase, pass: &mut wgpu::RenderPass<'_>) {
        base.set_scissor(pass, self.plot.x, self.plot.y, self.plot.w, self.plot.h);
    }

    fn draw_grid(&self, base: &mut PanelBase, pass: &mut wgpu::RenderPass<'_>) {
        let p = base.palette;
        let (px, py, pw, ph) = (self.plot.x, self.plot.y, self.plot.w, self.plot.h);
        // Frequency grid: minor log lines 5 % white, labeled lines 12 %.
        for f in LogAxis::MINOR {
            if f > self.axis.min_hz && f < self.axis.max_hz {
                base.vline(self.x_for(f), py, py + ph, p.grid_minor);
            }
        }
        for f in LogAxis::LABELED {
            if f > self.axis.min_hz && f < self.axis.max_hz {
                base.vline(self.x_for(f), py, py + ph, p.grid_major);
            }
        }
        // dB grid.
        let step = self.db_step(base);
        let lo = self.lo();
        let mut db = (self.music_top() / step).floor() * step;
        while db >= lo - 0.01 {
            let strong = db.abs() < 0.01;
            let yy = self.y_for(db);
            if yy >= py - 0.5 && yy <= py + ph + 0.5 {
                base.hline(px, px + pw, yy, if strong { p.grid_strong } else { p.grid_major });
            }
            db -= step;
        }
        // Tick marks of the SPL axis, at the right edge, in the color of the bars.
        for v in self.spl_ticks(base) {
            base.hline(
                px + pw - 7.0,
                px + pw,
                self.y_for(v - self.spl_offset),
                with_alpha(p.spl_band, 0.8),
            );
        }
        base.flush(pass);
    }

    /// The span of each stress flag: a wash in the severity color that is
    /// strongest at the top, a bright top border, faint sides.
    fn draw_stress_bands(&mut self, base: &mut PanelBase, pass: &mut wgpu::RenderPass<'_>, f: &AnalysisFrame) {
        self.ensure_annotations(f);
        if self.stress_span_cache.is_empty() {
            return;
        }
        let (py, ph) = (self.plot.y, self.plot.h);
        for (flag, x0, x1) in &self.stress_span_cache {
            let c = Self::severity_color(&base.palette, flag.severity);
            let w = x1 - x0;
            let strong = if flag.severity == StressSeverity::Info {
                0.10
            } else {
                0.16
            };
            base.gradient_rect([*x0, py, w, ph], with_alpha(c, strong), with_alpha(c, 0.025), 0.0);
            base.rect([*x0, py, w, 2.0], with_alpha(c, 0.9));
            base.vline(*x0, py, py + ph, with_alpha(c, 0.28));
            base.vline(x0 + w, py, py + ph, with_alpha(c, 0.28));
        }
        base.flush(pass);
    }

    /// Top of the SPL layer: under the headphone band when that is shown.
    fn spl_top_y(&self) -> f32 {
        if self.hp_shown {
            self.hp_band().max_y()
        } else {
            self.plot.min_y()
        }
    }

    /// Top of the "dB SPL" unit on the right-hand axis: under the headphone band and under its lowest tick label.
    fn spl_unit_y(&self) -> f32 {
        let top = self.spl_top_y() + 2.0;
        match self.hp_ticks().last() {
            Some(lowest) if self.hp_shown => top.max(self.hp_y(*lowest) + 9.0),
            _ => top,
        }
    }

    /// Ticks of the SPL axis: round values, 10 dB apart or 20 when the plot is low.
    fn spl_ticks(&self, base: &PanelBase) -> Vec<f32> {
        if !self.spl_shown {
            return Vec::new();
        }
        let compact = Self::compact(base);
        let (lo, hi) = self.shown_range();
        let per_db = self.plot.h / (hi - lo).max(1.0);
        let step = if per_db * 10.0 >= if compact { 20.0 } else { 26.0 } {
            10.0
        } else {
            20.0
        };
        let mut out = Vec::new();
        let mut v = ((lo + self.spl_offset) / step).ceil() * step;
        while v <= self.music_top() + self.spl_offset {
            let yy = self.y_for(v - self.spl_offset);
            // The unit stands at the top of the axis: the first tick keeps clear of it.
            let top = if compact {
                self.spl_top_y() + 8.0
            } else {
                self.spl_unit_y() + 18.0
            };
            if yy >= top && yy <= self.plot.max_y() - 6.0 {
                out.push(v);
            }
            v += step;
        }
        out
    }

    /// Third-octave levels at the eardrum: stepped bars behind the curves, read on the right-hand axis.
    ///
    /// The fine spectrum is NOT rescaled into SPL. Its calibration is for tones
    /// (a full-scale sine reads 0 dBFS in every bin it falls into), and a bin
    /// is a fraction of a band: the level of a bin says nothing about the sound
    /// pressure of broadband music. Band RMS is the quantity that converts to
    /// pressure, so only the bands stand on the SPL axis.
    fn draw_spl_bands(&self, base: &mut PanelBase, pass: &mut wgpu::RenderPass<'_>, levels: &[f32], centers: &[f32]) {
        let c = base.palette.spl_band;
        let n = levels.len().min(centers.len());
        let count = self.point_count;
        if n == 0 || count < 2 {
            return;
        }
        let Some(slice) = base.arena.allocate_floats(count) else {
            return;
        };
        // One staircase over the points of the plot: one fill and one line, so there is no seam between two bands.
        let edge = 2f32.powf(1.0 / 6.0);
        let (lo, hi) = self.shown_range();
        let range = (hi - lo).max(1.0);
        let mut b = 0;
        let values = base.arena.floats_mut(slice);
        for (j, value) in values.iter_mut().enumerate() {
            let hz = self.axis.frequency(j as f32 / (count - 1) as f32);
            // Nominal centers (31.5, 63, ...) are not exactly a third apart: a step changes half way between two centers.
            while b < n - 1 && hz * hz > centers[b] * centers[b + 1] {
                b += 1;
            }
            let inside = hz >= centers[0] / edge && hz <= centers[n - 1] * edge;
            *value = if inside {
                ((levels[b] - self.spl_offset - lo) / range).max(0.0).min(1.0)
            } else {
                0.0
            };
        }
        let top = self.spl_top_y();
        base.set_scissor(pass, self.plot.x, top, self.plot.w, self.plot.max_y() - top);
        let high_contrast = base.high_contrast();
        let mut u = CurveUniforms {
            rect: [self.plot.x, self.plot.y, self.plot.w, self.plot.h],
            color: c,
            fill_top: if high_contrast { 0.34 } else { 0.26 },
            fill_bottom: 0.05,
            ..Default::default()
        };
        base.draw_curve_fill(pass, slice, (1.0, 1.0), u, None);
        u.color = scaled_alpha(c, if high_contrast { 0.85 } else { 0.55 });
        u.half_width = 0.5;
        base.draw_curve_line(pass, slice, u, false, None);
    }

    /// The shared cursor: a hairline at its frequency, from the axis of this
    /// panel. The crosshair of the pointer (level line and dot) stays while the
    /// pointer is what moves the cursor here.
    fn draw_linked_cursor(&self, base: &mut PanelBase, pass: &mut wgpu::RenderPass<'_>, f: &AnalysisFrame) {
        let Some(c) = base.cursor else { return };
        if !(c.frequency_hz >= self.axis.min_hz && c.frequency_hz <= self.axis.max_hz) {
            return;
        }
        let (px, py, pw, ph) = (self.plot.x, self.plot.y, self.plot.w, self.plot.h);
        let cx = self.x_for(c.frequency_hz);
        if let (true, Some(hv)) = (self.shows_pointer_readout(base), base.hover) {
            if self.pointer_in_plot(hv) {
                let color = with_alpha(base.palette.text, 0.38);
                base.hline(px, px + pw, hv[1], color);
            }
        }
        base.draw_cursor_vertical(cx, py, py + ph);
        // Where the hairline meets Mid: the point the "Mid" number of the readout belongs to.
        if self.options.show_mid {
            if let Some(m) = cursor::level(&f.spectrum.mid, &f.spectrum.frequencies, c.frequency_hz) {
                if m > self.lo() {
                    let my = self.y_for(m.min(self.hi()));
                    base.circle([cx, my], 3.0, with_alpha(base.palette.plot, 0.9), 0.0, 0.0);
                    base.circle([cx, my], 3.0, [1.0, 1.0, 1.0, 0.95], 1.1, 0.0);
                }
            }
        }
        base.flush(pass);
    }

    // MARK: For tests

    /// A display curve in dB, one value per plot point, as drawn (after clamps and smoothing). `None` when it is not shown.
    pub fn curve_for_testing(&mut self, base: &PanelBase, name: &str) -> Option<Vec<f32>> {
        let f = base.frame.clone()?;
        if !self.curves_valid {
            self.rebuild_curves(base, &f);
        }
        let slot = Slot::ALL.into_iter().find(|s| s.name() == name)?;
        if !self.valid(slot) {
            return None;
        }
        let (lo, hi) = self.shown_range();
        Some(
            self.slot(slot)[..self.point_count]
                .iter()
                .map(|v| v * (hi - lo) + lo)
                .collect(),
        )
    }

    /// Runs the L / R display smoothing over a curve given in dB (one value per plot point). `other`: the opposite channel.
    pub fn smooth_highs_for_testing(&mut self, db: &[f32], other: Option<&[f32]>) -> Vec<f32> {
        let (lo, hi) = self.shown_range();
        let n = db.len().min(other.map_or(usize::MAX, |o| o.len())).min(MAX_POINTS - 1);
        let normalized = |values: &[f32]| -> Vec<f32> {
            let mut out = vec![0.0; MAX_POINTS];
            for i in 0..n {
                out[i] = (values[i] - lo) / (hi - lo);
            }
            out
        };
        let mut v = normalized(db);
        let other = other.map(normalized);
        smooth_highs(
            &mut v,
            n,
            other.as_deref(),
            &mut self.scratch,
            SmoothContext {
                axis: self.axis,
                range_db: hi - lo,
            },
        );
        v[..n].iter().map(|x| x * (hi - lo) + lo).collect()
    }

    pub fn point_count(&self) -> usize {
        self.point_count
    }

    pub fn frequency_of_point(&self, i: usize) -> f32 {
        self.axis.frequency(i as f32 / (self.point_count.max(2) - 1) as f32)
    }

    /// The band of the headphone curves in points (`None` with no overlay).
    pub fn headphone_band(&self) -> Option<Rect> {
        self.hp_shown.then(|| self.hp_band())
    }

    /// The plot rectangle in points.
    pub fn plot_rect(&self) -> Rect {
        self.plot
    }

    /// Labeled and minor grid frequencies inside the plot.
    pub fn grid_frequencies(&self) -> Vec<f32> {
        LogAxis::LABELED
            .into_iter()
            .chain(LogAxis::MINOR)
            .filter(|f| *f > self.axis.min_hz && *f < self.axis.max_hz)
            .collect()
    }

    /// x of the cursor hairline in points, `None` with no cursor inside the axis.
    pub fn cursor_x(&self, base: &PanelBase) -> Option<f32> {
        let c = base.cursor.filter(|_| base.cursor_linked)?;
        (c.frequency_hz >= self.axis.min_hz && c.frequency_hz <= self.axis.max_hz).then(|| self.x_for(c.frequency_hz))
    }

    pub fn peak_label_placements(&self) -> &[PeakLabelPlacement] {
        &self.peak_label_placements
    }

    pub fn legend_named(&self) -> &[Slot] {
        &self.legend_named
    }
}

fn is_same_peak(a: &PeakReading, b: &PeakReading) -> bool {
    b.frequency_hz > 0.0 && (a.frequency_hz - b.frequency_hz).abs() <= b.frequency_hz * 0.003
}

impl Panel for SpectrumPanel {
    fn kind(&self) -> PanelKind {
        PanelKind::Spectrum
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

    fn palette_changed(&mut self, base: &mut PanelBase) {
        let colors: Vec<[f32; 3]> = (0..256)
            .map(|i| Palette::spectrum_color(self.axis.frequency(i as f32 / 255.0)))
            .collect();
        self.hue_lut = Some(base.ctx.make_lut(&colors));
    }

    fn layout_changed(&mut self, base: &mut PanelBase) {
        self.layout(base);
    }

    fn static_signature(&mut self, base: &PanelBase) -> u64 {
        let mut h = Signature::new();
        h.add(base.static_signature())
            .add((self.lo() * 8.0) as i64)
            .add((self.hi() * 8.0) as i64)
            .add(self.hp_shown)
            .add(self.hp_hidden)
            .add(self.at_ear_shown);
        if self.spl_shown {
            h.add(self.spl_offset as i64);
        }
        h.finish()
    }

    fn dynamic_signature(&self, base: &PanelBase) -> u64 {
        let mut h = Signature::new();
        if let Some(f) = &base.frame {
            h.add((f.peak.frequency_hz * 10.0) as i64)
                .add((f.peak.level_db * 10.0) as i64)
                .add(f.peak.cents as i64)
                .add(f.headphone.as_ref().map(|hp| (&hp.model_name, hp.has_target)))
                .add(&f.peak.note_name);
            for pk in &f.top_peaks {
                h.add((pk.frequency_hz * 4.0) as i64)
                    .add((pk.level_db * 2.0) as i64)
                    .add(&pk.note_name);
            }
            h.add((f.lowest_strong_hz * 10.0) as i64);
            for flag in f.headphone.iter().flat_map(|hp| &hp.stress_flags) {
                h.add(&flag.id)
                    .add(&flag.plot_label)
                    .add(flag.severity as i32)
                    .add(flag.frequency_range_hz.as_ref().map(|r| r.start().to_bits()));
            }
        }
        let o = &self.options;
        h.add(self.show_stress_bands)
            .add(self.level_axis)
            .add(self.spl_shown)
            .add(self.auto_declutter)
            .add(self.ghost_slice(base).is_some())
            .add((
                o.show_left_right,
                o.show_mid,
                o.show_peak_hold,
                o.show_average,
                o.show_headphone_overlay,
                o.show_side,
            ));
        h.finish()
    }

    fn update(&mut self, base: &mut PanelBase, f: &AnalysisFrame, dt: f64) {
        self.curves_valid = false;
        self.annotations_valid = false;
        if self.resolve_bands(base) {
            self.layout(base);
            base.needs_display = true;
        }
        if self.level_axis == LevelAxis::DbSpl || self.spl_shown {
            self.update_spl_axis(base, f);
        }
        self.update_auto_range(f, dt);
    }

    fn cursor_slice_changed(&mut self, base: &mut PanelBase) {
        self.curves_valid = false;
        base.needs_display = true;
    }

    fn cursor_at(&self, _base: &PanelBase, point: [f32; 2]) -> Option<PanelCursor> {
        if !(self.pointer_in_plot(point) && self.plot.w > 1.0) {
            return None;
        }
        Some(PanelCursor::new(
            self.axis.frequency((point[0] - self.plot.min_x()) / self.plot.w),
            PanelKind::Spectrum,
        ))
    }

    fn is_on_cursor(&self, base: &PanelBase, point: [f32; 2]) -> bool {
        match base.cursor {
            Some(c) if self.pointer_in_plot(point) => (self.x_for(c.frequency_hz) - point[0]).abs() <= 5.0,
            _ => false,
        }
    }

    fn draw(&mut self, base: &mut PanelBase, pass: &mut wgpu::RenderPass<'_>) {
        let p = base.palette;
        let (px, py, pw, ph) = (self.plot.x, self.plot.y, self.plot.w, self.plot.h);

        // Plot field: deeper than the panel, a touch lighter toward the top for depth.
        base.gradient_rect([px, py, pw, ph], mix(p.plot, p.panel, 0.55), p.plot, 3.0);
        base.flush(pass);

        let Some(f) = base.frame.clone() else {
            self.draw_grid(base, pass);
            return;
        };
        if !self.curves_valid {
            self.rebuild_curves(base, &f);
        }
        if let (true, Some(s), Some(t)) = (self.spl_shown, &f.spl, &f.third_octave) {
            self.plot_scissor(base, pass);
            self.draw_spl_bands(base, pass, &s.band_levels_eardrum, &t.centers_hz);
            base.clear_scissor(pass);
        }

        let compact = Self::compact(base);
        let hue_lut = self.hue_lut.take();
        let mid = self.upload(base, Slot::Mid);
        if let Some(mid) = mid {
            self.plot_scissor(base, pass);
            // Fill: one polygon, one gradient. 55 % at the reference line, 0 % at the floor, so the grid reads through it.
            let u = CurveUniforms {
                mode: 1.0,
                color: WHITE,
                fill_top: 0.55,
                fill_bottom: 0.0,
                ..self.uniforms(base)
            };
            base.draw_curve_fill(pass, mid, self.fill_reference, u, hue_lut.as_ref());
            base.clear_scissor(pass);
        }
        self.draw_grid(base, pass);
        self.plot_scissor(base, pass);
        self.draw_stress_bands(base, pass, &f);

        if let Some(mid) = mid {
            // One soft glow under the line: 3 pt sigma, 30 % at most. The data edge stays the line, not the glow.
            let glow = CurveUniforms {
                mode: 1.0,
                color: [1.0, 1.0, 1.0, if compact { 0.22 } else { 0.30 }],
                half_width: 0.4,
                soft: if compact { 1.6 } else { 3.0 },
                ..self.uniforms(base)
            };
            base.draw_curve_line(pass, mid, glow, true, hue_lut.as_ref());
        }

        let proto = self.uniforms(base);
        let plain = move |color: Color, half_width: f32| CurveUniforms {
            color,
            half_width,
            ..proto
        };
        if self.wants_long_term(base) && self.is_named(Slot::Average) {
            if let Some(c) = self.upload(base, Slot::Average) {
                // Solid, thin, cool grey at 60 %: it cannot break up on steep slopes and cannot be taken for peak hold (warm white).
                let u = plain(p.average, 0.5);
                base.draw_curve_line(pass, c, u, false, None);
            }
        }
        // L and R: 1 px, no glow, desaturated tints at 75 %, everywhere. Mid is the only curve with the hue of the frequency.
        if self.is_named(Slot::Left) {
            for (slot, color) in [(Slot::Right, p.spectrum_right), (Slot::Left, p.spectrum_left)] {
                if let Some(c) = self.upload(base, slot) {
                    let u = plain(color, 0.5);
                    base.draw_curve_line(pass, c, u, false, None);
                }
            }
        }
        if self.is_named(Slot::Side) {
            if let Some(c) = self.upload(base, Slot::Side) {
                let u = plain(p.side, 0.45);
                base.draw_curve_line(pass, c, u, false, None);
            }
        }
        if self.is_named(Slot::PeakHold) {
            if let Some(c) = self.upload(base, Slot::PeakHold) {
                let color = if self.valid(Slot::Ghost) {
                    scaled_alpha(p.peak_hold, 0.35)
                } else {
                    p.peak_hold
                };
                let u = plain(color, 0.5);
                base.draw_curve_line(pass, c, u, false, None);
            }
        }
        if let Some(c) = self.upload(base, Slot::Ghost) {
            // The spectrum at the time of the cursor: dashed (6 on, 4 off) in the cyan of the spectrogram it comes
            // from, no fill, no glow, under the live Mid outline. No other trace of the panel is dashed cyan.
            let u = CurveUniforms {
                dash_period: Self::GHOST_DASH.0,
                dash_duty: Self::GHOST_DASH.1,
                ..plain(p.cursor_ghost, 0.6)
            };
            base.draw_curve_line(pass, c, u, false, None);
        }
        if let Some(mid) = mid {
            // The data: a 1.25 pt opaque line.
            let u = CurveUniforms {
                mode: 1.0,
                whiten: 0.38,
                ..plain(WHITE, 0.625)
            };
            base.draw_curve_line(pass, mid, u, false, hue_lut.as_ref());
        }
        self.hue_lut = hue_lut;

        if let (true, Some(hp)) = (self.hp_shown, &f.headphone) {
            if self.at_ear_shown {
                if let Some(c) = self.upload(base, Slot::AtEar) {
                    // As thin as Mid and lighter (1.25 pt, 70 %): a prediction, it must not compete with the measurement.
                    let u = plain(with_alpha(p.hp_at_ear, 0.70), 0.625);
                    base.draw_curve_line(pass, c, u, false, None);
                }
            }
            // The band of the headphone curves: a hairline under it, the 0 line and the tick marks, in the response color.
            let a = Self::HP_BAND_ALPHA;
            let band_bottom = self.hp_band().max_y();
            base.hline(px, px + pw, band_bottom, with_alpha(p.hp_response, 0.16));
            base.hline(px, px + pw, self.hp_y(0.0), with_alpha(p.hp_response, 0.30 * a));
            for d in self.hp_ticks() {
                if d != 0.0 {
                    base.hline(px + pw - 7.0, px + pw, self.hp_y(d), with_alpha(p.hp_response, 0.75));
                }
            }
            base.flush(pass);
            // Nothing of the curves of the band leaves the band (a response under -14 dB ends at the hairline).
            base.set_scissor(pass, px, py, pw, band_bottom - py);
            let response = self.upload(base, Slot::Response);
            if let (true, Some(response)) = (hp.has_target, response) {
                if let Some(target) = self.upload(base, Slot::Target) {
                    // Error area: where the headphone departs from the target.
                    let u = plain(with_alpha(p.hp_response, 0.17 * a), 0.5);
                    base.draw_curve_band(pass, response, target, u);
                    // Dotted target.
                    let t = CurveUniforms {
                        dash_period: 4.5,
                        dash_duty: 0.36,
                        ..plain(with_alpha(p.hp_target, p.hp_target[3] * a), 0.9)
                    };
                    base.draw_curve_line(pass, target, t, false, None);
                }
            }
            if let Some(response) = response {
                let u = plain(with_alpha(p.hp_response, a), 0.8);
                base.draw_curve_line(pass, response, u, false, None);
            }
            self.plot_scissor(base, pass);
        }

        // Peak marker.
        let (lo, hi) = self.shown_range();
        if !fmt::is_floor(f.peak.level_db)
            && f.peak.frequency_hz > self.axis.min_hz
            && f.peak.frequency_hz < self.axis.max_hz
            && f.peak.level_db > lo
        {
            let at = [self.x_for(f.peak.frequency_hz), self.y_for(f.peak.level_db.min(hi))];
            let hue = rgba(Palette::spectrum_color(f.peak.frequency_hz), 1.0);
            base.circle(at, 6.0, with_alpha(hue, 0.30), 0.0, 2.5);
            base.flush_add(pass);
            base.circle(at, 3.2, [1.0, 1.0, 1.0, 0.95], 1.1, 0.0);
            base.flush(pass);
        }

        // Peaks 2 to 5: small rings on the curve. Their note names are in the text layer.
        self.ensure_annotations(&f);
        for pk in &self.secondary_peak_cache {
            let at = [self.x_for(pk.frequency_hz), self.y_for(pk.level_db.min(hi))];
            base.circle(at, 2.6, with_alpha(p.plot, 0.85), 0.0, 0.0);
            let ring = mix(rgba(Palette::spectrum_color(pk.frequency_hz), 1.0), WHITE, 0.45);
            base.circle(at, 2.6, ring, 1.0, 0.0);
        }
        base.flush(pass);

        if base.cursor_linked {
            self.draw_linked_cursor(base, pass, &f);
        } else if let Some(hv) = base.hover.filter(|hv| self.pointer_in_plot(*hv)) {
            // Hover crosshair.
            let c = with_alpha(p.text, 0.38);
            base.vline(hv[0], py, py + ph, c);
            base.hline(px, px + pw, hv[1], c);
            base.circle(hv, 2.5, p.text, 0.0, 0.0);
            base.flush(pass);
        }
        base.clear_scissor(pass);
        // Lowest strong content: a small tick under the x axis at that frequency, in the neutral text color.
        if f.lowest_strong_hz >= self.axis.min_hz && f.lowest_strong_hz <= self.axis.max_hz {
            let tx = self.x_for(f.lowest_strong_hz);
            base.rounded_rect(
                [tx - 0.75, self.axis_bottom() + 1.0, 1.5, 5.0],
                with_alpha(p.text, 0.9),
                0.5,
            );
            base.flush(pass);
        }
    }

    fn draw_static(&mut self, base: &PanelBase, o: &mut crate::text::OverlayContext) {
        self.draw_static_labels(base, o);
    }

    fn draw_dynamic(&mut self, base: &mut PanelBase, o: &mut crate::text::OverlayContext) {
        self.draw_dynamic_labels(base, o);
    }

    fn hover_label(&mut self, base: &PanelBase) -> Option<(Vec<String>, [f32; 2])> {
        self.hover_lines(base)
    }

    fn cursor_items(&mut self, base: &PanelBase) -> Vec<crate::cursor::CursorReadoutItem> {
        self.readout_items(base)
    }

    fn accessibility_label(&self) -> String {
        "Spectrum".to_string()
    }

    fn accessibility_value(&self, base: &PanelBase) -> String {
        self.accessibility_text(base)
    }
}
