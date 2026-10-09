//! The meters panel: loudness (M / S / I), true peak + RMS per channel, numeric
//! readouts, a 60 s loudness history, band energy. With `frame.spl`: the "Level
//! at the ear" block (see `ear.rs`). Without it the layout is unchanged.
//!
//! Trust rule: a bar and the number next to it come from one value. There is no
//! display smoothing here. The loudness values are windowed measurements
//! already, and the true peak values carry the meter ballistics of the contract.
//!
//! Not ported yet: the A/B comparison table (`MetersRenderer+Compare.swift`).
//! It comes with the session features.
//!
//! Port source: `Sources/JoseonRender/MetersRenderer.swift`.

pub mod ear;

use crate::arena::FloatSlice;
use crate::contracts::{DoseStandard, PanelCursor, PanelKind};
use crate::cursor::{self, CursorReadoutItem, Tone};
use crate::fmt::{self, band_labels};
use crate::gpu::{Color, CurveUniforms};
use crate::palette::{mix, rgba, scaled_alpha, with_alpha, Palette, WHITE};
use crate::panel::{Panel, PanelBase, Signature};
use crate::text::fonts::{self, Font, Weight};
use crate::text::{HAlign, OverlayContext, Rect, VAlign};
use ear::{EarDoseLedger, EarPlan, EarShown, APPROX, EAR_COLUMN_WIDTH, EAR_STRIP_HEIGHT};
use joseon_core::contracts::{AnalysisFrame, BandEnergy};
use std::any::Any;

/// (value, color) pairs of a bar, low to high.
pub type Stops = Vec<(f32, Color)>;

// Scales. The loudness bars and the peak bars share one scale, so their grid lines run through both groups.
const LEVEL_MIN: f32 = -60.0;
const LEVEL_MAX: f32 = 3.0;
const BAND_MIN: f32 = -78.0;
const BAND_MAX: f32 = -6.0;

// Loudness history: M and S every 100 ms, 60 s.
const HISTORY_SLOTS: usize = 600;
const HISTORY_STEP: f64 = 0.1;
/// Marks a history slot without a measurement.
pub const NO_VALUE: f32 = -1000.0;
const MOMENTARY_WINDOW: f64 = 0.4;
const SHORT_TERM_WINDOW: f64 = 3.0;
/// LRA needs a stretch of program: EBU Tech 3342 works on 3 s blocks, and a number from a few of them is noise.
pub const LOUDNESS_RANGE_MINIMUM_SECONDS: f64 = 30.0;
/// The numbers of the ear block are taken this often.
pub const EAR_TEXT_INTERVAL: f64 = 0.25;

/// Where the "Level at the ear" block stands. `None` while the frames carry no SPL reading.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EarMode {
    None,
    Column,
    Strip,
    Row,
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct Cap {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub color: Color,
}

pub struct MetersPanel {
    /// Loudness target in LUFS (for example -14, -16, -23). Draws a tick on the I bar and the delta in LU.
    target_lufs: Option<f32>,
    /// Dose rule of the "Level at the ear" block.
    dose_standard: DoseStandard,
    /// The dose the app keeps (persisted, new every day / ISO week). `None` = the running dose of the reading.
    dose_ledger: Option<EarDoseLedger>,

    // Layout (points).
    loud: Rect,
    read: Rect,
    peak: Rect,
    band: Rect,
    loud_bars: Rect,
    peak_bars: Rect,
    band_bars: Rect,
    spark: Rect,
    compact: bool,

    ear_mode: EarMode,
    /// Column (wide), strip (card with band energy) or zero (the row mode lives inside the readout column).
    ear: Rect,
    /// The level bar of the block: vertical in the column, zero when there is none.
    ear_bar: Rect,
    ear_plan: EarPlan,
    has_spl: bool,
    /// What the block's text shows: whole numbers, taken from the reading at most 4 times per second.
    ear_shown: Option<EarShown>,
    ear_latch_time: f64,
    strip_bar_cache: Rect,
    strip_bar_cache_key: Option<Rect>,
    compact_spark_width: f32,
    show_bands: bool,
    big_font_size: f32,
    big_row_height: f32,
    small_row_height: f32,
    small_top: f32,

    history_m: Vec<f32>,
    history_s: Vec<f32>,
    history_count: usize,
    history_head: usize,
    history_bank: f64,
    slot_max_m: f32,
    last_measured_seconds: f64,

    // Bar colors, rebuilt with the palette (not per frame).
    level_stops: Stops,
    rms_stops: Stops,
    band_stops: Vec<Stops>,
    cap_color: Color,
    pub(crate) caps: Vec<Cap>,
}

impl Default for MetersPanel {
    fn default() -> Self {
        Self::new()
    }
}

pub(crate) fn y_for(v: f32, lo: f32, hi: f32, r: Rect) -> f32 {
    let t = ((v - lo) / (hi - lo)).max(0.0).min(1.0);
    r.max_y() - t * r.h
}

fn color_at(stops: &[(f32, Color)], s: f32) -> Color {
    if s <= stops[0].0 {
        return stops[0].1;
    }
    for i in 1..stops.len() {
        if s <= stops[i].0 {
            let t = (s - stops[i - 1].0) / (stops[i].0 - stops[i - 1].0).max(1e-6);
            return mix(stops[i - 1].1, stops[i].1, t);
        }
    }
    stops[stops.len() - 1].1
}

/// A vertical bar whose color depends on height.
#[allow(clippy::too_many_arguments)]
pub(crate) fn zoned_bar(
    base: &mut PanelBase,
    x: f32,
    width: f32,
    r: Rect,
    value: f32,
    lo: f32,
    hi: f32,
    stops: &[(f32, Color)],
) {
    if !(value > lo && stops.len() >= 2) {
        return;
    }
    let v = value.min(hi);
    let mut from = lo;
    let mut segment = |base: &mut PanelBase, e: f32| {
        let (y_top, y_bot) = (y_for(e, lo, hi, r), y_for(from, lo, hi, r));
        if y_bot - y_top > 0.01 {
            base.gradient_rect(
                [x, y_top, width, y_bot - y_top],
                color_at(stops, e),
                color_at(stops, from),
                0.0,
            );
        }
        from = e;
    };
    for s in stops {
        if s.0 > lo && s.0 < v {
            segment(base, s.0);
        }
    }
    segment(base, v);
}

pub(crate) fn track(base: &mut PanelBase, x: f32, width: f32, r: Rect) {
    let c = base.palette.track;
    base.gradient_rect([x, r.min_y(), width, r.h], c, scaled_alpha(c, 0.75), 2.0);
}

fn grid_lines(base: &mut PanelBase, x0: f32, x1: f32, r: Rect, lo: f32, hi: f32, step: f32) {
    let mut v = (hi / step).floor() * step;
    while v >= lo - 0.01 {
        let major = (v / 12.0).round() * 12.0 == v;
        let color = if major {
            base.palette.grid_major
        } else {
            base.palette.grid_minor
        };
        base.hline(x0, x1, y_for(v, lo, hi, r), color);
        v -= step;
    }
}

/// One color rule for the distance to the loudness target, at every size of the panel: over the target (more than
/// +1 LU) = amber, within ±1 LU = the neutral text color, under = the calm green-cyan of the target.
pub fn target_delta_color(delta_lu: f32, p: &Palette) -> Color {
    // The rule reads the number as it is printed (one decimal): "+1.0 LU" is never amber.
    let d = (delta_lu * 10.0).round() / 10.0;
    if d > 1.0 {
        p.warn
    } else if d >= -1.0 {
        p.text
    } else {
        p.good
    }
}

struct Item {
    label: String,
    value: String,
    unit: &'static str,
    color: Color,
}

impl MetersPanel {
    pub fn new() -> Self {
        Self {
            target_lufs: None,
            dose_standard: DoseStandard::NioshDaily,
            dose_ledger: None,
            loud: Rect::default(),
            read: Rect::default(),
            peak: Rect::default(),
            band: Rect::default(),
            loud_bars: Rect::default(),
            peak_bars: Rect::default(),
            band_bars: Rect::default(),
            spark: Rect::default(),
            compact: false,
            ear_mode: EarMode::None,
            ear: Rect::default(),
            ear_bar: Rect::default(),
            ear_plan: EarPlan::default(),
            has_spl: false,
            ear_shown: None,
            ear_latch_time: 0.0,
            strip_bar_cache: Rect::default(),
            strip_bar_cache_key: None,
            compact_spark_width: 0.0,
            show_bands: true,
            big_font_size: 30.0,
            big_row_height: 80.0,
            small_row_height: 50.0,
            small_top: 0.0,
            history_m: vec![NO_VALUE; HISTORY_SLOTS],
            history_s: vec![NO_VALUE; HISTORY_SLOTS],
            history_count: 0,
            history_head: 0,
            history_bank: 0.0,
            slot_max_m: NO_VALUE,
            last_measured_seconds: 0.0,
            level_stops: Vec::new(),
            rms_stops: Vec::new(),
            band_stops: Vec::new(),
            cap_color: WHITE,
            caps: Vec::with_capacity(24),
        }
    }

    pub fn set_target_lufs(&mut self, base: &mut PanelBase, target: Option<f32>) {
        if target != self.target_lufs {
            self.target_lufs = target;
            base.needs_display = true;
        }
    }

    pub fn set_dose_standard(&mut self, base: &mut PanelBase, standard: DoseStandard) {
        if standard != self.dose_standard {
            self.dose_standard = standard;
            base.needs_display = true;
        }
    }

    /// The app's dose ledger. While set, the percent, the ring and "time left at this level" of the block come from it.
    pub fn set_dose_ledger(&mut self, base: &mut PanelBase, ledger: Option<EarDoseLedger>) {
        if ledger == self.dose_ledger {
            return;
        }
        self.dose_ledger = ledger;
        // The text is taken again only when a shown percent changes: the levels keep their 4 Hz latch.
        let (Some(shown), Some(f)) = (&self.ear_shown, base.frame.clone()) else {
            return;
        };
        let Some(s) = &f.spl else { return };
        let e = EarShown::new(s, f.is_silent, ledger);
        if e.dose_niosh != shown.dose_niosh || e.dose_who != shown.dose_who {
            self.ear_shown = Some(e);
            base.needs_display = true;
        }
    }

    pub fn is_compact(&self) -> bool {
        self.compact
    }

    pub fn ear_mode(&self) -> EarMode {
        self.ear_mode
    }

    pub fn ear_shown(&self) -> Option<&EarShown> {
        self.ear_shown.as_ref()
    }

    pub fn ear_plan(&self) -> &EarPlan {
        &self.ear_plan
    }

    pub fn big_font_size(&self) -> f32 {
        self.big_font_size
    }

    pub fn cap_color(&self) -> Color {
        self.cap_color
    }

    /// The rectangles of the layout: loudness, readouts, peak, band energy, band bars, history, ear block, ear bar.
    pub fn layout_rects(&self) -> [Rect; 8] {
        [
            self.loud,
            self.read,
            self.peak,
            self.band,
            self.band_bars,
            self.spark,
            self.ear,
            self.ear_bar,
        ]
    }

    pub(crate) fn cap_font(&self) -> Font {
        fonts::ui(if self.compact { 10.0 } else { 11.0 }, Weight::Semibold)
    }

    /// dB window of the history plot: 30 LU under a top that follows the loudest value, on a 6 LU grid.
    pub fn spark_range(&self) -> (f32, f32) {
        let mut m = -120.0f32;
        for i in 0..HISTORY_SLOTS {
            m = m.max(self.history_m[i].max(self.history_s[i]));
        }
        if let Some(t) = self.target_lufs {
            m = m.max(t);
        }
        let top = if m < -100.0 {
            -6.0
        } else {
            (((m + 1.0) / 6.0).ceil() * 6.0).min(0.0)
        };
        (top, top - 30.0)
    }

    /// History values, oldest first, `NO_VALUE` where there was no measurement yet: momentary, short term.
    pub fn history(&self) -> (Vec<f32>, Vec<f32>) {
        let start = (self.history_head + HISTORY_SLOTS - self.history_count) % HISTORY_SLOTS;
        let pick = |h: &[f32]| {
            (0..self.history_count)
                .map(|i| h[(start + i) % HISTORY_SLOTS])
                .collect()
        };
        (pick(&self.history_m), pick(&self.history_s))
    }

    // MARK: Geometry helpers

    fn loud_gap(&self) -> f32 {
        if self.compact {
            6.0
        } else {
            8.0
        }
    }

    /// x and width of a loudness bar.
    fn loud_bar(&self, i: usize) -> (f32, f32) {
        let bw = (self.loud_bars.w - self.loud_gap() * 2.0) / 3.0;
        (self.loud_bars.min_x() + i as f32 * (bw + self.loud_gap()), bw)
    }

    /// True peak bar and RMS bar of one channel: x, true peak width, RMS x, RMS width, channel width.
    fn channel_bars(&self, ch: usize) -> (f32, f32, f32, f32, f32) {
        let gap = if self.compact { 8.0 } else { 12.0 };
        let ch_w = (self.peak_bars.w - gap) / 2.0;
        let rms_w = (ch_w * 0.34).max(6.0);
        let tp_w = ch_w - rms_w - 3.0;
        let x = self.peak_bars.min_x() + ch as f32 * (ch_w + gap);
        (x, tp_w, x + tp_w + 3.0, rms_w, ch_w)
    }

    /// Center of the clip lamp (points, top-left origin).
    pub fn clip_lamp_center(&self) -> [f32; 2] {
        [
            self.peak.max_x() - 5.0,
            self.peak.min_y() + if self.compact { 9.0 } else { 27.0 },
        ]
    }

    /// Click target of the clip indicator.
    pub fn clip_indicator_rect(&self) -> Rect {
        Rect::new(self.peak.max_x() - 74.0, self.clip_lamp_center()[1] - 10.0, 74.0, 20.0)
    }

    /// The band-energy bar under the cursor, while the bars are shown.
    fn cursor_band(&self, base: &PanelBase) -> Option<usize> {
        if !(base.cursor_linked && self.show_bands) {
            return None;
        }
        cursor::band(base.cursor?.frequency_hz)
    }

    fn rms_text_color(base: &PanelBase) -> Color {
        mix(base.palette.rms, WHITE, 0.55)
    }

    // MARK: Loudness history

    fn draw_spark_frame(&self, base: &mut PanelBase) {
        let p = base.palette;
        let r = self.spark;
        base.rounded_rect([r.x, r.y, r.w, r.h], p.plot, 3.0);
        let (top, bottom) = self.spark_range();
        let mut v = top;
        while v >= bottom - 0.01 {
            let color = if v == top || v == bottom {
                p.grid_major
            } else {
                p.grid_minor
            };
            base.hline(r.min_x(), r.max_x(), y_for(v, bottom, top, r), color);
            v -= 6.0;
        }
        for s in [15.0, 30.0, 45.0] {
            base.vline(r.max_x() - r.w * s / 60.0, r.min_y(), r.max_y(), p.grid_minor);
        }
        if let Some(t) = self.target_lufs.filter(|t| *t > bottom && *t < top) {
            let y = y_for(t, bottom, top, r);
            let mut x = r.min_x();
            while x < r.max_x() - 1.0 {
                base.hline(x, (x + 5.0).min(r.max_x()), y, with_alpha(p.good, 0.9));
                x += 9.0;
            }
        }
    }

    /// Runs of slots that hold a measurement. Newest slot at the right edge; a short history fills only its share of
    /// the 60 s.
    fn runs(&self, base: &mut PanelBase, h: &[f32]) -> Vec<(FloatSlice, [f32; 4])> {
        let n = self.history_count;
        let (top, bottom) = self.spark_range();
        let start = (self.history_head + HISTORY_SLOTS - n) % HISTORY_SLOTS;
        let r = self.spark;
        let dx = r.w / (HISTORY_SLOTS - 1) as f32;
        let at = |i: usize| h[(start + i) % HISTORY_SLOTS];
        let mut out = Vec::new();
        let mut i = 0;
        while i < n {
            while i < n && at(i) <= NO_VALUE + 1.0 {
                i += 1;
            }
            let first = i;
            while i < n && at(i) > NO_VALUE + 1.0 {
                i += 1;
            }
            let count = i - first;
            if count < 2 {
                continue;
            }
            let Some(slice) = base.arena.allocate_floats(count) else {
                continue;
            };
            for (k, value) in base.arena.floats_mut(slice).iter_mut().enumerate() {
                *value = ((at(first + k) - bottom) / (top - bottom)).max(0.0).min(1.0);
            }
            let x0 = r.max_x() - (n - 1 - first) as f32 * dx;
            out.push((slice, [x0, r.min_y() + 1.0, (count - 1) as f32 * dx, r.h - 2.0]));
        }
        out
    }

    fn draw_spark_curves(&self, base: &mut PanelBase, pass: &mut wgpu::RenderPass<'_>) {
        if self.history_count < 2 {
            return;
        }
        let r = self.spark;
        base.set_scissor(pass, r.x, r.y, r.w, r.h);
        let accent = base.palette.accent;
        let mut u = CurveUniforms::default();
        for (values, rect) in self.runs(base, &self.history_s) {
            u.rect = rect;
            u.color = with_alpha(accent, 0.22);
            u.fill_top = 1.0;
            u.fill_bottom = 0.15;
            base.draw_curve_fill(pass, values, (1.0, 1.0), u, None);
        }
        for (values, rect) in self.runs(base, &self.history_m) {
            u.rect = rect;
            u.color = with_alpha(accent, 0.75);
            u.half_width = 0.5;
            base.draw_curve_line(pass, values, u, false, None);
        }
        for (values, rect) in self.runs(base, &self.history_s) {
            u.rect = rect;
            u.color = mix(accent, WHITE, 0.65);
            u.half_width = 0.9;
            base.draw_curve_line(pass, values, u, false, None);
        }
        base.clear_scissor(pass);
    }

    // MARK: Text

    /// The caption of the highlighted band, on the row of the "BAND ENERGY" caption, right-aligned: the longest form
    /// that stands clear of what is already there.
    fn draw_cursor_caption(&self, base: &PanelBase, o: &mut OverlayContext) {
        let (Some(b), Some(f)) = (self.cursor_band(base), base.frame.as_ref()) else {
            return;
        };
        let font = fonts::ui(if self.compact { 10.0 } else { 11.0 }, Weight::Medium);
        let range = cursor::band_range(b);
        let level = format!("{} dB", fmt::db(f.bands.values()[b], 1, false));
        let name = BandEnergy::NAMES[b];
        let forms = [
            format!("{name} {range}   {level}"),
            format!("{name} {range}"),
            format!("{} {range}", band_labels::SHORT[b]),
            range.clone(),
        ];
        let (x, y) = (self.band_bars.max_x(), self.band.min_y() + 4.0);
        for text in forms {
            let bounds = o.text_bounds(&text, x, y, &font, HAlign::Right, VAlign::Top, 0.0);
            if !(bounds.min_x() >= self.band.min_x() && o.is_free(bounds, 3.0, &[])) {
                continue;
            }
            let color = mix(base.palette.accent, WHITE, 0.55);
            o.text(&text, x, y, &font, color, HAlign::Right, VAlign::Top);
            return;
        }
    }
}

impl Panel for MetersPanel {
    fn kind(&self) -> PanelKind {
        PanelKind::Meters
    }

    fn as_any(&self) -> &dyn Any {
        self
    }

    fn as_any_mut(&mut self) -> &mut dyn Any {
        self
    }

    fn palette_changed(&mut self, base: &mut PanelBase) {
        let p = base.palette;
        let deep = with_alpha(mix(p.accent, p.background, 0.55), 1.0);
        let bright = mix(p.accent, [0.35, 0.95, 1.0, 1.0], 0.6);
        let lime = [0.72, 0.95, 0.42, 1.0];
        self.cap_color = bright;
        self.level_stops = vec![
            (LEVEL_MIN, deep),
            (-30.0, p.accent),
            (-12.0, bright),
            (-6.0, lime),
            (-2.0, p.warn),
            (0.0, p.danger),
            (LEVEL_MAX, p.danger),
        ];
        // RMS is a quiet slate: it must not compete with the true peak bar next to it.
        self.rms_stops = vec![
            (LEVEL_MIN, with_alpha(mix(p.rms, p.background, 0.45), 1.0)),
            (-12.0, p.rms),
            (LEVEL_MAX, mix(p.rms, WHITE, 0.35)),
        ];
        self.band_stops = (0..8)
            .map(|i| {
                let hz = (BandEnergy::EDGES_HZ[i] * BandEnergy::EDGES_HZ[i + 1]).sqrt();
                let hue = rgba(Palette::spectrum_color(hz), 1.0);
                vec![
                    (BAND_MIN, with_alpha(mix(hue, p.background, 0.72), 1.0)),
                    (BAND_MAX, hue),
                ]
            })
            .collect();
    }

    fn layout_changed(&mut self, base: &mut PanelBase) {
        let (w, h) = (base.width, base.height);
        let pad = 14.0;
        let has_spl = self.has_spl;
        // The ear column needs 214 pt beside the readouts: with an SPL reading the wide layout starts at 820 pt.
        self.compact = w < if has_spl { 820.0 } else { 760.0 };
        let compact = self.compact;
        self.ear = Rect::default();
        self.ear_bar = Rect::default();
        let header = if compact { 30.0 } else { 66.0 };
        let footer = 22.0;
        let scale_w = if compact { 26.0 } else { 32.0 };
        if !compact {
            let (a_w, c_w) = (150.0, 150.0);
            // With the ear column the readouts share what is left of the width: band energy keeps room for its names.
            let ear_room = if has_spl { EAR_COLUMN_WIDTH + pad } else { 0.0 };
            let b_w = ((w - a_w - c_w - pad * 5.0 - ear_room) * 0.40).max(250.0).min(340.0);
            self.loud = Rect::new(pad, pad, a_w, h - pad * 2.0);
            // The peak bars stand next to the loudness bars: one scale, one set of grid lines.
            self.peak = Rect::new(self.loud.max_x() + 10.0, pad, c_w - scale_w + 14.0, h - pad * 2.0);
            self.read = Rect::new(self.peak.max_x() + pad + 8.0, pad, b_w, h - pad * 2.0);
            let band_x = self.read.max_x() + pad;
            self.band = Rect::new(
                band_x,
                pad,
                (w - self.read.max_x() - pad * 2.0).max(80.0),
                h - pad * 2.0,
            );
            self.ear_mode = if has_spl { EarMode::Column } else { EarMode::None };
            if has_spl {
                // The estimate stands apart from the measurements: the last column, behind its own divider.
                let ear_w = self.band.w.min(EAR_COLUMN_WIDTH);
                if self.band.w - ear_w - pad >= 220.0 {
                    self.ear = Rect::new(self.band.max_x() - ear_w, pad, ear_w, h - pad * 2.0);
                    self.band.w -= ear_w + pad;
                } else {
                    self.ear = Rect::new(self.band.min_x(), pad, ear_w, h - pad * 2.0);
                    self.band.w = 0.0;
                }
                let ear = self.ear;
                self.ear_bar = Rect::new(
                    ear.min_x() + scale_w,
                    ear.min_y() + header,
                    12.0,
                    ear.h - header - footer,
                );
            }
            self.show_bands = self.band.w >= 220.0;
            let (loud, peak) = (self.loud, self.peak);
            self.loud_bars = Rect::new(
                loud.min_x() + scale_w,
                loud.min_y() + header,
                loud.w - scale_w,
                loud.h - header - footer,
            );
            self.peak_bars = Rect::new(
                peak.min_x() + 14.0,
                peak.min_y() + header,
                peak.w - 14.0,
                peak.h - header - footer,
            );
        } else {
            self.show_bands = h >= 250.0;
            self.ear_mode = match (has_spl, self.show_bands) {
                (false, _) => EarMode::None,
                (true, true) => EarMode::Strip,
                (true, false) => EarMode::Row,
            };
            // The strip stands between the two rows of the card, 8 pt from each.
            let strip_h = if self.ear_mode == EarMode::Strip {
                EAR_STRIP_HEIGHT
            } else {
                0.0
            };
            let top_h = if self.show_bands {
                (h - pad * 3.0 - if strip_h > 0.0 { strip_h + 16.0 - pad } else { 0.0 }) * 0.58
            } else {
                h - pad * 2.0
            };
            let (a_w, c_w) = ((w * 0.26).min(124.0), (w * 0.26).min(124.0));
            self.loud = Rect::new(pad, pad, a_w, top_h);
            self.peak = Rect::new(w - pad - c_w, pad, c_w, top_h);
            let (loud, peak) = (self.loud, self.peak);
            self.read = Rect::new(
                loud.max_x() + pad,
                pad,
                (peak.min_x() - loud.max_x() - pad * 2.0).max(60.0),
                top_h,
            );
            if strip_h > 0.0 {
                self.ear = Rect::new(pad, loud.max_y() + 8.0, w - pad * 2.0, strip_h);
            }
            let band_top = if strip_h > 0.0 {
                self.ear.max_y() + 8.0
            } else {
                loud.max_y() + pad
            };
            self.band = Rect::new(pad, band_top, w - pad * 2.0, (h - band_top - pad).max(10.0));
            // The loudness history shares the lower row with the band energy (42 % of it) from 400 pt of width: no
            // card size leaves a dead zone.
            self.compact_spark_width = if self.show_bands && w >= 400.0 {
                ((w - pad * 2.0) * if w >= 520.0 { 0.38 } else { 0.42 }).round()
            } else {
                0.0
            };
            if self.compact_spark_width > 0.0 {
                self.band.w -= self.compact_spark_width + 10.0;
            }
            self.loud_bars = Rect::new(
                loud.min_x() + scale_w,
                loud.min_y() + header,
                loud.w - scale_w,
                loud.h - header - footer,
            );
            self.peak_bars = Rect::new(
                peak.min_x() + scale_w,
                peak.min_y() + header,
                peak.w - scale_w,
                peak.h - header - footer,
            );
        }
        let band = self.band;
        let band_header = if compact { 32.0 } else { header };
        self.band_bars = Rect::new(
            band.min_x() + scale_w,
            band.min_y() + band_header,
            band.w - scale_w,
            band.h - band_header - footer,
        );

        // Readout column: three big numbers, a 2 x 2 grid, and the loudness history in what is left.
        self.spark = Rect::default();
        if !compact {
            let r = self.read;
            self.big_font_size = if r.h > 520.0 {
                36.0
            } else if r.h > 440.0 {
                28.0
            } else {
                22.0
            };
            // Small numerals stand closer together: the loudness history under them keeps a useful height.
            self.big_row_height = self.big_font_size * 0.74 + if self.big_font_size < 28.0 { 36.0 } else { 46.0 };
            self.small_row_height = 50.0;
            self.small_top = r.min_y() + self.big_row_height * 3.0 + 6.0;
            let spark_top = self.small_top + self.small_row_height * 2.0 + 12.0;
            if r.max_y() - spark_top >= 64.0 {
                self.spark = Rect::new(
                    r.min_x() + 6.0,
                    spark_top + 18.0,
                    r.w - 16.0 - 30.0,
                    r.max_y() - spark_top - 18.0 - 18.0,
                );
            }
        } else if self.compact_spark_width > 0.0 {
            // Beside the band bars, on their rows: caption over it, time labels on the row of the band names.
            let x = band.max_x() + 18.0;
            self.spark = Rect::new(
                x,
                self.band_bars.min_y() - 8.0,
                w - pad - x - 24.0,
                self.band_bars.h + 8.0,
            );
        }
        self.ear_plan = if self.ear_mode == EarMode::Column {
            self.plan_ear_column()
        } else {
            EarPlan::default()
        };
        self.strip_bar_cache_key = None;
    }

    fn static_signature(&mut self, base: &PanelBase) -> u64 {
        let mut h = Signature::new();
        h.add(base.static_signature())
            .add(self.has_spl)
            .add(self.dose_standard)
            .add(false)
            .add(self.spark.w > 0.0);
        h.finish()
    }

    fn dynamic_signature(&self, base: &PanelBase) -> u64 {
        let Some(f) = &base.frame else { return 0 };
        let l = &f.loudness;
        let mut h = Signature::new();
        for v in [
            l.momentary_lufs,
            l.short_term_lufs,
            l.integrated_lufs,
            l.loudness_range_lu,
            l.true_peak_max_dbtp,
            l.plr_db,
            l.psr_db,
            l.true_peak_left_dbtp,
            l.true_peak_right_dbtp,
            l.rms_left_db,
            l.rms_right_db,
        ] {
            h.add((v * 10.0).round() as i64);
        }
        for v in f.bands.values() {
            h.add(v.round() as i64);
        }
        h.add(l.clip_count)
            .add(l.is_integrated_valid)
            .add(self.target_lufs.map(f32::to_bits))
            .add(l.measured_seconds < LOUDNESS_RANGE_MINIMUM_SECONDS)
            .add(self.spark_range().0 as i64);
        if let Some(e) = &self.ear_shown {
            h.add(e).add(self.dose_standard);
        }
        h.finish()
    }

    // MARK: State

    fn update(&mut self, base: &mut PanelBase, f: &AnalysisFrame, dt: f64) {
        if f.spl.is_some() != self.has_spl {
            self.has_spl = f.spl.is_some();
            self.layout_changed(base);
            base.needs_display = true;
        }
        match &f.spl {
            Some(s) => {
                // The numbers of the block are taken 4 times per second: an estimate with 2 dB of uncertainty does
                // not need more, and the text is repainted only when one of the whole numbers changed. The bar shows
                // the live value.
                let due = f.host_time - self.ear_latch_time >= EAR_TEXT_INTERVAL || f.host_time < self.ear_latch_time;
                if self.ear_shown.is_none() || due {
                    self.ear_shown = Some(EarShown::new(s, f.is_silent, self.dose_ledger));
                    self.ear_latch_time = f.host_time;
                }
            }
            None => self.ear_shown = None,
        }
        // Loudness history, one slot per 100 ms: the loudest momentary value of the slot, the short-term value at its
        // end. M is a 400 ms window and S a 3 s window: before the window has filled once the value is not a
        // measurement yet (it climbs from silence, which drew a spike at the start of the plot). Those slots stay empty.
        let measured = f.loudness.measured_seconds;
        if measured < self.last_measured_seconds - 0.05 {
            self.slot_max_m = NO_VALUE; // the measurement was reset
        }
        self.last_measured_seconds = measured;
        if measured >= MOMENTARY_WINDOW {
            self.slot_max_m = self.slot_max_m.max(f.loudness.momentary_lufs);
        }
        self.history_bank += dt;
        let mut guard_count = 0;
        while self.history_bank >= HISTORY_STEP && guard_count < 20 {
            self.history_bank -= HISTORY_STEP;
            guard_count += 1;
            self.history_m[self.history_head] = self.slot_max_m;
            self.history_s[self.history_head] = if measured >= SHORT_TERM_WINDOW {
                f.loudness.short_term_lufs
            } else {
                NO_VALUE
            };
            self.history_head = (self.history_head + 1) % HISTORY_SLOTS;
            self.history_count = (self.history_count + 1).min(HISTORY_SLOTS);
            self.slot_max_m = NO_VALUE;
        }
        if self.history_bank > 2.0 {
            self.history_bank = 0.0;
        }
    }

    // MARK: Shapes

    fn draw(&mut self, base: &mut PanelBase, pass: &mut wgpu::RenderPass<'_>) {
        let Some(f) = base.frame.clone() else { return };
        let p = base.palette;
        let l = &f.loudness;
        self.caps.clear();
        let compact = self.compact;
        let (lb, pb) = (self.loud_bars, self.peak_bars);
        let thick = |base: &mut PanelBase, x0: f32, x1: f32, y: f32, color: Color| {
            base.batch.hline(&mut base.arena, x0, x1, y, color, 2.0);
        };

        // ---- Loudness M / S / I. Bar = the number in the readout column.
        let integrated = if l.is_integrated_valid {
            l.integrated_lufs
        } else {
            -120.0
        };
        for (i, v) in [l.momentary_lufs, l.short_term_lufs, integrated]
            .into_iter()
            .enumerate()
        {
            let (x, bw) = self.loud_bar(i);
            track(base, x, bw, lb);
            zoned_bar(base, x, bw, lb, v, LEVEL_MIN, LEVEL_MAX, &self.level_stops);
            if v > LEVEL_MIN {
                self.caps.push(Cap {
                    x,
                    y: y_for(v, LEVEL_MIN, LEVEL_MAX, lb),
                    w: bw,
                    color: self.cap_color,
                });
            }
        }
        // Max hold ticks for M and S.
        for (i, m) in [l.momentary_max_lufs, l.short_term_max_lufs].into_iter().enumerate() {
            if m > LEVEL_MIN {
                let (x, bw) = self.loud_bar(i);
                thick(
                    base,
                    x,
                    x + bw,
                    y_for(m, LEVEL_MIN, LEVEL_MAX, lb),
                    with_alpha(p.text, 0.8),
                );
            }
        }
        // Loudness target on the I bar.
        if let Some(t) = self.target_lufs.filter(|t| *t > LEVEL_MIN && *t < LEVEL_MAX) {
            let (x, bw) = self.loud_bar(2);
            let y = y_for(t, LEVEL_MIN, LEVEL_MAX, lb);
            thick(base, x - 3.0, x + bw + 3.0, y, p.good);
            if !compact {
                base.rounded_rect([x + bw + 2.0, y - 3.0, 6.0, 6.0], p.good, 3.0);
            }
        }

        // ---- True peak + RMS per channel. Bar = the number over it.
        let zero_y = y_for(0.0, LEVEL_MIN, LEVEL_MAX, pb);
        for ch in 0..2 {
            let (x, tp_w, rms_x, rms_w, _) = self.channel_bars(ch);
            track(base, x, tp_w, pb);
            track(base, rms_x, rms_w, pb);
            // Red zone over 0 dBTP.
            base.rounded_rect(
                [x, pb.min_y(), tp_w, zero_y - pb.min_y()],
                with_alpha(p.danger, 0.26),
                2.0,
            );
            let (tp, rms) = if ch == 0 {
                (l.true_peak_left_dbtp, l.rms_left_db)
            } else {
                (l.true_peak_right_dbtp, l.rms_right_db)
            };
            zoned_bar(base, x, tp_w, pb, tp, LEVEL_MIN, LEVEL_MAX, &self.level_stops);
            zoned_bar(base, rms_x, rms_w, pb, rms, LEVEL_MIN, LEVEL_MAX, &self.rms_stops);
            if tp > LEVEL_MIN {
                self.caps.push(Cap {
                    x,
                    y: y_for(tp, LEVEL_MIN, LEVEL_MAX, pb),
                    w: tp_w,
                    color: if tp > -1.0 { p.danger } else { self.cap_color },
                });
            }
        }
        // ---- Shared level grid: one scale through the loudness and the peak bars, drawn over them so it reads
        // everywhere.
        if compact {
            grid_lines(base, lb.min_x() - 3.0, lb.max_x(), lb, LEVEL_MIN, LEVEL_MAX, 12.0);
            grid_lines(base, pb.min_x() - 3.0, pb.max_x(), pb, LEVEL_MIN, LEVEL_MAX, 12.0);
        } else {
            grid_lines(base, lb.min_x() - 3.0, pb.max_x(), lb, LEVEL_MIN, LEVEL_MAX, 6.0);
        }
        base.hline(pb.min_x() - 3.0, pb.max_x(), zero_y, with_alpha(p.danger, 0.7));
        if l.true_peak_max_dbtp > LEVEL_MIN {
            let y = y_for(l.true_peak_max_dbtp, LEVEL_MIN, LEVEL_MAX, pb);
            for ch in 0..2 {
                let (x, tp_w, ..) = self.channel_bars(ch);
                thick(base, x, x + tp_w, y, with_alpha(p.text, 0.8));
            }
        }

        // ---- Band energy: its own scale, behind a divider, with its own tick labels.
        if self.show_bands {
            let bb = self.band_bars;
            let cw = bb.w / 8.0;
            if let Some(b) = self.cursor_band(base) {
                // The band that holds the cursor frequency: a quiet plate behind its bar, its value and its name; an
                // accent foot.
                let up = if compact { 4.0 } else { 18.0 };
                let r = Rect::new(
                    bb.min_x() + b as f32 * cw + 1.0,
                    bb.min_y() - up,
                    cw - 2.0,
                    bb.h + up + 20.0,
                );
                base.rounded_rect([r.x, r.y, r.w, r.h], with_alpha(p.accent, 0.22), 4.0);
                base.rounded_rect([r.x + 3.0, r.max_y() - 2.0, r.w - 6.0, 2.0], p.accent, 1.0);
            }
            grid_lines(base, bb.min_x() - 3.0, bb.max_x(), bb, BAND_MIN, BAND_MAX, 12.0);
            let bar_w = (cw * 0.5).min(30.0);
            let values = f.bands.values();
            for i in 0..8 {
                let x = bb.min_x() + i as f32 * cw + (cw - bar_w) / 2.0;
                track(base, x, bar_w, bb);
                let v = values[i];
                zoned_bar(base, x, bar_w, bb, v, BAND_MIN, BAND_MAX, &self.band_stops[i]);
                if v > BAND_MIN {
                    self.caps.push(Cap {
                        x,
                        y: y_for(v, BAND_MIN, BAND_MAX, bb),
                        w: bar_w,
                        color: mix(self.band_stops[i][1].1, WHITE, 0.35),
                    });
                }
            }
        }

        // ---- Clip indicator: a lamp, not a button. Dark ring when clean, latched red with the count after a clip.
        let lamp = self.clip_lamp_center();
        let clipped = l.clip_count > 0;
        if clipped {
            base.circle(lamp, 4.5, p.danger, 0.0, 0.0);
        } else {
            base.circle(lamp, 4.5, p.track, 0.0, 0.0);
            base.circle(lamp, 4.5, with_alpha(p.text_faint, 0.7), 1.0, 0.0);
        }

        // Section dividers.
        if !compact {
            let (loud, band) = (self.loud, self.band);
            for x in [self.read.min_x() - 10.0, band.min_x() - 7.0] {
                if self.show_bands || x < band.min_x() - 8.0 {
                    base.vline(x, loud.min_y() + 4.0, loud.max_y() - 4.0, p.grid_major);
                }
            }
        }
        if let Some(s) = f.spl.as_ref().filter(|_| self.ear_mode != EarMode::None) {
            self.draw_ear(base, s);
        }
        if self.spark.w > 0.0 {
            self.draw_spark_frame(base);
        }
        base.flush(pass);

        if self.spark.w > 0.0 {
            self.draw_spark_curves(base, pass);
        }

        // Bar caps: a bright line with a soft additive halo.
        let halo = if compact { 2.2 } else { 3.0 };
        for c in &self.caps {
            let color = with_alpha(c.color, 0.34);
            base.batch.gradient_rect(
                &mut base.arena,
                [c.x, c.y - 1.5, c.w, 3.0],
                color,
                color,
                1.5,
                halo,
                0.0,
            );
        }
        if clipped {
            base.circle(lamp, 5.0, with_alpha(p.danger, 0.55), 0.0, 4.0);
        }
        base.flush_add(pass);
        for c in &self.caps {
            base.rounded_rect([c.x, c.y - 0.75, c.w, 1.5], mix(c.color, WHITE, 0.6), 0.75);
        }
        base.flush(pass);
    }

    // MARK: Text

    fn draw_static(&mut self, base: &PanelBase, o: &mut OverlayContext) {
        let p = &base.palette;
        let compact = self.compact;
        let cap = self.cap_font();
        let small = fonts::mono(if compact { 10.0 } else { 11.0 }, Weight::Regular);
        let label = fonts::ui(11.0, Weight::Medium);
        let (left, top) = (HAlign::Left, VAlign::Top);

        let scale = |o: &mut OverlayContext, r: Rect, lo: f32, hi: f32, step: f32| {
            let mut v = (hi / step).floor() * step;
            let mut last_y = -100.0;
            while v >= lo - 0.01 {
                let y = y_for(v, lo, hi, r);
                if y - last_y > 13.0 {
                    let text = fmt::number(v, 0, false);
                    o.text(
                        &text,
                        r.min_x() - 7.0,
                        y,
                        &small,
                        p.text_dim,
                        HAlign::Right,
                        VAlign::Middle,
                    );
                    last_y = y;
                }
                v -= step;
            }
        };

        let (loud, band) = (self.loud, self.band);
        let title = if compact { "LUFS" } else { "LOUDNESS" };
        o.text_tracked(
            title,
            loud.min_x(),
            loud.min_y() + 4.0,
            &cap,
            p.text_faint,
            left,
            top,
            1.0,
        );
        if !compact {
            o.text(
                "LUFS",
                loud.min_x(),
                loud.min_y() + 20.0,
                &small,
                p.text_faint,
                left,
                top,
            );
        }
        scale(o, self.loud_bars, LEVEL_MIN, LEVEL_MAX, 12.0);
        for (i, name) in ["M", "S", "I"].into_iter().enumerate() {
            let (x, bw) = self.loud_bar(i);
            o.text(
                name,
                x + bw / 2.0,
                self.loud_bars.max_y() + 7.0,
                &label,
                p.text,
                HAlign::Center,
                top,
            );
        }

        // A narrow card has room for the clip indicator and two letters.
        // The caption of the peak group is drawn with the clip indicator (`draw_dynamic`): the two share one row and
        // are measured together, so they never touch.
        let pb = self.peak_bars;
        if compact {
            scale(o, pb, LEVEL_MIN, LEVEL_MAX, 12.0);
        }
        for (i, name) in ["L", "R"].into_iter().enumerate() {
            let (x, .., width) = self.channel_bars(i);
            let color = if i == 0 { p.left } else { p.right };
            o.text(
                name,
                x + width / 2.0,
                pb.max_y() + 7.0,
                &label,
                color,
                HAlign::Center,
                top,
            );
        }
        if !compact {
            // Row tags of the two numbers over each channel, in the color of their bar.
            let tag_x = pb.min_x() - 4.0;
            let tp_color = mix(self.cap_color, WHITE, 0.3);
            o.text(
                "TP",
                tag_x,
                pb.min_y() - 24.0,
                &small,
                tp_color,
                HAlign::Right,
                VAlign::Middle,
            );
            let rms_color = Self::rms_text_color(base);
            o.text(
                "RMS",
                tag_x,
                pb.min_y() - 9.0,
                &small,
                rms_color,
                HAlign::Right,
                VAlign::Middle,
            );
        }

        if self.show_bands {
            let title = if compact { "BAND ENERGY  dB" } else { "BAND ENERGY" };
            o.text_tracked(
                title,
                band.min_x(),
                band.min_y() + 4.0,
                &cap,
                p.text_faint,
                left,
                top,
                1.0,
            );
            if !compact {
                let note = "dB RMS, mid channel";
                o.text(note, band.min_x(), band.min_y() + 20.0, &small, p.text_faint, left, top);
            }
            let bb = self.band_bars;
            scale(o, bb, BAND_MIN, BAND_MAX, if compact { 24.0 } else { 12.0 });
            let cw = bb.w / 8.0;
            let name_font = fonts::ui(if cw < 44.0 { 10.0 } else { 11.0 }, Weight::Regular);
            let names = band_labels::fitting(o, &name_font, cw, 4.0, base.width);
            for i in 0..8 {
                let x = bb.min_x() + i as f32 * cw + cw / 2.0;
                o.text(
                    names[i],
                    x,
                    bb.max_y() + 7.0,
                    &name_font,
                    p.text_dim,
                    HAlign::Center,
                    top,
                );
            }
        }

        if self.ear_mode != EarMode::None {
            self.draw_ear_static(base, o);
        }

        let spark = self.spark;
        if spark.w > 0.0 {
            let wide = spark.w >= 190.0;
            let title = if wide { "LOUDNESS HISTORY" } else { "HISTORY" };
            let y = spark.min_y() - 7.0;
            o.text_tracked(title, spark.min_x(), y, &cap, p.text_faint, left, VAlign::Bottom, 1.0);
            let labels = [
                (60.0, format!("{}60 s", fmt::MINUS), HAlign::Left),
                (30.0, format!("{}30 s", fmt::MINUS), HAlign::Center),
                (0.0, "now".to_string(), HAlign::Right),
            ];
            for (s, text, h) in labels {
                if !(wide || s != 30.0) {
                    continue;
                }
                let x = spark.max_x() - spark.w * s / 60.0;
                let color = if s == 0.0 { p.text } else { p.text_dim };
                o.text(
                    &text,
                    x,
                    spark.max_y() + if compact { 7.0 } else { 5.0 },
                    &small,
                    color,
                    h,
                    top,
                );
            }
            // Legend: M thin, S bold.
            let mut x = spark.max_x();
            let line_y = spark.min_y() - 11.0;
            x -= o.text("S", x, y, &small, p.text_dim, HAlign::Right, VAlign::Bottom) + 4.0;
            o.line([x - 14.0, line_y], [x, line_y], mix(p.accent, WHITE, 0.65), 2.0, &[]);
            x -= 24.0;
            x -= o.text("M", x, y, &small, p.text_dim, HAlign::Right, VAlign::Bottom) + 4.0;
            o.line([x - 14.0, line_y], [x, line_y], with_alpha(p.accent, 0.85), 1.0, &[]);
        }
    }

    fn draw_dynamic(&mut self, base: &mut PanelBase, o: &mut OverlayContext) {
        let Some(f) = base.frame.clone() else { return };
        let base: &PanelBase = base;
        let p = &base.palette;
        let l = &f.loudness;
        let compact = self.compact;
        let cap = self.cap_font();
        let (left, right_align, top, baseline) = (HAlign::Left, HAlign::Right, VAlign::Top, VAlign::Baseline);
        // I, LRA and PLR are measurements only after the first gated block.
        let no_program = !l.is_integrated_valid || fmt::is_floor(l.integrated_lufs);
        // The loudness range of the first seconds is not a range yet: a dash until 30 s are measured.
        let no_range = l.measured_seconds < LOUDNESS_RANGE_MINIMUM_SECONDS;
        let dash = || fmt::DASH.to_string();

        let tp_color = if l.true_peak_max_dbtp > 0.0 {
            p.danger
        } else if l.true_peak_max_dbtp > -1.0 {
            p.warn
        } else {
            p.text
        };
        let item = |label: &str, value: String, unit: &'static str, color: Color| Item {
            label: label.to_string(),
            value,
            unit,
            color,
        };
        let big_items = [
            item("MOMENTARY", fmt::db(l.momentary_lufs, 1, false), "LUFS", p.text),
            item("SHORT TERM", fmt::db(l.short_term_lufs, 1, false), "LUFS", p.text),
            item(
                "INTEGRATED",
                if no_program {
                    dash()
                } else {
                    fmt::db(l.integrated_lufs, 1, false)
                },
                "LUFS",
                mix(p.accent, WHITE, 0.45),
            ),
        ];
        let tp_floor = fmt::is_floor(l.true_peak_max_dbtp);
        // PSR here is the contract's number: the live true peak (the higher of the two bars) minus short-term
        // loudness. It is not "true peak max minus S": the caption says which peak it uses.
        let small_items = [
            item(
                "RANGE  LRA",
                if no_program || no_range {
                    dash()
                } else {
                    fmt::number(l.loudness_range_lu, 1, false)
                },
                "LU",
                p.text,
            ),
            item(
                "TRUE PEAK MAX",
                fmt::db(l.true_peak_max_dbtp, 1, true),
                "dBTP",
                tp_color,
            ),
            item(
                &format!("PLR  TP MAX {} I", fmt::MINUS),
                if no_program || tp_floor {
                    dash()
                } else {
                    fmt::number(l.plr_db, 1, false)
                },
                "dB",
                p.text,
            ),
            item(
                &format!("PSR  LIVE TP {} S", fmt::MINUS),
                if fmt::is_floor(l.short_term_lufs) || tp_floor {
                    dash()
                } else {
                    fmt::number(l.psr_db, 1, false)
                },
                "dB",
                p.text,
            ),
        ];

        let r = self.read;
        // A value that is not there: a short dash under the left edge of its caption, in the dim color, the same in
        // every cell.
        let dash_font = fonts::ui(if compact { 12.0 } else { 15.0 }, Weight::Regular);
        let target_text = |t: f32| format!("TARGET {}", fmt::number(t, 0, false));
        let delta_text = |d: f32| format!("{} LU", fmt::number(d, 1, true));
        if !compact {
            let big_font = fonts::ui(self.big_font_size, Weight::Light);
            let unit_font = fonts::ui(11.0, Weight::Regular);
            let mono = fonts::mono(11.0, Weight::Regular);
            let mut y = r.min_y() + 2.0;
            let num_w = o.measure(&format!("{}00.0", fmt::MINUS), &big_font, 0.0);
            let cap_height = big_font.cap_height();
            for (i, it) in big_items.iter().enumerate() {
                o.text_tracked(&it.label, r.min_x() + 6.0, y + 2.0, &cap, p.text_faint, left, top, 1.0);
                let base_y = y + 18.0 + cap_height + 6.0;
                if it.value == fmt::DASH {
                    o.text(
                        fmt::DASH,
                        r.min_x() + 6.0,
                        base_y,
                        &dash_font,
                        p.text_dim,
                        left,
                        baseline,
                    );
                } else {
                    o.text(
                        &it.value,
                        r.min_x() + 6.0 + num_w,
                        base_y,
                        &big_font,
                        it.color,
                        right_align,
                        baseline,
                    );
                }
                o.text(
                    it.unit,
                    r.min_x() + 6.0 + num_w + 7.0,
                    base_y,
                    &unit_font,
                    p.text_dim,
                    left,
                    baseline,
                );
                if let Some(t) = self.target_lufs.filter(|_| i == 2) {
                    // Distance to the loudness target.
                    let x = r.min_x() + 6.0 + num_w + 48.0;
                    if cap_height >= 24.0 {
                        o.text(&target_text(t), x, base_y - cap_height + 1.0, &mono, p.good, left, top);
                    } else {
                        // Small numerals: no room for two lines beside them. The target goes on the caption row.
                        o.text(
                            &target_text(t),
                            r.max_x() - 12.0,
                            y + 2.0,
                            &mono,
                            p.good,
                            right_align,
                            top,
                        );
                    }
                    if !no_program {
                        let d = l.integrated_lufs - t;
                        let font = fonts::ui(14.0, Weight::Medium);
                        o.text(
                            &delta_text(d),
                            x,
                            base_y,
                            &font,
                            target_delta_color(d, p),
                            left,
                            baseline,
                        );
                    }
                }
                y += self.big_row_height;
            }
            // Secondary numbers: a 2 x 2 grid.
            let grid_top = self.small_top;
            let col_w = r.w / 2.0;
            let med_font = fonts::ui(19.0, Weight::Regular);
            let m_w = o.measure("+00.0", &med_font, 0.0);
            let line_y = grid_top - 4.0;
            o.line(
                [r.min_x() + 6.0, line_y],
                [r.max_x() - 10.0, line_y],
                p.grid_major,
                1.0,
                &[],
            );
            for (i, it) in small_items.iter().enumerate() {
                let x = r.min_x() + 6.0 + (i % 2) as f32 * col_w;
                let yy = grid_top + (i / 2) as f32 * self.small_row_height + 5.0;
                o.text_tracked(&it.label, x, yy, &cap, p.text_faint, left, top, 0.6);
                let base_y = yy + 16.0 + med_font.cap_height() + 5.0;
                if it.value == fmt::DASH {
                    o.text(fmt::DASH, x, base_y, &dash_font, p.text_dim, left, baseline);
                } else {
                    o.text(&it.value, x + m_w, base_y, &med_font, it.color, right_align, baseline);
                }
                o.text(it.unit, x + m_w + 5.0, base_y, &unit_font, p.text_dim, left, baseline);
            }
        } else {
            // Compact: M / S / I on the left, the four secondary numbers on the right.
            let two_cols = r.w >= 150.0;
            // Without band energy the ear estimate is a fourth row under M / S / I (and the dose a fifth small number).
            let ear_row = self.ear_mode == EarMode::Row;
            let row_h = r.h / if ear_row { 4.0 } else { 3.0 };
            let large = if ear_row { row_h >= 56.0 } else { r.h > 150.0 };
            let big_font = fonts::ui(if large { 19.0 } else { 15.0 }, Weight::Regular);
            let num_w = o.measure(&format!("{}00.0", fmt::MINUS), &big_font, 0.0);
            for (i, it) in big_items.iter().enumerate() {
                let y = r.min_y() + i as f32 * row_h;
                o.text_tracked(
                    ["M", "S", "I"][i],
                    r.min_x(),
                    y + 4.0,
                    &cap,
                    p.text_faint,
                    left,
                    top,
                    1.0,
                );
                let base_y = y + 18.0 + big_font.cap_height();
                if it.value == fmt::DASH {
                    o.text(fmt::DASH, r.min_x(), base_y, &dash_font, p.text_dim, left, baseline);
                } else {
                    o.text(
                        &it.value,
                        r.min_x() + num_w,
                        base_y,
                        &big_font,
                        it.color,
                        right_align,
                        baseline,
                    );
                }
                if let Some(t) = self.target_lufs.filter(|_| i == 2 && !no_program) {
                    let d = l.integrated_lufs - t;
                    let font = fonts::mono(10.0, Weight::Regular);
                    o.text(
                        &delta_text(d),
                        r.min_x() + 14.0,
                        y + 4.0,
                        &font,
                        target_delta_color(d, p),
                        left,
                        top,
                    );
                }
            }
            // The two columns share the width evenly: no dead zone beside them in a wide card.
            let x = (r.min_x() + num_w + 18.0).max(r.min_x() + (r.w * 0.52).round());
            if let Some(e) = self.ear_shown.as_ref().filter(|_| ear_row) {
                let width = if two_cols { x - r.min_x() - 10.0 } else { r.w };
                let y = r.min_y() + 3.0 * row_h;
                self.draw_ear_row(base, o, e, r.min_x(), y, width, row_h, &big_font, !two_cols);
            }
            if two_cols {
                let f2 = fonts::ui(12.0, Weight::Regular);
                let rh = r.h / if ear_row { 5.0 } else { 4.0 };
                let w2 = o.measure("+00.0", &f2, 0.0);
                let cap_height = f2.cap_height();
                for (i, it) in small_items.iter().enumerate() {
                    let y = r.min_y() + i as f32 * rh;
                    let short = ["LRA", "TP MAX", "PLR", "PSR live"][i];
                    o.text_tracked(short, x, y + 4.0, &cap, p.text_faint, left, top, 0.6);
                    let base_y = y + 17.0 + cap_height;
                    if it.value == fmt::DASH {
                        o.text(fmt::DASH, x, base_y, &dash_font, p.text_dim, left, baseline);
                    } else {
                        o.text(&it.value, x + w2, base_y, &f2, it.color, right_align, baseline);
                    }
                }
                if ear_row && self.ear_shown.is_some() {
                    // The dose of the ear estimate: the fifth small number.
                    let y = r.min_y() + 4.0 * rh;
                    let (percent, _) = self.dose_value();
                    o.text_tracked("DOSE", x, y + 4.0, &cap, p.text_faint, left, top, 0.6);
                    let color = if percent >= 80 {
                        Self::dose_color(base, percent)
                    } else {
                        p.text
                    };
                    let text = format!("{APPROX} {} %", percent.min(999));
                    o.text(&text, x, y + 17.0 + cap_height, &f2, color, left, baseline);
                }
            }
        }

        if let Some(e) = self.ear_shown.as_ref().filter(|_| self.ear_mode != EarMode::None) {
            self.draw_ear_dynamic(base, o, e);
        }

        let spark = self.spark;
        if spark.w > 0.0 {
            let (range_top, range_bottom) = self.spark_range();
            let f11 = fonts::mono(if compact { 10.0 } else { 11.0 }, Weight::Regular);
            let x = spark.max_x() + 4.0;
            o.text(
                &fmt::number(range_top, 0, false),
                x,
                spark.min_y(),
                &f11,
                p.text_dim,
                left,
                top,
            );
            o.text(
                &fmt::number(range_bottom, 0, false),
                x,
                spark.max_y(),
                &f11,
                p.text_dim,
                left,
                VAlign::Bottom,
            );
            // Inner ticks on the 12 LU lines, where they keep 13 pt from the end labels.
            let mut v = (range_top / 12.0).floor() * 12.0;
            while v > range_bottom {
                let y = y_for(v, range_bottom, range_top, spark);
                if v < range_top && y - spark.min_y() > 17.0 && spark.max_y() - y > 17.0 {
                    o.text(
                        &fmt::number(v, 0, false),
                        x,
                        y,
                        &f11,
                        p.text_faint,
                        left,
                        VAlign::Middle,
                    );
                }
                v -= 12.0;
            }
        }

        // Bar-side numbers.
        let vf = fonts::mono(if compact { 10.0 } else { 11.0 }, Weight::Regular);
        let pb = self.peak_bars;
        if self.show_bands {
            let bb = self.band_bars;
            let cw = bb.w / 8.0;
            for (i, v) in f.bands.values().into_iter().enumerate() {
                let x = bb.min_x() + i as f32 * cw + cw / 2.0;
                o.text(
                    &fmt::db(v, 0, false),
                    x,
                    bb.min_y() - 5.0,
                    &vf,
                    p.text_dim,
                    HAlign::Center,
                    VAlign::Bottom,
                );
            }
        }
        if !compact {
            // Live true peak and RMS per channel, over the bars: the same values the bars show.
            for ch in 0..2 {
                let (x, .., width) = self.channel_bars(ch);
                let (tp, rms) = if ch == 0 {
                    (l.true_peak_left_dbtp, l.rms_left_db)
                } else {
                    (l.true_peak_right_dbtp, l.rms_right_db)
                };
                let cx = x + width / 2.0;
                o.text(
                    &fmt::db(tp, 1, true),
                    cx,
                    pb.min_y() - 24.0,
                    &vf,
                    p.text,
                    HAlign::Center,
                    VAlign::Middle,
                );
                let color = Self::rms_text_color(base);
                o.text(
                    &fmt::db(rms, 1, false),
                    cx,
                    pb.min_y() - 9.0,
                    &vf,
                    color,
                    HAlign::Center,
                    VAlign::Middle,
                );
            }
        }

        // Clip indicator text: count of clipped runs since the last reset.
        let lamp = self.clip_lamp_center();
        let clipped = l.clip_count > 0;
        let count = if l.clip_count > 999 {
            "999+".to_string()
        } else {
            l.clip_count.to_string()
        };
        let weight = if clipped { Weight::Bold } else { Weight::Medium };
        let clip_font = fonts::ui(if compact { 10.0 } else { 11.0 }, weight);
        let clip_color = if clipped { p.danger } else { p.text_faint };
        let peak = self.peak;
        if compact {
            // One row: caption left, clip indicator right. At least 10 pt between them: the caption shortens first
            // (TRUE PEAK, TP, none), then the clip text loses its count. The word and the lamp stay: a bare "999+"
            // beside a red dot says nothing to a reader who has not seen the large layout.
            let (x0, x1) = (peak.min_x(), lamp[0] - 9.0);
            let room = x1 - x0;
            let mut clip_text = format!("CLIP {count}");
            let w_full = o.measure(&clip_text, &clip_font, 0.6);
            // The column is TRUE PEAK, or TP where that is too long: one name, never "PEAK".
            let caption = ["TRUE PEAK", "TP"]
                .into_iter()
                .find(|c| o.measure(c, &cap, 1.0) + 10.0 + w_full <= room);
            if caption.is_none() && w_full > room {
                clip_text = "CLIP".to_string();
            }
            if let Some(caption) = caption {
                o.text_tracked(caption, x0, lamp[1], &cap, p.text_faint, left, VAlign::Middle, 1.0);
            }
            o.text_tracked(
                &clip_text,
                x1,
                lamp[1],
                &clip_font,
                clip_color,
                right_align,
                VAlign::Middle,
                0.6,
            );
        } else {
            o.text_tracked(
                "TRUE PEAK",
                pb.min_x(),
                peak.min_y() + 4.0,
                &cap,
                p.text_faint,
                left,
                top,
                1.0,
            );
            let text = format!("CLIP {count}");
            o.text_tracked(
                &text,
                lamp[0] - 9.0,
                lamp[1],
                &clip_font,
                clip_color,
                right_align,
                VAlign::Middle,
                0.6,
            );
        }
        // Last: it takes the longest form that stands clear of everything above.
        self.draw_cursor_caption(base, o);
    }

    // MARK: Linked cursor

    fn cursor_change_shows(&self, base: &PanelBase, old: Option<PanelCursor>) -> bool {
        let band = |c: Option<PanelCursor>| c.and_then(|c| cursor::band(c.frequency_hz));
        band(old) != band(base.cursor)
    }

    fn combine_cursor_signature(&self, _base: &PanelBase, c: &PanelCursor, h: &mut Signature) {
        h.add(cursor::band(c.frequency_hz));
    }

    fn cursor_items(&mut self, base: &PanelBase) -> Vec<CursorReadoutItem> {
        let (Some(c), Some(f)) = (base.cursor, base.frame.as_ref()) else {
            return Vec::new();
        };
        let Some(b) = cursor::band(c.frequency_hz) else {
            return Vec::new();
        };
        vec![
            CursorReadoutItem::new(
                format!("{} {}", BandEnergy::NAMES[b], cursor::band_range(b)),
                0,
                Tone::Value,
            ),
            CursorReadoutItem::new(format!("{} dB", fmt::db(f.bands.values()[b], 1, false)), 1, Tone::Value),
        ]
    }

    fn accessibility_label(&self) -> String {
        "Loudness and level meters".to_string()
    }

    fn accessibility_value(&self, base: &PanelBase) -> String {
        let Some(f) = &base.frame else {
            return "No signal".to_string();
        };
        let l = &f.loudness;
        let s = |v: f32, unit: &str| {
            if fmt::is_floor(v) {
                "no value".to_string()
            } else {
                format!("{v:.1} {unit}")
            }
        };
        let valid = l.is_integrated_valid;
        let none = || "no value".to_string();
        let mut parts = vec![
            format!("Momentary {}", s(l.momentary_lufs, "LUFS")),
            format!("short term {}", s(l.short_term_lufs, "LUFS")),
            format!(
                "integrated {}",
                if valid { s(l.integrated_lufs, "LUFS") } else { none() }
            ),
            format!(
                "range {}",
                if valid && l.measured_seconds >= LOUDNESS_RANGE_MINIMUM_SECONDS {
                    format!("{:.1} LU", l.loudness_range_lu)
                } else {
                    none()
                }
            ),
            format!("true peak max {}", s(l.true_peak_max_dbtp, "dBTP")),
            format!(
                "PLR {}",
                if valid && !fmt::is_floor(l.true_peak_max_dbtp) {
                    format!("{:.1} dB", l.plr_db)
                } else {
                    none()
                }
            ),
            format!("PSR {:.1} dB", l.psr_db),
        ];
        if let Some(t) = self.target_lufs.filter(|_| valid) {
            parts.push(format!(
                "{:+.1} LU against the {t:.0} LUFS target",
                l.integrated_lufs - t
            ));
        }
        if l.clip_count > 0 {
            parts.push(format!("{} clipped runs", l.clip_count));
        }
        if let Some(spl) = &f.spl {
            parts.push(self.ear_accessibility_text(spl, f.is_silent));
        }
        parts.join(", ")
    }
}
