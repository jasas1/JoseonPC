//! The text of the spectrum panel: axis labels, the header row, the legends,
//! the note names, the cursor readout and the screen-reader text.
//!
//! Port source: the "Text", "Hover" and "Accessibility" parts of
//! `Sources/JoseonRender/SpectrumRenderer.swift`.

use super::{is_same_peak, CursorValues, PeakLabelPlacement, Slot, SpectrumPanel};
use crate::axis::LogAxis;
use crate::contracts::LevelAxis;
use crate::cursor::{self, header, CursorReadoutItem, Tone};
use crate::fmt;
use crate::gpu::Color;
use crate::palette::{mix, rgba, scaled_alpha, with_alpha, Palette, WHITE};
use crate::panel::{Panel, PanelBase};
use crate::text::fonts::{self, Weight};
use crate::text::{HAlign, OverlayContext, Rect, VAlign};
use joseon_core::contracts::{AnalysisFrame, HeadphoneReading, SplReading};

/// The note that says the headphone band is not drawn, long and short.
const BAND_HIDDEN: [&str; 2] = ["headphone band hidden", "hp band hidden"];

fn whole_hz(hz: f32) -> String {
    if hz >= 10_000.0 {
        format!("{:.1} kHz", (hz / 1000.0) as f64)
    } else {
        format!("{} Hz", hz.round() as i64)
    }
}

/// A line of `length` points as the swatch of a legend entry.
fn stroke(color: Color, dash: &'static [f32], width: f32) -> impl Fn(&mut OverlayContext, f32, f32) {
    move |o, x, y| o.line([x, y], [x + 16.0, y], color, width, dash)
}

impl SpectrumPanel {
    pub(super) fn draw_static_labels(&mut self, base: &PanelBase, o: &mut OverlayContext) {
        let p = base.palette;
        let compact = Self::compact(base);
        let small = Self::axis_font(base);
        let plot = self.plot;
        // Frequency labels. One that would touch the label before it is left out.
        let mut last_right = -100f32;
        for f in LogAxis::LABELED {
            if !(f >= self.axis.min_hz && f <= self.axis.max_hz) {
                continue;
            }
            let label = fmt::axis_hz(f);
            let w = o.measure(&label, &small, 0.0);
            // The end labels stay inside the plot width.
            let cx = self.x_for(f).max(plot.min_x() + w / 2.0).min(plot.max_x() - w / 2.0);
            if !(cx - w / 2.0 > last_right + 6.0) {
                continue;
            }
            o.text(
                &label,
                cx,
                self.axis_bottom() + 7.0,
                &small,
                p.text_dim,
                HAlign::Center,
                VAlign::Top,
            );
            last_right = cx + w / 2.0;
        }
        if !compact {
            o.text(
                "Hz",
                plot.min_x() - 7.0,
                self.axis_bottom() + 7.0,
                &small,
                p.text_faint,
                HAlign::Right,
                VAlign::Top,
            );
        }

        let step = self.db_step(base);
        let lo = self.lo();
        let mut db = (self.music_top() / step).floor() * step;
        while db >= lo - 0.01 {
            let yy = self.y_for(db);
            if yy > plot.min_y() + 3.0 && (yy < plot.max_y() - 3.0 || db <= lo + 0.01) {
                o.text(
                    &fmt::number(db, 0, false),
                    plot.min_x() - 7.0,
                    yy.min(plot.max_y() - 4.0),
                    &small,
                    p.text_dim,
                    HAlign::Right,
                    VAlign::Middle,
                );
            }
            db -= step;
        }
        // With the headphone band the music scale starts under the band: its unit stands there.
        let unit_y = if self.hp_shown {
            self.y_for(self.music_top()) - 18.0
        } else {
            plot.min_y() + 1.0
        };
        o.text(
            if self.spl_shown { "dBFS" } else { "dB" },
            plot.min_x() - 7.0,
            unit_y,
            &small,
            p.text_faint,
            HAlign::Right,
            VAlign::Top,
        );
        if self.spl_shown {
            // Right-hand axis of the band layer, in its color. The unit stands on the row of the Hz labels.
            let col = with_alpha(p.spl_band, 1.0);
            for v in self.spl_ticks(base) {
                o.text(
                    &fmt::number(v, 0, false),
                    plot.max_x() + 5.0,
                    self.y_for(v - self.spl_offset),
                    &small,
                    col,
                    HAlign::Left,
                    VAlign::Middle,
                );
            }
            if !compact {
                o.text(
                    "dB SPL",
                    base.width - 3.0,
                    self.spl_unit_y(),
                    &small,
                    col,
                    HAlign::Right,
                    VAlign::Top,
                );
            }
        }
    }

    pub(super) fn draw_dynamic_labels(&mut self, base: &mut PanelBase, o: &mut OverlayContext) {
        let Some(f) = base.frame.clone() else { return };
        let p = base.palette;
        self.draw_header(base, o, &f);
        if let (true, Some(hp)) = (self.shows_headphone_legend(base), &f.headphone) {
            self.draw_headphone_legend(base, o, hp);
        }
        if self.hp_shown && !Self::compact(base) {
            // Right-hand axis of the headphone curves: dB relative to 1 kHz, in the response color.
            let font = Self::axis_font(base);
            let col = p.hp_response;
            for d in self.hp_ticks() {
                let label = if d == 0.0 {
                    "0".to_string()
                } else {
                    fmt::number(d, 0, true)
                };
                o.text(
                    &label,
                    self.plot.max_x() + 5.0,
                    self.hp_y(d),
                    &font,
                    col,
                    HAlign::Left,
                    VAlign::Middle,
                );
            }
            // The axis name stands over the axis, at the right end of the headphone legend row.
            o.text(
                "dB rel",
                base.width - 3.0,
                self.plot.min_y() - 4.0,
                &font,
                col,
                HAlign::Right,
                VAlign::Bottom,
            );
        }
        self.draw_stress_labels(base, o, &f);
        self.draw_peak_labels(base, o, &f);
    }

    fn header_mid_y(base: &PanelBase) -> f32 {
        let top = if Self::compact(base) { 3.0 } else { 7.0 };
        top + Self::header_height(base) / 2.0
    }

    fn legend_font(base: &PanelBase, bold: bool) -> fonts::Font {
        fonts::ui(
            if Self::compact(base) { 10.0 } else { 11.0 },
            if bold { Weight::Semibold } else { Weight::Regular },
        )
    }

    fn legend_gap(base: &PanelBase) -> f32 {
        if Self::compact(base) {
            9.0
        } else {
            14.0
        }
    }

    /// Room the row keeps for the first legend entry.
    fn minimum_legend_width(&self, base: &PanelBase, o: &OverlayContext) -> f32 {
        if self.options.show_mid {
            21.0 + o.measure("Mid", &Self::legend_font(base, false), 0.0) + Self::legend_gap(base)
        } else {
            0.0
        }
    }

    fn note_width(base: &PanelBase, o: &OverlayContext, text: &str) -> f32 {
        o.measure(text, &Self::legend_font(base, false), 0.0) + Self::legend_gap(base)
    }

    /// The row over the plot, at every size: legend on the left; on the right
    /// the lowest strong content and the peak readout. Priority when the row is
    /// short: peak, lowest strong content, then as many legend entries as fit.
    /// With no peak the readout takes no room at all.
    fn draw_header(&mut self, base: &mut PanelBase, o: &mut OverlayContext, f: &AnalysisFrame) {
        let p = base.palette;
        let compact = Self::compact(base);
        let mid_y = Self::header_mid_y(base);
        let cap = Self::caption_font(base);
        let plot = self.plot;
        let mut right = base.width - 6.0;

        let has = !fmt::is_floor(f.peak.level_db) && f.peak.frequency_hz > 0.0;
        // The analyzer names a note only for a tonal peak. No name = broad content: frequency and level, no note, no
        // cents. (Nothing is made up here from the frequency.)
        let note = fmt::pretty_note(&f.peak.note_name);
        let header_readout = self.shows_header_readout(base);
        if header_readout {
            // The cursor asks the question now: its answer takes the place of the peak readout and of the lowest
            // strong content. The legend keeps at least its first entry, and a third of the row when the row is long.
            let mut keep = (if compact { 40.0 } else { 56.0f32 }).max((right - plot.min_x()) * 0.32);
            if self.hp_hidden {
                keep = keep.max(self.minimum_legend_width(base, o) + Self::note_width(base, o, BAND_HIDDEN[1]));
            }
            let items = self.readout_items(base);
            right = header::draw(
                o,
                &items,
                right,
                plot.min_x() + 2.0 + keep,
                mid_y,
                compact,
                base.cursor.is_some_and(|c| c.is_pinned),
                &p,
            ) - 20.0;
        } else if has && self.header_has_room_for_peak(base, o, right) {
            // The large view gives the number a little more weight: it is the headline of the panel.
            let large = base.width >= 1200.0;
            let num = fonts::ui(
                if compact {
                    11.0
                } else if large {
                    15.0
                } else {
                    13.0
                },
                Weight::Medium,
            );
            let small = fonts::ui(
                if compact {
                    10.0
                } else if large {
                    13.0
                } else {
                    12.0
                },
                Weight::Regular,
            );
            let w_db = o.measure("\u{2212}00.0 dB", &small, 0.0);
            let w_cents = o.measure("+50 \u{00A2}", &small, 0.0);
            let w_note = o.measure("G\u{266F}8", &small, 0.0);
            let w_hz = o.measure("00000.0 Hz", &num, 0.0);
            let narrow = base.width < 640.0;
            let gap = if compact { 5.0 } else { 8.0 };
            let note_room = w_db
                + w_note
                + w_hz
                + 7.0
                + if compact { 10.0 } else { 16.0 }
                + if narrow { 0.0 } else { w_cents + 6.0 };
            let show_note =
                !note.is_empty() && right - note_room - (plot.min_x() + 2.0) >= self.header_reserve(base, o);
            o.text(
                &format!("{} dB", fmt::db(f.peak.level_db, 1, false)),
                right,
                mid_y,
                &small,
                p.text,
                HAlign::Right,
                VAlign::Middle,
            );
            right -= w_db + gap;
            if show_note {
                if !narrow {
                    o.text(
                        &fmt::cents(f.peak.cents),
                        right,
                        mid_y,
                        &small,
                        p.text_dim,
                        HAlign::Right,
                        VAlign::Middle,
                    );
                    right -= w_cents + 6.0;
                }
                let hue = rgba(Palette::spectrum_color(f.peak.frequency_hz), 1.0);
                o.text(
                    &note,
                    right - w_note,
                    mid_y,
                    &small,
                    mix(hue, WHITE, 0.25),
                    HAlign::Left,
                    VAlign::Middle,
                );
                right -= w_note + gap;
            }
            o.text(
                &fmt::hz(f.peak.frequency_hz),
                right,
                mid_y,
                &num,
                p.text,
                HAlign::Right,
                VAlign::Middle,
            );
            right -= w_hz + 7.0;
            if base.width >= 560.0 {
                right -= o.text_tracked(
                    "PEAK",
                    right,
                    mid_y,
                    &cap,
                    p.text_faint,
                    HAlign::Right,
                    VAlign::Middle,
                    1.0,
                ) + 16.0;
            }
        }
        // The legend comes first in a short row: under 640 pt the lowest strong content is left to the large view.
        if f.lowest_strong_hz > 0.0 && base.width >= 640.0 && !header_readout {
            // Whole Hz (the measure is not finer), in the neutral text color: a fact about the music, not an alarm.
            let value = whole_hz(f.lowest_strong_hz);
            let label = if base.width >= 900.0 {
                "lowest strong content:"
            } else {
                "lowest"
            };
            let (vf, lf) = (fonts::ui(12.0, Weight::Medium), fonts::ui(11.0, Weight::Regular));
            let need = o.measure(&value, &vf, 0.0) + 5.0 + o.measure(label, &lf, 0.0);
            if right - need > plot.min_x() + 90.0 {
                right -= o.text(&value, right, mid_y, &vf, p.text, HAlign::Right, VAlign::Middle) + 5.0;
                right -= o.text(label, right, mid_y, &lf, p.text_dim, HAlign::Right, VAlign::Middle) + 16.0;
            }
        }
        // The SPL layer names itself in the legend row, after the curves; its place is kept free first.
        let note_font = Self::legend_font(base, false);
        let mut spl_label = "";
        if self.level_axis == LevelAxis::DbSpl {
            let room = right - (plot.min_x() + 2.0);
            let names: &[&str] = if self.spl_shown {
                &Self::SPL_LEGENDS
            } else {
                &Self::NOT_CALIBRATED_NOTES
            };
            let swatch = if self.spl_shown { 21.0 } else { 0.0 };
            // A longer name only when the first curve of the legend still fits beside it; the shortest name always.
            spl_label = names
                .iter()
                .find(|name| o.measure(name, &note_font, 0.0) + swatch + 60.0 <= room)
                .unwrap_or(&names[names.len() - 1]);
            if !spl_label.is_empty() {
                right -= o.measure(spl_label, &note_font, 0.0) + swatch + 14.0;
            }
        }
        // The headphone band has no room: the header says so, before any legend entry past the first.
        let mut hidden_note = "";
        if self.hp_hidden {
            let room = right - (plot.min_x() + 2.0) - self.minimum_legend_width(base, o);
            hidden_note = BAND_HIDDEN
                .into_iter()
                .find(|text| Self::note_width(base, o, text) <= room)
                .unwrap_or("");
            if !hidden_note.is_empty() {
                right -= Self::note_width(base, o, hidden_note);
            }
        }
        let mut x = self.draw_legend_row(base, o, mid_y, plot.min_x() + 2.0, right);
        let note_color = mix(p.warn, p.text_dim, 0.4);
        if !hidden_note.is_empty() {
            x += o.text(
                hidden_note,
                x,
                mid_y,
                &note_font,
                note_color,
                HAlign::Left,
                VAlign::Middle,
            ) + Self::legend_gap(base);
        }
        if spl_label.is_empty() {
            return;
        }
        let col = p.spl_band;
        if self.spl_shown {
            self.legend_item(base, o, spl_label, &mut x, mid_y, base.width, 16.0, false, |o, x, y| {
                // The swatch is two steps of the layer.
                o.fill_rect(Rect::new(x, y - 1.0, 8.0, 6.0), scaled_alpha(col, 0.30), 0.0);
                o.fill_rect(Rect::new(x + 8.0, y - 5.0, 8.0, 10.0), scaled_alpha(col, 0.30), 0.0);
                o.line([x, y - 1.0], [x + 8.0, y - 1.0], scaled_alpha(col, 0.9), 1.0, &[]);
                o.line(
                    [x + 8.0, y - 5.0],
                    [x + 16.0, y - 5.0],
                    scaled_alpha(col, 0.9),
                    1.0,
                    &[],
                );
            });
        } else {
            o.text(
                spl_label,
                x,
                mid_y,
                &note_font,
                note_color,
                HAlign::Left,
                VAlign::Middle,
            );
        }
    }

    /// A note that says what the panel does NOT show comes before the peak
    /// readout: in a card too narrow for both, the readout gives way.
    fn header_has_room_for_peak(&self, base: &PanelBase, o: &OverlayContext, right: f32) -> bool {
        let reserve = self.header_reserve(base, o);
        if !(reserve > 0.0 && self.hp_hidden) {
            return true;
        }
        let compact = Self::compact(base);
        let num = fonts::ui(if compact { 11.0 } else { 13.0 }, Weight::Medium);
        let small = fonts::ui(if compact { 10.0 } else { 12.0 }, Weight::Regular);
        let readout = o.measure("\u{2212}00.0 dB", &small, 0.0)
            + o.measure("00000.0 Hz", &num, 0.0)
            + if compact { 5.0 } else { 8.0 }
            + 7.0;
        right - readout - (self.plot.min_x() + 2.0) >= reserve
    }

    /// Room the row keeps on the left before the peak readout may take it: the
    /// first legend entry and the shortest form of the note that says what is
    /// hidden. Zero when there is nothing to keep.
    fn header_reserve(&self, base: &PanelBase, o: &OverlayContext) -> f32 {
        if self.hp_hidden {
            self.minimum_legend_width(base, o) + Self::note_width(base, o, BAND_HIDDEN[1])
        } else {
            0.0
        }
    }

    /// One legend entry. Gives false (and draws nothing) when it does not fit before `limit`.
    #[allow(clippy::too_many_arguments)]
    fn legend_item(
        &self,
        base: &PanelBase,
        o: &mut OverlayContext,
        label: &str,
        x: &mut f32,
        y: f32,
        limit: f32,
        swatch: f32,
        bold: bool,
        draw: impl FnOnce(&mut OverlayContext, f32, f32),
    ) -> bool {
        let font = Self::legend_font(base, bold);
        let w = if swatch > 0.0 { swatch + 5.0 } else { 0.0 } + o.measure(label, &font, 0.0);
        if !(*x + w <= limit) {
            return false;
        }
        if swatch > 0.0 {
            draw(o, *x, y);
            *x += swatch + 5.0;
        }
        let color = if bold { base.palette.text } else { base.palette.text_dim };
        *x += o.text(label, *x, y, &font, color, HAlign::Left, VAlign::Middle) + Self::legend_gap(base);
        true
    }

    /// Legend of the source curves, in the header. What does not fit is left
    /// out, from the end. Gives where the row ends.
    fn draw_legend_row(&mut self, base: &mut PanelBase, o: &mut OverlayContext, y: f32, from: f32, limit: f32) -> f32 {
        let mut x = from;
        let mut naming: Vec<Slot> = Vec::new();
        self.legend_body(base, o, y, limit, &mut x, &mut naming);
        // The GPU pass draws what this row named: a change of the row is a change of the picture.
        if naming != self.legend_named {
            self.legend_named = naming;
            base.needs_display = true;
        }
        x
    }

    fn legend_body(
        &self,
        base: &PanelBase,
        o: &mut OverlayContext,
        y: f32,
        limit: f32,
        x: &mut f32,
        naming: &mut Vec<Slot>,
    ) {
        let p = base.palette;
        if self.options.show_mid {
            let drawn = self.legend_item(base, o, "Mid", x, y, limit, 16.0, false, |o, x, y| {
                for (k, hz) in [60.0, 700.0, 9000.0].into_iter().enumerate() {
                    let x0 = x + k as f32 * 16.0 / 3.0;
                    o.line(
                        [x0, y],
                        [x0 + 16.0 / 3.0, y],
                        rgba(Palette::spectrum_color(hz), 1.0),
                        2.5,
                        &[],
                    );
                }
            });
            if !drawn {
                return;
            }
        }
        if let Some(slice) = self.ghost_slice(base) {
            // Only while the ghost trace shows, right after the curve whose past it is.
            let label = format!("then ({})", cursor::ago(slice.seconds_ago));
            if !self.legend_item(
                base,
                o,
                &label,
                x,
                y,
                limit,
                16.0,
                false,
                stroke(p.cursor_ghost, &[6.0, 4.0], 1.25),
            ) {
                return;
            }
        }
        if self.wants_left_right() {
            // L and R together or not at all: a legend that names only L would say there is no R curve.
            let font = Self::legend_font(base, false);
            let both =
                2.0 * (16.0 + 5.0) + o.measure("L", &font, 0.0) + o.measure("R", &font, 0.0) + Self::legend_gap(base);
            if !(*x + both <= limit) {
                return;
            }
            for (label, color) in [("L", p.spectrum_left), ("R", p.spectrum_right)] {
                if !self.legend_item(
                    base,
                    o,
                    label,
                    x,
                    y,
                    limit,
                    16.0,
                    false,
                    stroke(with_alpha(color, 1.0), &[], 1.5),
                ) {
                    return;
                }
            }
            naming.push(Slot::Left);
        }
        if self.options.show_side {
            if !self.legend_item(base, o, "Side", x, y, limit, 16.0, false, stroke(p.side, &[], 1.25)) {
                return;
            }
            naming.push(Slot::Side);
        }
        // The swatches are the lines of the plot: warm white for peak hold, solid cool grey for the average.
        if self.wants_peak_hold(base) {
            if !self.legend_item(
                base,
                o,
                "Peak hold",
                x,
                y,
                limit,
                16.0,
                false,
                stroke(p.peak_hold, &[], 1.0),
            ) {
                return;
            }
            naming.push(Slot::PeakHold);
        }
        if self.wants_long_term(base) {
            if !self.legend_item(
                base,
                o,
                Self::AVERAGE_LEGEND,
                x,
                y,
                limit,
                16.0,
                false,
                stroke(p.average, &[], 1.0),
            ) {
                return;
            }
            naming.push(Slot::Average);
        }
        // Mid is drawn as measured; L and R get about 1/6 octave of display smoothing above 2 kHz. Said only when it is so.
        if self.wants_left_right() && self.left_right_smoothed() {
            self.legend_item(
                base,
                o,
                Self::LEFT_RIGHT_SMOOTHING_HINT,
                x,
                y,
                limit,
                0.0,
                false,
                |_, _, _| {},
            );
        }
    }

    /// Legend of the headphone curves: the second header row, over the plot. No plate, nothing over the data.
    fn draw_headphone_legend(&self, base: &PanelBase, o: &mut OverlayContext, hp: &HeadphoneReading) {
        let p = base.palette;
        let y = Self::header_mid_y(base) + Self::header_height(base) / 2.0 + Self::HEADPHONE_ROW_HEIGHT / 2.0 - 1.0;
        let limit = base.width - 6.0 - o.measure("dB rel", &Self::axis_font(base), 0.0) - 10.0;
        let mut x = self.plot.min_x() + 2.0;
        if !self.legend_item(base, o, &hp.model_name, &mut x, y, limit, 0.0, true, |_, _, _| {}) {
            return;
        }
        if !self.legend_item(
            base,
            o,
            "Response",
            &mut x,
            y,
            limit,
            16.0,
            false,
            stroke(p.hp_response, &[], 1.5),
        ) {
            return;
        }
        if hp.has_target {
            if !self.legend_item(
                base,
                o,
                "Target",
                &mut x,
                y,
                limit,
                16.0,
                false,
                stroke(p.hp_target, &[1.8, 2.7], 1.8),
            ) {
                return;
            }
            let error = with_alpha(p.hp_response, 0.22);
            if !self.legend_item(base, o, "Error", &mut x, y, limit, 16.0, false, |o, x, y| {
                o.fill_rect(Rect::new(x, y - 4.0, 16.0, 8.0), error, 1.0);
            }) {
                return;
            }
        }
        if !self.at_ear_shown {
            return;
        }
        let label = if hp.has_target {
            Self::AT_EAR_VS_TARGET_LEGEND
        } else {
            Self::AT_EARDRUM_LEGEND
        };
        self.legend_item(
            base,
            o,
            label,
            &mut x,
            y,
            limit,
            16.0,
            false,
            stroke(with_alpha(p.hp_at_ear, 0.8), &[], 1.25),
        );
    }

    /// The plot label of each stress span, at the top of the span. A label
    /// moves down a row when its place is taken, and is left out when no row is
    /// free: never text on text.
    fn draw_stress_labels(&mut self, base: &PanelBase, o: &mut OverlayContext, f: &AnalysisFrame) {
        self.ensure_annotations(f);
        let font = fonts::ui(if Self::compact(base) { 10.0 } else { 11.0 }, Weight::Semibold);
        let plot = self.plot;
        let top = if self.hp_shown {
            self.hp_band().max_y()
        } else {
            plot.min_y()
        };
        for (flag, x0, x1) in &self.stress_span_cache {
            let text = if flag.plot_label.is_empty() {
                &flag.title
            } else {
                &flag.plot_label
            };
            let w = o.measure(text, &font, 0.0);
            if !(w + 8.0 < plot.w) {
                continue;
            }
            let cx = ((x0 + x1) / 2.0)
                .max(plot.min_x() + w / 2.0 + 5.0)
                .min(plot.max_x() - w / 2.0 - 5.0);
            let color = with_alpha(
                mix(Self::severity_color(&base.palette, flag.severity), WHITE, 0.30),
                1.0,
            );
            for row in 0..6 {
                let yy = top + 7.0 + row as f32 * 16.0;
                if !(yy + 14.0 < plot.max_y()) {
                    break;
                }
                let ink = o.text_bounds(text, cx, yy, &font, HAlign::Center, VAlign::Top, 0.0);
                if !o.is_free(ink, 3.0, &[]) {
                    continue;
                }
                o.fill_rect(ink.inset(-4.0, -2.5), with_alpha(base.palette.plot, 0.72), 4.0);
                o.text(text, cx, yy, &font, color, HAlign::Center, VAlign::Top);
                break;
            }
        }
    }

    /// Note names of the secondary peaks. A label is directly above its own
    /// dot. When that place is taken it may move a little (higher, or up and to
    /// one side) WITH a 1 px leader line to its dot, and never onto or past the
    /// frequency of another peak of the list (labelled or not). When no such
    /// place is free the label is left out: a missing name is better than a
    /// name over the wrong peak.
    fn draw_peak_labels(&mut self, base: &PanelBase, o: &mut OverlayContext, f: &AnalysisFrame) {
        self.ensure_annotations(f);
        let font = fonts::ui(10.0, Weight::Semibold);
        self.peak_label_placements.clear();
        let plot = self.plot;
        let (lo, hi) = self.shown_range();
        // Every peak of the list inside the plot, tonal or not, with or without a label: nothing goes on its marker.
        let mut dots: Vec<(f32, [f32; 2])> = Vec::new();
        for pk in &f.top_peaks {
            if !fmt::is_floor(pk.level_db) && pk.frequency_hz > self.axis.min_hz && pk.frequency_hz < self.axis.max_hz {
                dots.push((
                    pk.frequency_hz,
                    [self.x_for(pk.frequency_hz), self.y_for(pk.level_db.max(lo).min(hi))],
                ));
            }
        }
        if !fmt::is_floor(f.peak.level_db)
            && f.peak.frequency_hz > self.axis.min_hz
            && !dots.iter().any(|d| d.0 == f.peak.frequency_hz)
        {
            dots.push((
                f.peak.frequency_hz,
                [self.x_for(f.peak.frequency_hz), self.y_for(f.peak.level_db.min(hi))],
            ));
        }
        let peaks = self.secondary_peak_cache.clone();
        for pk in &peaks {
            let name = fmt::pretty_note(&pk.note_name);
            if name.is_empty() {
                continue;
            }
            let (mx, my) = (self.x_for(pk.frequency_hz), self.y_for(pk.level_db.min(hi)));
            let color = mix(rgba(Palette::spectrum_color(pk.frequency_hz), 1.0), WHITE, 0.55);
            let others: Vec<[f32; 2]> = dots.iter().filter(|d| d.0 != pk.frequency_hz).map(|d| d.1).collect();
            let taken: Vec<Rect> = others
                .iter()
                .map(|at| Rect::new(at[0] - 5.0, at[1] - 5.0, 10.0, 10.0))
                .collect();
            // (center x, bottom y, leader)
            let tries = [
                (mx, my - 9.0, false),
                (mx, my - 21.0, true),
                (mx + 7.0, my - 17.0, true),
                (mx - 7.0, my - 17.0, true),
                (mx + 10.0, my - 27.0, true),
                (mx - 10.0, my - 27.0, true),
            ];
            for (tx, ty, leader) in tries {
                let ink = o.text_bounds(&name, tx, ty, &font, HAlign::Center, VAlign::Bottom, 0.0);
                if !(ink.min_x() > plot.min_x() + 2.0
                    && ink.max_x() < plot.max_x() - 2.0
                    && ink.min_y() > plot.min_y() + 2.0
                    && o.is_free(ink, 2.0, &taken))
                {
                    continue;
                }
                // A label that moved stays on its own side of every other peak.
                let crosses = |at: &[f32; 2]| {
                    if at[0] > mx {
                        ink.max_x() >= at[0] - 1.0
                    } else {
                        ink.min_x() <= at[0] + 1.0
                    }
                };
                if leader && others.iter().any(crosses) {
                    continue;
                }
                if leader {
                    o.line(
                        [mx, my - 3.6],
                        [tx, ink.max_y() + 1.5],
                        with_alpha(color, 0.75),
                        1.0,
                        &[],
                    );
                }
                // A small plate: the label stands on peak hold and L / R lines.
                o.fill_rect(ink.inset(-2.5, -1.5), with_alpha(base.palette.plot, 0.62), 0.0);
                o.text(&name, tx, ty, &font, color, HAlign::Center, VAlign::Bottom);
                self.peak_label_placements.push(PeakLabelPlacement {
                    name: name.clone(),
                    dot: [mx, my],
                    ink,
                    leader,
                });
                break;
            }
        }
    }

    // MARK: Cursor

    /// The values of the newest frame at the cursor frequency.
    pub fn cursor_values(&self, base: &PanelBase) -> Option<CursorValues> {
        let (c, f) = (base.cursor?, base.frame.as_ref()?);
        let (s, hz) = (&f.spectrum, c.frequency_hz);
        let mut v = CursorValues {
            mid: cursor::level(&s.mid, &s.frequencies, hz),
            ..Default::default()
        };
        // Not tied to the legend: the width of the readout decides the room of the legend, so that would be a loop.
        if self.wants_left_right() {
            v.left = cursor::level(&s.left, &s.frequencies, hz);
            v.right = cursor::level(&s.right, &s.frequencies, hz);
        }
        if self.options.show_side {
            v.side = cursor::level(&s.side, &s.frequencies, hz);
        }
        if let (true, Some(hp), Some(m)) = (self.hp_shown, &f.headphone, v.mid) {
            if hp.has_target {
                let r = cursor::level(&hp.response_db, &s.frequencies, hz);
                let t = cursor::level(&hp.target_db, &s.frequencies, hz);
                if let (Some(r), Some(t)) = (r, t) {
                    // The "At ear vs target" curve at this frequency, before its display smoothing: Mid + response - target.
                    v.at_ear = Some(m + r - t);
                    v.at_ear_delta = Some(r - t);
                }
            }
        }
        if let (true, Some(e), Some(t)) = (self.spl_shown, &f.spl, &f.third_octave) {
            if let Some(b) = cursor::third_octave_band(&t.centers_hz, hz) {
                if b < e.band_levels_eardrum.len() && e.band_levels_eardrum[b] > SplReading::FLOOR_DB + 1.0 {
                    v.spl = Some((t.centers_hz[b], e.band_levels_eardrum[b]));
                }
            }
        }
        if let Some(slice) = self.ghost_slice(base) {
            v.then = cursor::level(&slice.mid_db, &slice.frequencies, hz);
        }
        Some(v)
    }

    pub(super) fn readout_items(&self, base: &PanelBase) -> Vec<CursorReadoutItem> {
        let (Some(c), Some(v)) = (base.cursor, self.cursor_values(base)) else {
            return Vec::new();
        };
        let db = |value: f32| fmt::db(value, 1, false);
        let mut items = vec![CursorReadoutItem::new(
            cursor::hz(c.frequency_hz),
            CursorReadoutItem::HZ,
            Tone::Value,
        )];
        if let Some(n) = cursor::note(c.frequency_hz) {
            items.push(CursorReadoutItem::new(n, CursorReadoutItem::NOTE, Tone::Dim));
        }
        if let Some(m) = v.mid {
            items.push(CursorReadoutItem::new(
                format!("Mid {} dB", db(m)),
                CursorReadoutItem::MID,
                Tone::Value,
            ));
        }
        if let (Some(l), Some(r)) = (v.left, v.right) {
            items.push(CursorReadoutItem::new(
                format!("L {}  R {}", db(l), db(r)),
                CursorReadoutItem::LEFT_RIGHT,
                Tone::Dim,
            ));
        }
        if let Some(side) = v.side {
            items.push(CursorReadoutItem::new(
                format!("Side {}", db(side)),
                CursorReadoutItem::SIDE,
                Tone::Dim,
            ));
        }
        if let Some(a) = v.at_ear_delta {
            items.push(CursorReadoutItem::new(
                format!("at ear vs target {} dB", fmt::number(a, 1, true)),
                CursorReadoutItem::AT_EAR,
                Tone::Dim,
            ));
        }
        if let Some((_, level)) = v.spl {
            items.push(CursorReadoutItem::new(
                format!("1/3 oct \u{2248}{} dB SPL", fmt::number(level, 0, false)),
                CursorReadoutItem::SPL,
                Tone::Dim,
            ));
        }
        if let (Some(slice), Some(t)) = (self.ghost_slice(base), v.then) {
            let level = if t <= Self::GHOST_FLOOR_DB {
                format!("under {}", fmt::number(Self::GHOST_FLOOR_DB, 0, false))
            } else {
                db(t)
            };
            items.push(CursorReadoutItem::new(
                format!("then {level} dB ({})", cursor::ago(slice.seconds_ago)),
                CursorReadoutItem::THEN,
                Tone::Ghost,
            ));
        }
        items
    }

    pub(super) fn hover_lines(&self, base: &PanelBase) -> Option<(Vec<String>, [f32; 2])> {
        let db = |value: f32| fmt::db(value, 1, false);
        let (lo, hi) = self.shown_range();
        let plot = self.plot;
        if base.cursor_linked {
            if !self.shows_pointer_readout(base) {
                return None;
            }
            let hv = base.hover.filter(|hv| self.pointer_in_plot(*hv))?;
            let (c, v) = (base.cursor?, self.cursor_values(base)?);
            let mut first = cursor::hz(c.frequency_hz);
            if let Some(n) = cursor::note(c.frequency_hz) {
                first += &format!("   {n}");
            }
            let mut lines = vec![first];
            if let Some(m) = v.mid {
                let mut l = format!("Mid  {} dB", db(m));
                if let Some(side) = v.side {
                    l += &format!("   Side  {} dB", db(side));
                }
                lines.push(l);
            }
            if let (Some(l), Some(r)) = (v.left, v.right) {
                lines.push(format!("L  {} dB   R  {} dB", db(l), db(r)));
            }
            if let (Some(a), Some(d)) = (v.at_ear, v.at_ear_delta) {
                lines.push(format!("at ear vs target  {} dB  ({})", db(a), fmt::number(d, 1, true)));
            }
            if let Some((center, level)) = v.spl {
                lines.push(format!(
                    "1/3 oct {} Hz  \u{2248}{} dB SPL at eardrum",
                    fmt::axis_hz(center),
                    fmt::number(level, 0, false)
                ));
            }
            let level = lo + (plot.max_y() - hv[1]) / plot.h * (hi - lo);
            lines.push(format!("pointer level  {} dB", fmt::number(level, 1, false)));
            return Some((lines, hv));
        }
        let hv = base.hover.filter(|hv| self.pointer_in_plot(*hv))?;
        let hz = self.axis.frequency((hv[0] - plot.min_x()) / plot.w);
        let level = lo + (plot.max_y() - hv[1]) / plot.h * (hi - lo);
        // Every number says what it is: the pointer position, or a measured curve under the pointer.
        let mut first = format!("cursor  {}", fmt::hz(hz));
        if let Some((name, cents)) = fmt::note(hz) {
            first += &format!("   {name} {}", fmt::cents(cents));
        }
        let mut lines = vec![first, format!("cursor  {} dB", fmt::number(level, 1, false))];
        if let Some(f) = &base.frame {
            let s = &f.spectrum;
            if let Some(m) = cursor::level_unchecked(&s.mid, &s.frequencies, hz) {
                let mut third = format!("Mid  {} dB", db(m));
                if let Some(side) = cursor::level_unchecked(&s.side, &s.frequencies, hz) {
                    third += &format!("   Side  {} dB", db(side));
                }
                lines.push(third);
            }
            if self.wants_left_right() {
                let l = cursor::level_unchecked(&s.left, &s.frequencies, hz);
                let r = cursor::level_unchecked(&s.right, &s.frequencies, hz);
                if let (Some(l), Some(r)) = (l, r) {
                    lines.push(format!("L  {} dB   R  {} dB", db(l), db(r)));
                }
            }
        }
        Some((lines, hv))
    }

    // MARK: Accessibility

    pub(super) fn accessibility_text(&self, base: &PanelBase) -> String {
        let Some(f) = base.frame.as_ref().filter(|f| !fmt::is_floor(f.peak.level_db)) else {
            return "No signal".to_string();
        };
        let mut s = format!("Peak {}", fmt::hz(f.peak.frequency_hz));
        if !f.peak.note_name.is_empty() {
            s += &format!(", note {}, {} cents", f.peak.note_name, f.peak.cents.round() as i64);
        }
        s += &format!(", {:.1} dB", f.peak.level_db as f64);
        let others: Vec<String> = f
            .top_peaks
            .iter()
            .filter(|pk| !pk.note_name.is_empty() && !is_same_peak(pk, &f.peak))
            .take(4)
            .map(|pk| format!("{} at {}", pk.note_name, fmt::hz(pk.frequency_hz)))
            .collect();
        if !others.is_empty() {
            s += &format!(". Further peaks: {}", others.join(", "));
        }
        if f.lowest_strong_hz > 0.0 {
            s += &format!(". Lowest strong content {}", whole_hz(f.lowest_strong_hz));
        }
        if self.hp_hidden {
            s += ". Headphone band hidden";
        }
        if let Some(hp) = &f.headphone {
            s += &format!(". Headphone overlay: {}", hp.model_name);
            for flag in hp.stress_flags.iter().filter(|_| self.show_stress_bands) {
                let Some(r) = &flag.frequency_range_hz else { continue };
                s += &format!(". {}, {} to {}", flag.title, fmt::hz(*r.start()), fmt::hz(*r.end()));
                if !flag.plot_label.is_empty() {
                    s += &format!(": {}", flag.plot_label);
                }
            }
        }
        s
    }
}
