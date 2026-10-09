//! Number and note formats of the panels.
//!
//! Port source: `Fmt` and `BandLabels` in `Sources/JoseonRender/TextOverlay.swift`.

pub const MINUS: &str = "\u{2212}";
pub const DASH: &str = "\u{2014}";

/// Floor values read as an em dash, not as -120.
pub fn is_floor(v: f32) -> bool {
    !v.is_finite() || v <= -119.0
}

pub fn db(v: f32, digits: usize, signed: bool) -> String {
    if is_floor(v) {
        return DASH.to_string();
    }
    number(v, digits, signed)
}

pub fn number(v: f32, digits: usize, signed: bool) -> String {
    if !v.is_finite() {
        return DASH.to_string();
    }
    let p = 10f32.powf(digits as f32);
    let r = (v * p).round() / p;
    let body = format!("{:.digits$}", r.abs() as f64);
    if r < 0.0 {
        return format!("{MINUS}{body}");
    }
    if signed && r > 0.0 {
        format!("+{body}")
    } else {
        body
    }
}

pub fn hz(f: f32) -> String {
    if !(f.is_finite() && f > 0.0) {
        return DASH.to_string();
    }
    // One decimal: the analyzer does not resolve more, and more digits would claim it does.
    if f >= 10_000.0 {
        return format!("{:.1} kHz", (f / 1000.0) as f64);
    }
    format!("{:.1} Hz", f as f64)
}

pub fn axis_hz(f: f32) -> String {
    if f >= 1000.0 {
        let k = f / 1000.0;
        return if k == k.round() {
            format!("{}k", k as i64)
        } else {
            format!("{:.1}k", k as f64)
        };
    }
    format!("{}", f as i64)
}

const NOTE_NAMES: [&str; 12] = [
    "C",
    "C\u{266F}",
    "D",
    "D\u{266F}",
    "E",
    "F",
    "F\u{266F}",
    "G",
    "G\u{266F}",
    "A",
    "A\u{266F}",
    "B",
];

/// Equal-tempered note for a frequency, A4 = 440 Hz: the name and the cents off.
pub fn note(f: f32) -> Option<(String, f32)> {
    if !(f.is_finite() && f >= 8.0 && f <= 30_000.0) {
        return None;
    }
    let n = 69.0 + 12.0 * (f / 440.0).log2();
    let nearest = n.round();
    let idx = nearest as i64;
    if idx < 0 {
        return None;
    }
    let name = format!("{}{}", NOTE_NAMES[(idx % 12) as usize], idx / 12 - 1);
    Some((name, (n - nearest) * 100.0))
}

/// Contract note names use "#"; show the real sharp sign.
pub fn pretty_note(s: &str) -> String {
    s.replace('#', "\u{266F}")
}

pub fn cents(c: f32) -> String {
    let r = c.round();
    if r == 0.0 {
        return "\u{00B1}0 \u{00A2}".to_string();
    }
    format!("{}{} \u{00A2}", if r > 0.0 { "+" } else { MINUS }, r.abs() as i64)
}

/// Names of the 8 listening bands in two short lengths. The long names are `BandEnergy::NAMES`.
pub mod band_labels {
    pub const SHORT: [&str; 8] = ["Sub", "Bass", "LoMid", "Mid", "UpMid", "Pres", "Brill", "Air"];
    pub const TWO: [&str; 8] = ["Su", "Ba", "LM", "Mi", "UM", "Pr", "Br", "Ai"];

    /// The longest set of names that fits a column. Under 480 pt of panel width the two-letter set, always: eight
    /// longer names in a narrow card read as one word.
    pub fn fitting(
        o: &crate::text::OverlayContext,
        font: &crate::text::fonts::Font,
        column_width: f32,
        gap: f32,
        panel_width: f32,
    ) -> [&'static str; 8] {
        if panel_width < 480.0 {
            return TWO;
        }
        for set in [joseon_core::contracts::BandEnergy::NAMES, SHORT] {
            let widest = set.iter().map(|name| o.measure(name, font, 0.0)).fold(0.0, f32::max);
            if widest + gap <= column_width {
                return set;
            }
        }
        TWO
    }
}
