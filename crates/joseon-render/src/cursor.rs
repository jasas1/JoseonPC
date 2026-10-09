//! The linked cursor: what every panel shares. The numbers, the one-line
//! readout of the header row, the label at the pointer.
//!
//! Port source: `Sources/JoseonRender/CursorReadout.swift`, and `HoverLabel`
//! in `OffscreenRenderer.swift`.

use crate::fmt;
use crate::gpu::Color;
use crate::palette::{mix, with_alpha, Palette, WHITE};
use crate::text::fonts::{self, Font, Weight};
use crate::text::{HAlign, OverlayContext, Rect, VAlign};
use joseon_core::contracts::BandEnergy;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Tone {
    #[default]
    Value,
    Dim,
    Accent,
    Ghost,
    Reference,
}

/// One part of a cursor readout ("392.0 Hz", "G4 +0 ¢", "Mid −41.2 dB").
/// `rank`: 0 = the most important. A row that is too short drops the highest
/// ranks first; the parts that stay keep their reading order.
#[derive(Debug, Clone, PartialEq)]
pub struct CursorReadoutItem {
    pub text: String,
    pub rank: u32,
    pub tone: Tone,
    /// A small bar in this color before the text.
    pub swatch: Option<Color>,
}

impl CursorReadoutItem {
    pub const SWATCH_WIDTH: f32 = 12.0;

    // Order of importance: Hz, note, Mid dB, pan, L/R, at-ear, then. The SPL of the band and the Side trace go first
    // when the row is short. With a comparison set, "B − A" and "A" come right after Mid.
    pub const HZ: u32 = 0;
    pub const NOTE: u32 = 1;
    pub const MID: u32 = 2;
    pub const DELTA: u32 = 3;
    pub const REFERENCE: u32 = 4;
    pub const PAN: u32 = 5;
    pub const LEFT_RIGHT: u32 = 6;
    pub const AT_EAR: u32 = 7;
    pub const THEN: u32 = 8;
    pub const SPL: u32 = 9;
    pub const SIDE: u32 = 10;

    pub fn new(text: impl Into<String>, rank: u32, tone: Tone) -> Self {
        Self {
            text: text.into(),
            rank,
            tone,
            swatch: None,
        }
    }
}

/// One decimal under 1 kHz, `x.xx kHz` from 1 kHz, `xx.x kHz` from 10 kHz.
pub fn hz(f: f32) -> String {
    if !(f.is_finite() && f > 0.0) {
        return fmt::DASH.to_string();
    }
    if f >= 9_995.0 {
        return format!("{:.1} kHz", (f / 1000.0) as f64);
    }
    if f >= 999.95 {
        return format!("{:.2} kHz", (f / 1000.0) as f64);
    }
    format!("{:.1} Hz", f as f64)
}

/// "A4 +3 ¢": the equal-tempered note of a frequency (A4 = 440 Hz). `None` outside the range of note names.
pub fn note(f: f32) -> Option<String> {
    fmt::note(f).map(|(name, cents)| format!("{name} {}", fmt::cents(cents)))
}

/// "−3.2 s", or "now".
pub fn ago(seconds: f64) -> String {
    if seconds < 0.05 {
        "now".to_string()
    } else {
        format!("{}{seconds:.1} s", fmt::MINUS)
    }
}

/// Level of a display array at a frequency: linear interpolation in dB between
/// the two bins around it (the bins are uniform on a log axis). `None` outside
/// the bins. Every panel reads its number through this one function, so two
/// panels that show "Mid" at the cursor show the same number.
pub fn level(values: &[f32], frequencies: &[f32], hz: f32) -> Option<f32> {
    if !(hz > 0.0) {
        return None;
    }
    level_unchecked(values, frequencies, hz)
}

/// The same with no check of `hz` (the pointer readout of a panel with no link).
pub fn level_unchecked(values: &[f32], frequencies: &[f32], hz: f32) -> Option<f32> {
    let n = values.len().min(frequencies.len());
    if n < 2 {
        return None;
    }
    let f0 = frequencies[0];
    if !(f0 > 0.0 && frequencies[n - 1] > f0) {
        return None;
    }
    let idx = (hz / f0).ln() / (frequencies[n - 1] / f0).ln() * (n - 1) as f32;
    if !(idx >= 0.0 && idx <= (n - 1) as f32) {
        return None;
    }
    let i = (idx as usize).min(n - 2);
    let t = idx - i as f32;
    Some(values[i] * (1.0 - t) + values[i + 1] * t)
}

/// Index of the bin nearest to a frequency, `None` outside the bins.
pub fn nearest_bin(frequencies: &[f32], count: usize, hz: f32) -> Option<usize> {
    let n = count.min(frequencies.len());
    if !(n >= 2 && hz > 0.0 && frequencies[0] > 0.0 && frequencies[n - 1] > frequencies[0]) {
        return None;
    }
    let idx = (hz / frequencies[0]).ln() / (frequencies[n - 1] / frequencies[0]).ln() * (n - 1) as f32;
    if !(idx >= -0.5 && idx <= n as f32 - 0.5) {
        return None;
    }
    Some((idx.round() as i64).max(0).min(n as i64 - 1) as usize)
}

/// The listening band (0...7, `BandEnergy::EDGES_HZ`) that contains a frequency. `None` outside 20 Hz ... 24 kHz.
pub fn band(hz: f32) -> Option<usize> {
    let e = BandEnergy::EDGES_HZ;
    if !(hz >= e[0] && hz <= e[e.len() - 1]) {
        return None;
    }
    Some((0..e.len() - 1).find(|&i| hz < e[i + 1]).unwrap_or(e.len() - 2))
}

/// "4–6 kHz", "60–250 Hz", "500 Hz–2 kHz".
pub fn band_range(i: usize) -> String {
    let e = BandEnergy::EDGES_HZ;
    let (lo, hi) = (e[i], e[i + 1]);
    let n = |v: f32| {
        if v >= 1000.0 {
            fmt::axis_hz(v).replace('k', "")
        } else {
            format!("{}", v as i64)
        }
    };
    if lo >= 1000.0 {
        return format!("{}\u{2013}{} kHz", n(lo), n(hi));
    }
    if hi < 1000.0 {
        return format!("{}\u{2013}{} Hz", n(lo), n(hi));
    }
    format!("{} Hz\u{2013}{} kHz", n(lo), n(hi))
}

/// The third-octave band that contains a frequency: a step changes half way
/// (geometric) between two centers, the same rule the SPL layer of the spectrum draws with.
pub fn third_octave_band(centers: &[f32], hz: f32) -> Option<usize> {
    let n = centers.len();
    if n < 2 {
        return None;
    }
    let edge = 2f32.powf(1.0 / 6.0);
    if !(hz >= centers[0] / edge && hz <= centers[n - 1] * edge) {
        return None;
    }
    let mut b = 0;
    while b < n - 1 && hz * hz > centers[b] * centers[b + 1] {
        b += 1;
    }
    Some(b)
}

/// One semitone (or `semitones`) up or down, kept inside the axis.
pub fn step(hz: f32, semitones: f32, low: f32, high: f32) -> f32 {
    (hz * 2f32.powf(semitones / 12.0)).max(low).min(high)
}

/// The one-line cursor readout of a header row: right-aligned, parts separated
/// by a middle dot, the least important parts left out when the row is short.
/// A small glyph stands before it: a cross for a hover cursor, a pin for a pinned one.
pub mod header {
    use super::*;

    pub const SEPARATOR: &str = "  \u{00B7}  ";
    pub const TIGHT_SEPARATOR: &str = " \u{00B7} ";
    pub const GLYPH_WIDTH: f32 = 13.0;

    /// The parts that fit into `width`, in reading order.
    pub fn fit(
        o: &OverlayContext,
        items: &[CursorReadoutItem],
        width: f32,
        font: &Font,
        separator: &str,
    ) -> Vec<CursorReadoutItem> {
        let mut kept = items.to_vec();
        let sep = o.measure(separator, font, 0.0);
        let total = |v: &[CursorReadoutItem]| {
            v.iter().fold(GLYPH_WIDTH, |sum, item| {
                sum + o.measure(&item.text, font, 0.0)
                    + if item.swatch.is_some() {
                        CursorReadoutItem::SWATCH_WIDTH
                    } else {
                        0.0
                    }
            }) + sep * v.len().saturating_sub(1) as f32
        };
        while !kept.is_empty() && total(&kept) > width {
            // The highest rank goes. Of two with the same rank, the first one (as `max(by:)` in Swift).
            let mut worst = 0;
            for i in 0..kept.len() {
                if kept[i].rank > kept[worst].rank {
                    worst = i;
                }
            }
            kept.remove(worst);
        }
        kept
    }

    /// Draws the row with its right end at `right`. Gives the x where it starts
    /// (equal to `right` when nothing fits).
    #[allow(clippy::too_many_arguments)]
    pub fn draw(
        o: &mut OverlayContext,
        all: &[CursorReadoutItem],
        right: f32,
        left: f32,
        mid_y: f32,
        compact: bool,
        pinned: bool,
        p: &Palette,
    ) -> f32 {
        let font = fonts::ui(if compact { 10.0 } else { 11.0 }, Weight::Medium);
        let separator = if compact { TIGHT_SEPARATOR } else { SEPARATOR };
        let items = fit(o, all, right - left, &font, separator);
        if items.is_empty() {
            return right;
        }
        let mut x = right;
        for (i, item) in items.iter().enumerate().rev() {
            let color = match item.tone {
                Tone::Value => p.text,
                Tone::Dim => p.text_dim,
                Tone::Accent => mix(p.accent, WHITE, 0.45),
                Tone::Ghost => with_alpha(p.cursor_ghost, 1.0),
                Tone::Reference => with_alpha(p.reference(), 1.0),
            };
            x -= o.text(&item.text, x, mid_y, &font, color, HAlign::Right, VAlign::Middle);
            if let Some(c) = item.swatch {
                x -= CursorReadoutItem::SWATCH_WIDTH;
                o.fill_rect(Rect::new(x, mid_y - 2.0, 8.0, 4.0), c, 1.5);
            }
            if i > 0 {
                x -= o.text(separator, x, mid_y, &font, p.text_faint, HAlign::Right, VAlign::Middle);
            }
        }
        // The glyph: what this row is. It is not text, so it cannot be taken for a measured value.
        let gx = x - GLYPH_WIDTH + 4.0;
        if pinned {
            let c = mix(p.accent, WHITE, 0.25);
            o.fill_rect(Rect::new(gx - 2.5, mid_y - 5.0, 5.0, 5.0), c, 2.5);
            o.line([gx, mid_y], [gx, mid_y + 5.0], c, 1.0, &[]);
        } else {
            let c = with_alpha(p.text, 0.55);
            o.line([gx, mid_y - 5.0], [gx, mid_y + 5.0], c, 1.0, &[]);
            o.line([gx - 3.0, mid_y], [gx + 3.0, mid_y], c, 1.0, &[]);
        }
        x - GLYPH_WIDTH
    }
}

/// The small label that follows the pointer.
pub mod hover_label {
    use super::*;

    pub const LINE_HEIGHT: f32 = 15.0;
    pub const PADDING: f32 = 8.0;

    pub fn font() -> Font {
        fonts::ui(11.0, Weight::Medium)
    }

    /// Width and height of the box.
    pub fn box_size(o: &OverlayContext, lines: &[String]) -> (f32, f32) {
        let font = font();
        let w = lines.iter().map(|l| o.measure(l, &font, 0.0)).fold(0.0, f32::max);
        (
            (w + PADDING * 2.0).ceil(),
            lines.len() as f32 * LINE_HEIGHT + PADDING * 1.25,
        )
    }

    /// Top-left origin for the box: right and below the pointer, on the other
    /// side near the edges. `floor`: the box ends over this y. `None` = the bottom of `bounds`.
    pub fn origin(anchor: [f32; 2], size: (f32, f32), bounds: (f32, f32), floor: Option<f32>) -> [f32; 2] {
        let (mut x, mut y) = (anchor[0] + 14.0, anchor[1] + 14.0);
        let bottom = floor.unwrap_or(bounds.1 - 4.0).min(bounds.1 - 4.0);
        if x + size.0 > bounds.0 - 4.0 {
            x = anchor[0] - 14.0 - size.0;
        }
        if y + size.1 > bottom {
            y = anchor[1] - 14.0 - size.1;
        }
        if floor.is_some() {
            y = y.min(bottom - size.1);
        }
        [x.max(2.0), y.max(2.0)]
    }

    pub fn draw(o: &mut OverlayContext, lines: &[String], anchor: [f32; 2], p: &Palette, floor: Option<f32>) {
        let size = box_size(o, lines);
        let at = origin(anchor, size, (o.width, o.height), floor);
        draw_box(o, lines, Rect::new(at[0], at[1], size.0, size.1), p);
    }

    pub fn draw_box(o: &mut OverlayContext, lines: &[String], rect: Rect, p: &Palette) {
        let font = font();
        o.fill_rect(rect, with_alpha(p.background, 0.90), 6.0);
        o.stroke_rect(rect.inset(0.5, 0.5), p.grid_strong, 6.0, 1.0);
        for (i, line) in lines.iter().enumerate() {
            o.text(
                line,
                rect.min_x() + PADDING,
                rect.min_y() + PADDING * 0.6 + i as f32 * LINE_HEIGHT + 10.0,
                &font,
                if i == 0 { p.text } else { p.text_dim },
                HAlign::Left,
                VAlign::Baseline,
            );
        }
    }
}
