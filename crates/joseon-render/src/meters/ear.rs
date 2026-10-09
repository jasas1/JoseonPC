//! "Level at the ear": the SPL estimate of `frame.spl` in the meters panel.
//!
//! Honesty rules of this block:
//! - Every number is an estimate. It shows "≈" or stands under a header that
//!   says "estimate", and the block names the calibration and its uncertainty.
//!   Whole dB only: a decimal would claim a precision the chain does not have.
//! - The bar is the fast level, live. The pointer beside it is the slow level:
//!   the big numeral. The tick is the maximum.
//! - One number, one text: the "±" prints through `uncertainty_text`, and the
//!   dose is the app's ledger (`EarDoseLedger`) whenever the app gives one.
//! - The zones are about hearing dose (70 / 85 / 100 dB(A)), not about the
//!   signal. The 85 line carries its rule: 8 h.
//! - The block never competes with the loudness numerals, and nothing of it is
//!   drawn while `frame.spl` is `None`.
//!
//! Port source: `Sources/JoseonRender/MetersRenderer+Ear.swift`.

use super::{track, y_for, zoned_bar, Cap, EarMode, MetersPanel, Stops};
use crate::contracts::DoseStandard;
use crate::fmt;
use crate::gpu::Color;
use crate::palette::{mix, scaled_alpha, with_alpha, WHITE};
use crate::panel::PanelBase;
use crate::text::fonts::{self, Font, Weight};
use crate::text::{HAlign, OverlayContext, Rect, VAlign};
use joseon_core::contracts::SplReading;

/// The ONE way a "±" figure of the level at the ear prints.
pub mod uncertainty_text {
    /// A TOTAL (voltage and sensitivity together): whole dB. 3.6 dB prints as "± 4 dB".
    pub fn total(db: f64, unit: bool) -> String {
        let v = if db.is_finite() { db } else { 0.0 }.max(0.0).round();
        format!("\u{00B1} {v:.0}{}", if unit { " dB" } else { "" })
    }

    /// One TERM of the total (the voltage of a method, the sensitivity). The fixed terms are whole dB. A measured
    /// sensitivity brings a half step: it prints as it is, never rounded a second time.
    pub fn term(db: f64, unit: bool) -> String {
        let half = (if db.is_finite() { db } else { 0.0 }.max(0.0) * 2.0).round() / 2.0;
        let number = if half == half.round() {
            format!("{half:.0}")
        } else {
            format!("{half:.1}")
        };
        format!("\u{00B1} {number}{}", if unit { " dB" } else { "" })
    }
}

/// The hearing dose the APP keeps: it is stored, the daily share starts again at midnight and the weekly share with
/// the ISO week. 1.0 = 100 %.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct EarDoseLedger {
    pub niosh_today: f64,
    pub who_week: f64,
}

/// What the text of the block shows: whole numbers, taken from the reading at most 4 times per second. Equal values
/// = equal text, so the text layer repaints only when a number changed.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct EarShown {
    pub slow: i32,
    pub fast: i32,
    pub leq_track: i32,
    pub leq_session: i32,
    pub max_fast: i32,
    pub dose_niosh: i32,
    pub dose_who: i32,
    /// Minutes, in the steps of `time_text`. -1 = infinite, -2 = silent.
    pub left_niosh: i32,
    pub left_who: i32,
    pub calibration_name: String,
    /// `uncertainty_text::total` of the reading's uncertainty.
    pub uncertainty_text: String,
}

impl EarShown {
    pub const NONE: i32 = i32::MIN;

    /// `ledger`: the dose the app keeps. `None` = the running dose of the reading.
    pub fn new(s: &SplReading, silent: bool, ledger: Option<EarDoseLedger>) -> Self {
        let level = |v: f32| {
            if v.is_finite() && v > SplReading::FLOOR_DB + 0.5 {
                v.round() as i32
            } else {
                Self::NONE
            }
        };
        let percent = |d: f64| {
            if d.is_finite() {
                (d.max(0.0) * 100.0).round() as i32
            } else {
                0
            }
        };
        let slow = if silent { Self::NONE } else { level(s.level_a_slow) };
        let niosh = ledger.map_or(s.dose_niosh as f64, |l| l.niosh_today);
        let who = ledger.map_or(s.dose_who_weekly as f64, |l| l.who_week);
        let (mut left_niosh, mut left_who) = (-2, -2);
        if slow != Self::NONE {
            left_niosh = if ledger.is_none() || !s.seconds_to_niosh_limit.is_finite() {
                // Infinite = the estimator says this level uses up no allowance (under the NIOSH threshold).
                Self::stepped_minutes(s.seconds_to_niosh_limit)
            } else {
                // NIOSH: 85 dB(A) for 8 h, 3 dB exchange rate; the rest of TODAY's allowance at this level.
                let allowed = 8.0 * 3600.0 * 2f64.powf((85.0 - s.level_a_slow as f64) / 3.0);
                Self::stepped_minutes((1.0 - niosh).max(0.0) * allowed)
            };
            // WHO / ITU H.870 adults: 80 dB(A) for 40 h, 3 dB exchange rate. More than a week of time left = no
            // limit in reach.
            let allowed = 40.0 * 3600.0 * 2f64.powf((80.0 - s.level_a_slow as f64) / 3.0);
            let left = (1.0 - who).max(0.0) * allowed;
            left_who = if left > 168.0 * 3600.0 {
                -1
            } else {
                Self::stepped_minutes(left)
            };
        }
        Self {
            slow,
            fast: if silent { Self::NONE } else { level(s.level_a_fast) },
            leq_track: level(s.leq_a_track),
            leq_session: level(s.leq_a_session),
            max_fast: level(s.max_a_fast),
            dose_niosh: percent(niosh),
            dose_who: percent(who),
            left_niosh,
            left_who,
            calibration_name: s.calibration_name.clone(),
            uncertainty_text: uncertainty_text::total(s.uncertainty_db as f64, true),
        }
    }

    /// Time left in minutes, in steps that fit an estimate (1 dB is 26 % of the time): 5 min under 2 h, 15 min
    /// under 10 h, then whole hours. -1 = infinite.
    pub fn stepped_minutes(seconds: f64) -> i32 {
        if !seconds.is_finite() {
            return -1;
        }
        let m = seconds.max(0.0) / 60.0;
        let step = if m < 120.0 {
            5.0
        } else if m < 600.0 {
            15.0
        } else {
            60.0
        };
        ((m / step).floor() * step) as i32
    }

    /// h:mm, "∞" when infinite, a dash while silent.
    pub fn time_text(minutes: i32) -> String {
        if minutes == -1 {
            return "\u{221E}".to_string();
        }
        if minutes < 0 {
            return fmt::DASH.to_string();
        }
        if minutes >= 100 * 60 {
            return "> 99 h".to_string();
        }
        format!("{}:{:02}", minutes / 60, minutes % 60)
    }

    pub fn level_text(v: i32) -> String {
        if v == Self::NONE {
            fmt::DASH.to_string()
        } else {
            v.to_string()
        }
    }
}

/// Where the parts of the ear column stand (points). Made in `layout_changed`, so text and shapes agree.
#[derive(Debug, Clone, PartialEq)]
pub struct EarPlan {
    pub content_x: f32,
    pub caption_y: f32,
    pub numeral_base: f32,
    pub numeral_size: f32,
    /// Tops of the rows of the level table (at most 4: fast, max, Leq track, Leq session). The table flows down from
    /// the numeral and steps over the label of the 85 line.
    pub rows: Vec<f32>,
    pub y85: f32,
    pub shows_label85: bool,
    pub dose_top: Option<f32>,
}

impl Default for EarPlan {
    fn default() -> Self {
        Self {
            content_x: 0.0,
            caption_y: 0.0,
            numeral_base: 0.0,
            numeral_size: 24.0,
            rows: Vec::new(),
            y85: 0.0,
            shows_label85: false,
            dose_top: None,
        }
    }
}

impl EarPlan {
    pub const ROW_HEIGHT: f32 = 21.0;
    pub const RING_RADIUS: f32 = 25.0;
    pub const DOSE_HEIGHT: f32 = 84.0;
}

/// Geometry of the strip's line of values. Widths come from the widest strings, so nothing moves when digits change.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct StripPlan {
    pub numeral_right: f32,
    pub bar: Rect,
    pub time_right: Option<f32>,
    pub dose_right: f32,
    pub line_y: f32,
}

pub const EAR_COLUMN_WIDTH: f32 = 226.0;
pub const EAR_STRIP_HEIGHT: f32 = 40.0;
pub const EAR_MIN: f32 = 40.0;
pub const EAR_MAX: f32 = 110.0;
pub const LABEL_85: &str = "85 dB(A) \u{00B7} 8 h";
pub const APPROX: &str = "\u{2248}";
const ZONE_EDGES: [f32; 5] = [40.0, 70.0, 85.0, 100.0, 110.0];

fn strip_numeral_font() -> Font {
    fonts::ui(17.0, Weight::Regular)
}
fn strip_approx_font() -> Font {
    fonts::ui(13.0, Weight::Regular)
}
fn strip_unit_font() -> Font {
    fonts::ui(10.0, Weight::Regular)
}
fn strip_dose_font() -> Font {
    fonts::ui(13.0, Weight::Medium)
}

impl MetersPanel {
    // MARK: Zones

    /// Under 70 calm, 70 ... 85 neutral, 85 ... 100 amber, over 100 red. About hearing dose.
    pub fn ear_zone_color(base: &PanelBase, db: f32) -> Color {
        let p = &base.palette;
        if db >= 100.0 {
            return p.danger;
        }
        if db >= 85.0 {
            return p.warn;
        }
        // Calm and neutral speak the language of the other bars: deep blue, then the bright cyan of their upper part.
        if db >= 70.0 {
            return with_alpha(mix(p.accent, [0.35, 0.95, 1.0, 1.0], 0.6), 1.0);
        }
        with_alpha(mix(p.accent, p.background, 0.18), 1.0)
    }

    fn ear_stops(base: &PanelBase) -> Stops {
        let mut out = Vec::with_capacity(8);
        for i in 0..4 {
            let c = Self::ear_zone_color(base, ZONE_EDGES[i]);
            out.push((
                ZONE_EDGES[i] + if i == 0 { 0.0 } else { 0.001 },
                with_alpha(mix(c, base.palette.background, 0.25), 1.0),
            ));
            out.push((ZONE_EDGES[i + 1], c));
        }
        out
    }

    /// The percent and the minutes left of the chosen dose rule.
    pub fn dose_value(&self) -> (i32, i32) {
        match (&self.ear_shown, self.dose_standard) {
            (None, _) => (0, -2),
            (Some(e), DoseStandard::NioshDaily) => (e.dose_niosh, e.left_niosh),
            (Some(e), DoseStandard::WhoWeekly) => (e.dose_who, e.left_who),
        }
    }

    pub fn dose_caption(&self) -> &'static str {
        match self.dose_standard {
            DoseStandard::NioshDaily => "DOSE \u{00B7} NIOSH DAY",
            DoseStandard::WhoWeekly => "DOSE \u{00B7} WHO WEEK",
        }
    }

    pub fn dose_color(base: &PanelBase, percent: i32) -> Color {
        let p = &base.palette;
        if percent >= 100 {
            p.danger
        } else if percent >= 80 {
            p.warn
        } else {
            with_alpha(mix(p.accent, WHITE, 0.35), 1.0)
        }
    }

    /// The zones and the dose carry the color. The numeral stays neutral until the red zone: 85 dB(A) is a matter of
    /// hours, not an alarm, and an amber numeral of this size would outshout the loudness numerals.
    fn ear_numeral_color(base: &PanelBase, db: i32) -> Color {
        if db == EarShown::NONE {
            base.palette.text_dim
        } else if db >= 100 {
            base.palette.danger
        } else {
            base.palette.text
        }
    }

    // MARK: Layout of the column

    pub(super) fn plan_ear_column(&self) -> EarPlan {
        let mut plan = EarPlan {
            content_x: self.ear_bar.max_x() + 18.0,
            numeral_size: if self.big_font_size >= 36.0 {
                30.0
            } else if self.big_font_size >= 28.0 {
                24.0
            } else {
                20.0
            },
            ..EarPlan::default()
        };
        let font = fonts::ui(plan.numeral_size, Weight::Light);
        plan.caption_y = self.ear.min_y() + 48.0;
        plan.numeral_base = plan.caption_y + 17.0 + font.cap_height() + 4.0;
        plan.y85 = y_for(85.0, EAR_MIN, EAR_MAX, self.ear_bar);
        let label_top = plan.y85 - 17.0;
        let mut y = plan.numeral_base + 14.0;
        let bottom = self.ear_bar.max_y();
        // Dose: at the foot of the column. It gives way only to the numeral.
        if bottom - EarPlan::DOSE_HEIGHT >= y + 6.0 {
            plan.dose_top = Some(bottom - EarPlan::DOSE_HEIGHT);
        }
        let floor = plan.dose_top.map_or(bottom, |top| top - 8.0);
        plan.shows_label85 = label_top >= y && plan.y85 + 4.0 <= floor;
        // The level table: one block under the numeral. A row that would touch the 85 line or its label moves under
        // the line.
        while plan.rows.len() < 4 {
            let clear = if plan.shows_label85 {
                label_top - 3.0
            } else {
                plan.y85 - 3.0
            };
            if y < plan.y85 + 6.0 && y + EarPlan::ROW_HEIGHT > clear {
                y = plan.y85 + 7.0;
            }
            if !(y + EarPlan::ROW_HEIGHT <= floor) {
                break;
            }
            plan.rows.push(y);
            y += EarPlan::ROW_HEIGHT;
        }
        plan
    }

    pub fn plan_ear_strip(&self, o: &OverlayContext) -> StripPlan {
        let ear = self.ear;
        let mut plan = StripPlan::default();
        let (x0, x1) = (ear.min_x() + 10.0, ear.max_x() - 10.0);
        plan.line_y = ear.min_y() + 27.0;
        let w_approx = o.measure(APPROX, &strip_approx_font(), 0.0) + 3.0;
        plan.numeral_right = x0 + w_approx + o.measure("000", &strip_numeral_font(), 0.0);
        let x = plan.numeral_right + 4.0 + o.measure("dB(A)", &strip_unit_font(), 0.0);
        let cap = self.cap_font();
        let w_dose = o.measure("DOSE", &cap, 0.6) + 6.0 + w_approx + o.measure("100 %", &strip_dose_font(), 0.0);
        // Order: numeral, bar, dose, time left (what the dose means at this level). The bar comes before the time left.
        let w_time =
            o.measure("00:00", &strip_dose_font(), 0.0) + 4.0 + o.measure("h left", &strip_unit_font(), 0.0) + 14.0;
        let mut room = x1 - w_dose - 16.0 - (x + 16.0);
        if room >= 96.0 + w_time || (room < 96.0 && room >= w_time) {
            plan.time_right = Some(x1);
            room -= w_time;
        }
        plan.dose_right = if plan.time_right.is_none() { x1 } else { x1 - w_time };
        if room >= 96.0 {
            let w = room.min(260.0);
            plan.bar = Rect::new(
                (x + 16.0 + (room - w) / 2.0).round(),
                ear.min_y() + 19.0,
                w.round(),
                5.0,
            );
        }
        plan
    }

    /// The strip's bar, cached per layout (the plan needs text widths).
    pub fn strip_bar(&mut self, base: &PanelBase) -> Rect {
        if self.ear_mode != EarMode::Strip {
            return Rect::default();
        }
        if self.strip_bar_cache_key != Some(self.ear) {
            let o = OverlayContext::new(base.width, base.height, base.scale);
            self.strip_bar_cache = self.plan_ear_strip(&o).bar;
            self.strip_bar_cache_key = Some(self.ear);
        }
        self.strip_bar_cache
    }

    // MARK: Shapes

    pub(super) fn draw_ear(&mut self, base: &mut PanelBase, s: &SplReading) {
        let p = base.palette;
        let silent = base.frame.as_ref().is_some_and(|f| f.is_silent);
        let fast = if silent { 0.0 } else { s.level_a_fast };
        let (ear, read) = (self.ear, self.read);
        match self.ear_mode {
            EarMode::Column => {
                let b = self.ear_bar;
                base.vline(ear.min_x() - 7.0, ear.min_y() + 4.0, ear.max_y() - 4.0, p.grid_major);
                let (x, w) = (b.min_x(), b.w);
                track(base, x, w, b);
                zoned_bar(base, x, w, b, fast, EAR_MIN, EAR_MAX, &Self::ear_stops(base));
                // The zones, always readable beside the bar.
                for i in 0..4 {
                    let y1 = y_for(ZONE_EDGES[i], EAR_MIN, EAR_MAX, b);
                    let y0 = y_for(ZONE_EDGES[i + 1], EAR_MIN, EAR_MAX, b);
                    let color = scaled_alpha(
                        Self::ear_zone_color(base, ZONE_EDGES[i]),
                        if i == 0 { 0.55 } else { 0.75 },
                    );
                    base.rounded_rect([x + w + 3.0, y0 + 0.5, 3.0, y1 - y0 - 1.0], color, 1.0);
                }
                for v in [55.0, 70.0, 100.0] {
                    base.hline(x - 3.0, x + w, y_for(v, EAR_MIN, EAR_MAX, b), p.grid_major);
                }
                // The 85 line runs through the column: the level over it, the exposure under it.
                let y85 = self.ear_plan.y85;
                let alpha = if self.ear_plan.shows_label85 { 0.55 } else { 0.35 };
                base.hline(x - 3.0, ear.max_x(), y85, with_alpha(p.warn, alpha));
                if fast > EAR_MIN {
                    self.caps.push(Cap {
                        x,
                        y: y_for(fast, EAR_MIN, EAR_MAX, b),
                        w,
                        color: mix(Self::ear_zone_color(base, fast), WHITE, 0.3),
                    });
                }
                if let Some(top) = self.ear_plan.dose_top {
                    base.hline(self.ear_plan.content_x, ear.max_x(), top - 1.0, p.grid_major);
                }
                if s.max_a_fast > EAR_MIN {
                    let y = y_for(s.max_a_fast, EAR_MIN, EAR_MAX, b);
                    base.batch
                        .hline(&mut base.arena, x, x + w, y, with_alpha(p.text, 0.8), 2.0);
                }
                // Pointer of the slow level = the big numeral.
                if !silent && s.level_a_slow > EAR_MIN {
                    let y = y_for(s.level_a_slow.min(EAR_MAX), EAR_MIN, EAR_MAX, b);
                    base.rounded_rect([x + w + 8.0, y - 1.0, 7.0, 2.0], p.text, 1.0);
                }
            }
            EarMode::Strip => {
                base.rounded_rect([ear.x, ear.y, ear.w, ear.h], p.plot, 5.0);
                base.batch.gradient_rect(
                    &mut base.arena,
                    [ear.x, ear.y, ear.w, ear.h],
                    p.grid_minor,
                    p.grid_minor,
                    5.0,
                    0.0,
                    1.0,
                );
                let b = self.strip_bar(base);
                if !(b.w > 0.0) {
                    return;
                }
                let (bx, by, bw, bh) = (b.x, b.y, b.w, b.h);
                let x_for = |v: f32| bx + ((v - EAR_MIN) / (EAR_MAX - EAR_MIN)).max(0.0).min(1.0) * bw;
                base.rounded_rect([bx, by, bw, bh], p.track, 1.5);
                for i in 0..4 {
                    let (a, e) = (x_for(ZONE_EDGES[i]), x_for(ZONE_EDGES[i + 1]));
                    let c = Self::ear_zone_color(base, ZONE_EDGES[i]);
                    // Zone underlay, then the part the fast level covers.
                    let shift = if i == 0 { 0.0 } else { 0.5 };
                    base.rect([a + shift, by + bh + 1.5, e - a - shift, 1.5], scaled_alpha(c, 0.7));
                    let end = x_for(fast).min(e);
                    if end > a {
                        base.rounded_rect([a, by, end - a, bh], c, if i == 0 { 1.5 } else { 0.0 });
                    }
                }
                // The 85 line points down at its label; the maximum stands over the bar. The two never read as one.
                base.vline(x_for(85.0), by, by + bh + 4.0, p.warn);
                if s.max_a_fast > EAR_MIN {
                    let x = x_for(s.max_a_fast);
                    base.batch
                        .vline(&mut base.arena, x, by - 3.0, by + bh, with_alpha(p.text, 0.85), 2.0);
                }
            }
            EarMode::Row => {
                // Hairline over the ear row of the readout column: under it stands an estimate, over it measurements.
                let y = read.min_y() + read.h * 3.0 / 4.0;
                base.hline(read.min_x(), read.max_x(), y, p.grid_major);
            }
            EarMode::None => {}
        }
    }

    // MARK: Text

    pub(super) fn draw_ear_static(&self, base: &PanelBase, o: &mut OverlayContext) {
        let p = &base.palette;
        let cap = self.cap_font();
        let small = fonts::mono(if self.compact { 10.0 } else { 11.0 }, Weight::Regular);
        let (ear, bar) = (self.ear, self.ear_bar);
        let warn = mix(p.warn, p.text_dim, 0.35);
        match self.ear_mode {
            EarMode::Column => {
                let (x, y) = (ear.min_x(), ear.min_y());
                o.text_tracked(
                    "LEVEL AT THE EAR",
                    x,
                    y + 4.0,
                    &cap,
                    p.text_faint,
                    HAlign::Left,
                    VAlign::Top,
                    1.0,
                );
                o.text(
                    "estimate \u{00B7} dB(A)",
                    x,
                    y + 20.0,
                    &small,
                    p.text_faint,
                    HAlign::Left,
                    VAlign::Top,
                );
                let mut last_y = -100.0;
                for v in [110.0, 100.0, 85.0, 70.0, 55.0, 40.0] {
                    let y = y_for(v, EAR_MIN, EAR_MAX, bar);
                    if !(y - last_y > 13.0) {
                        continue;
                    }
                    let color = if v == 85.0 { warn } else { p.text_dim };
                    o.text(
                        &fmt::number(v, 0, false),
                        bar.min_x() - 7.0,
                        y,
                        &small,
                        color,
                        HAlign::Right,
                        VAlign::Middle,
                    );
                    last_y = y;
                }
                let label = fonts::ui(11.0, Weight::Medium);
                o.text(
                    "fast",
                    bar.mid_x(),
                    bar.max_y() + 7.0,
                    &label,
                    p.text,
                    HAlign::Center,
                    VAlign::Top,
                );
                if self.ear_plan.shows_label85 {
                    let y = self.ear_plan.y85 - 4.0;
                    o.text(LABEL_85, ear.max_x(), y, &small, warn, HAlign::Right, VAlign::Bottom);
                }
            }
            EarMode::Strip => {
                let b = self.plan_ear_strip(o).bar;
                if !(b.w > 0.0) {
                    return;
                }
                let f = fonts::mono(9.0, Weight::Regular);
                let y = b.max_y() + 5.0;
                let x85 = b.min_x() + (85.0 - 40.0) / 70.0 * b.w;
                o.text("40", b.min_x(), y, &f, p.text_faint, HAlign::Left, VAlign::Top);
                o.text("110", b.max_x(), y, &f, p.text_faint, HAlign::Right, VAlign::Top);
                let full = o.text_bounds(LABEL_85, x85, y, &f, HAlign::Center, VAlign::Top, 0.0);
                let text = if o.is_free(full, 5.0, &[]) { LABEL_85 } else { "85" };
                o.text(text, x85, y, &f, warn, HAlign::Center, VAlign::Top);
            }
            EarMode::Row | EarMode::None => {}
        }
    }

    /// The name of the calibration and the uncertainty, cut to `width` with an ellipsis in the name.
    fn provenance(o: &OverlayContext, e: &EarShown, font: &Font, width: f32, prefix: &str) -> String {
        let tail = format!(" \u{00B7} {}", e.uncertainty_text);
        let mut name = e.calibration_name.clone();
        if name.is_empty() {
            return format!("{prefix}{}", e.uncertainty_text);
        }
        let full = format!("{prefix}{name}{tail}");
        if o.measure(&full, font, 0.0) <= width {
            return full;
        }
        while name.chars().count() > 4 {
            name.pop();
            let t = format!("{prefix}{}\u{2026}{tail}", name.trim_matches(' '));
            if o.measure(&t, font, 0.0) <= width {
                return t;
            }
        }
        let short = format!("{prefix}{}", e.uncertainty_text);
        if o.measure(&short, font, 0.0) <= width {
            short
        } else {
            e.uncertainty_text.clone()
        }
    }

    pub(super) fn draw_ear_dynamic(&self, base: &PanelBase, o: &mut OverlayContext, e: &EarShown) {
        match self.ear_mode {
            EarMode::Column => self.draw_ear_column(base, o, e),
            EarMode::Strip => self.draw_ear_strip(base, o, e),
            EarMode::Row | EarMode::None => {} // the row is drawn with the readout column
        }
    }

    fn draw_ear_column(&self, base: &PanelBase, o: &mut OverlayContext, e: &EarShown) {
        let p = &base.palette;
        let plan = &self.ear_plan;
        let cap = self.cap_font();
        let (x, right) = (plan.content_x, self.ear.max_x());
        let unit_font = fonts::ui(11.0, Weight::Regular);
        let (left, baseline) = (HAlign::Left, VAlign::Baseline);

        // The numeral: slow level, whole dB, with its sign of an estimate and its uncertainty.
        o.text_tracked(
            "SLOW \u{00B7} 1 s",
            x,
            plan.caption_y,
            &cap,
            p.text_faint,
            left,
            VAlign::Top,
            1.0,
        );
        let big = fonts::ui(plan.numeral_size, Weight::Light);
        let approx_font = fonts::ui((plan.numeral_size * 0.62).round(), Weight::Light);
        let w_approx = o.measure(APPROX, &approx_font, 0.0) + 5.0;
        let num_w = o.measure("000", &big, 0.0);
        let base_y = plan.numeral_base;
        if e.slow == EarShown::NONE {
            o.text(
                fmt::DASH,
                x,
                base_y,
                &fonts::ui(15.0, Weight::Regular),
                p.text_dim,
                left,
                baseline,
            );
        } else {
            // The number keeps its right edge (the unit never moves); the sign stands directly before it.
            let color = Self::ear_numeral_color(base, e.slow);
            let w = o.text(
                &e.slow.to_string(),
                x + w_approx + num_w,
                base_y,
                &big,
                color,
                HAlign::Right,
                baseline,
            );
            let ax = x + w_approx + num_w - w - 5.0;
            o.text(
                APPROX,
                ax,
                base_y - 1.0,
                &approx_font,
                p.text_dim,
                HAlign::Right,
                baseline,
            );
        }
        let ux = x + w_approx + num_w + 7.0;
        let w_unit = o.text("dB(A)", ux, base_y, &unit_font, p.text_dim, left, baseline);
        let mono = fonts::mono(11.0, Weight::Regular);
        if big.cap_height() >= 20.0 {
            // Over the unit, like the target over the LU delta.
            let y = base_y - big.cap_height() + 1.0;
            o.text(&e.uncertainty_text, ux, y, &mono, p.text_dim, left, VAlign::Top);
        } else {
            // Small numerals: no room for two lines beside them.
            o.text(
                &e.uncertainty_text,
                ux + w_unit + 8.0,
                base_y,
                &mono,
                p.text_dim,
                left,
                baseline,
            );
        }

        // Quiet rows: label left, whole dB right. They stand under "estimate · dB(A)".
        let row_font = fonts::ui(13.0, Weight::Regular);
        // Reading order: fast, max, Leq track, Leq session. When not all fit, the averages stay: the bar shows the
        // fast level and its tick the maximum.
        let table = [
            ("FAST", e.fast, 3),
            ("MAX FAST", e.max_fast, 2),
            ("LEQ TRACK", e.leq_track, 0),
            ("LEQ SESSION", e.leq_session, 1),
        ];
        let shown = table.iter().filter(|(_, _, priority)| *priority < plan.rows.len());
        for ((label, value, _), top) in shown.zip(&plan.rows) {
            let mid = top + EarPlan::ROW_HEIGHT / 2.0;
            o.text_tracked(label, x, mid, &cap, p.text_faint, left, VAlign::Middle, 0.6);
            let t = EarShown::level_text(*value);
            let (font, color) = if *value == EarShown::NONE {
                (fonts::ui(12.0, Weight::Regular), p.text_dim)
            } else {
                (row_font.clone(), p.text)
            };
            o.text(&t, right, mid, &font, color, HAlign::Right, VAlign::Middle);
        }

        // Dose: ring with the percent, time left at this level.
        if let Some(top) = plan.dose_top {
            let (percent, minutes_left) = self.dose_value();
            o.text_tracked(
                self.dose_caption(),
                x,
                top + 7.0,
                &cap,
                p.text_faint,
                left,
                VAlign::Top,
                0.6,
            );
            let r = EarPlan::RING_RADIUS;
            let (cx, cy) = (x + r + 3.0, top + 26.0 + r + 2.0);
            let color = Self::dose_color(base, percent);
            let turn = 2.0 * std::f32::consts::PI;
            o.arc([cx, cy], r, 0.0, turn - 0.0001, p.track, 5.0);
            let share = (percent as f32 / 100.0).min(1.0);
            if share > 0.0 {
                o.arc([cx, cy], r, 0.0, (share * turn - 0.0001).max(0.02), color, 5.0);
            }
            let pf = fonts::ui(if percent > 999 { 11.0 } else { 13.0 }, Weight::Medium);
            let text = if percent > 999 {
                "> 999 %".to_string()
            } else {
                format!("{percent} %")
            };
            let text_color = if percent >= 80 { color } else { p.text };
            o.text(&text, cx, cy, &pf, text_color, HAlign::Center, VAlign::Middle);
            let tx = cx + r + 14.0;
            o.text_tracked(
                "TIME LEFT",
                tx,
                cy - r + 1.0,
                &cap,
                p.text_faint,
                left,
                VAlign::Top,
                0.6,
            );
            let tf = fonts::ui(19.0, Weight::Regular);
            let t = EarShown::time_text(minutes_left);
            let t_base = cy - r + 17.0 + tf.cap_height() + 3.0;
            if t == fmt::DASH {
                o.text(
                    &t,
                    tx,
                    t_base,
                    &fonts::ui(15.0, Weight::Regular),
                    p.text_dim,
                    left,
                    baseline,
                );
            } else {
                let color = if minutes_left == 0 { p.danger } else { p.text };
                let w = o.text(&t, tx, t_base, &tf, color, left, baseline);
                if t.contains(':') {
                    o.text("h:mm", tx + w + 5.0, t_base, &unit_font, p.text_dim, left, baseline);
                }
            }
            o.text(
                "at this level",
                tx,
                t_base + 8.0,
                &unit_font,
                p.text_faint,
                left,
                VAlign::Top,
            );
        }

        // Provenance, on the row of the bar names.
        let pf = fonts::ui(11.0, Weight::Regular);
        let text = Self::provenance(o, e, &pf, right - x, "");
        o.text(
            &text,
            right,
            self.ear_bar.max_y() + 7.0,
            &pf,
            p.text_dim,
            HAlign::Right,
            VAlign::Top,
        );
    }

    fn draw_ear_strip(&self, base: &PanelBase, o: &mut OverlayContext, e: &EarShown) {
        let p = &base.palette;
        let plan = self.plan_ear_strip(o);
        let cap = self.cap_font();
        let ear = self.ear;
        let (x0, x1) = (ear.min_x() + 10.0, ear.max_x() - 10.0);
        let (left, baseline) = (HAlign::Left, VAlign::Baseline);
        // Line 1: what it is, and where the number comes from.
        let title = if ear.w >= 300.0 {
            "LEVEL AT THE EAR"
        } else {
            "AT THE EAR"
        };
        let w_title = o.text_tracked(title, x0, ear.min_y() + 6.0, &cap, p.text_faint, left, VAlign::Top, 1.0);
        let pf = fonts::ui(10.0, Weight::Regular);
        let room = x1 - x0 - w_title - 14.0;
        let mut prov = Self::provenance(o, e, &pf, room, "estimate \u{00B7} ");
        if o.measure(&prov, &pf, 0.0) > room {
            prov = "estimate".to_string();
        }
        o.text(
            &prov,
            x1,
            ear.min_y() + 6.0,
            &pf,
            p.text_dim,
            HAlign::Right,
            VAlign::Top,
        );

        // Line 2: the numeral, the bar (static labels), time left, dose.
        let base_y = ear.min_y() + 34.0;
        // Left edge on the left edge of the title. The unit follows the number (the plan keeps room for three digits).
        let nx = x0 + o.measure(APPROX, &strip_approx_font(), 0.0) + 3.0;
        let mut ux = nx + o.measure("00", &strip_numeral_font(), 0.0);
        if e.slow == EarShown::NONE {
            o.text(
                fmt::DASH,
                x0,
                base_y,
                &fonts::ui(12.0, Weight::Regular),
                p.text_dim,
                left,
                baseline,
            );
        } else {
            o.text(
                APPROX,
                x0,
                base_y - 1.0,
                &strip_approx_font(),
                p.text_dim,
                left,
                baseline,
            );
            let color = Self::ear_numeral_color(base, e.slow);
            ux = ux.max(
                nx + o.text(
                    &e.slow.to_string(),
                    nx,
                    base_y,
                    &strip_numeral_font(),
                    color,
                    left,
                    baseline,
                ),
            );
        }
        o.text(
            "dB(A)",
            ux + 4.0,
            base_y,
            &strip_unit_font(),
            p.text_dim,
            left,
            baseline,
        );

        let (percent, minutes_left) = self.dose_value();
        let color = Self::dose_color(base, percent);
        let mut x = plan.dose_right;
        let text = if percent > 999 {
            "> 999 %".to_string()
        } else {
            format!("{percent} %")
        };
        let text_color = if percent >= 80 { color } else { p.text };
        x -= o.text(
            &text,
            x,
            base_y,
            &strip_dose_font(),
            text_color,
            HAlign::Right,
            baseline,
        );
        x -= o.text(
            APPROX,
            x - 3.0,
            base_y,
            &strip_approx_font(),
            p.text_dim,
            HAlign::Right,
            baseline,
        ) + 3.0;
        o.text_tracked(
            "DOSE",
            x - 6.0,
            base_y,
            &cap,
            p.text_faint,
            HAlign::Right,
            baseline,
            0.6,
        );
        if let Some(tr) = plan.time_right {
            let mut tx = tr;
            // "h left": without the unit, 23:00 reads as a time of day.
            let t = EarShown::time_text(minutes_left);
            let unit = if t.contains(':') { "h left" } else { "left" };
            tx -= o.text(
                unit,
                tx,
                base_y,
                &strip_unit_font(),
                p.text_dim,
                HAlign::Right,
                baseline,
            ) + 4.0;
            let (font, color) = if t == fmt::DASH {
                (fonts::ui(12.0, Weight::Regular), p.text_dim)
            } else {
                (strip_dose_font(), p.text)
            };
            o.text(&t, tx, base_y, &font, color, HAlign::Right, baseline);
        }
    }

    /// The ear estimate as rows of the compact readout column (no band energy, so no strip). `y` = top of the row.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn draw_ear_row(
        &self,
        base: &PanelBase,
        o: &mut OverlayContext,
        e: &EarShown,
        x: f32,
        y: f32,
        width: f32,
        row_height: f32,
        font: &Font,
        with_dose: bool,
    ) {
        let p = &base.palette;
        let cap = self.cap_font();
        let (left, baseline) = (HAlign::Left, VAlign::Baseline);
        let w_cap = o.text_tracked("EAR", x, y + 4.0, &cap, p.text_faint, left, VAlign::Top, 1.0);
        let unit = fonts::ui(10.0, Weight::Regular);
        if w_cap + 6.0 + o.measure("dB(A)", &unit, 0.0) <= width {
            o.text(
                "dB(A)",
                x + w_cap + 6.0,
                y + 4.0,
                &unit,
                p.text_faint,
                left,
                VAlign::Top,
            );
        }
        let base_y = y + 18.0 + font.cap_height();
        let approx_font = fonts::ui(12.0, Weight::Regular);
        let t = EarShown::level_text(e.slow);
        // Left edge on the left edge of M / S / I and of the dose line.
        let w_approx = o.measure(APPROX, &approx_font, 0.0) + 4.0;
        let right = x + w_approx + o.measure("000", font, 0.0);
        if t == fmt::DASH {
            o.text(
                &t,
                x,
                base_y,
                &fonts::ui(12.0, Weight::Regular),
                p.text_dim,
                left,
                baseline,
            );
        } else {
            o.text(APPROX, x, base_y - 1.0, &approx_font, p.text_dim, left, baseline);
            o.text(
                &t,
                x + w_approx,
                base_y,
                font,
                Self::ear_numeral_color(base, e.slow),
                left,
                baseline,
            );
        }
        if !with_dose {
            return;
        }
        let (percent, _) = self.dose_value();
        let text = format!("{APPROX} {} %", percent.min(999));
        let df = fonts::ui(11.0, Weight::Regular);
        let color = if percent >= 80 {
            Self::dose_color(base, percent)
        } else {
            p.text_dim
        };
        if row_height >= 46.0 {
            // Second line: the dose.
            let w = o.text("dose", x, base_y + 14.0, &df, p.text_faint, left, baseline);
            o.text(&text, x + w + 4.0, base_y + 14.0, &df, color, left, baseline);
        } else if right + 10.0 + o.measure(&text, &df, 0.0) <= x + width {
            o.text(&text, x + width, base_y, &df, color, HAlign::Right, baseline);
        }
    }

    // MARK: Accessibility

    pub fn ear_accessibility_text(&self, s: &SplReading, silent: bool) -> String {
        let e = EarShown::new(s, silent, self.dose_ledger);
        let percent = match self.dose_standard {
            DoseStandard::NioshDaily => e.dose_niosh,
            DoseStandard::WhoWeekly => e.dose_who,
        };
        let level = if e.slow == EarShown::NONE {
            "no level at the ear".to_string()
        } else {
            format!("about {} dB A at the ear", e.slow)
        };
        format!("{level}, dose {percent} percent")
    }
}
