//! A measured magnitude response: the curve type, its interpolation, and the
//! AutoEq-style CSV parser.
//!
//! Port source: the first half of `Sources/JoseonHeadphones/HeadphoneModel.swift`.

use std::fmt;

/// A measured magnitude response. `frequencies_hz` ascending; `levels_db` raw dB from the file.
#[derive(Debug, Clone, PartialEq)]
pub struct HeadphoneCurve {
    pub name: String,
    pub source: String,
    pub frequencies_hz: Vec<f32>,
    pub levels_db: Vec<f32>,
}

/// Why a curve file was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CurveParseError {
    /// Fewer than 8 usable points.
    NoData,
}

impl fmt::Display for CurveParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "the file has fewer than 8 usable data rows")
    }
}

impl std::error::Error for CurveParseError {}

impl HeadphoneCurve {
    /// Lowest and highest measured frequency. `(0, 0)` when the curve is empty.
    pub fn range_hz(&self) -> (f32, f32) {
        match (self.frequencies_hz.first(), self.frequencies_hz.last()) {
            (Some(&low), Some(&high)) => (low, high),
            _ => (0.0, 0.0),
        }
    }

    /// Mean level over 800–1250 Hz: the reference that normalization subtracts.
    ///
    /// Falls back to the interpolated 1 kHz level when no measured point lands in the band.
    pub fn reference_level_db(&self) -> f32 {
        let (mut sum, mut n) = (0.0f32, 0);
        for (f, level) in self.frequencies_hz.iter().zip(&self.levels_db) {
            if *f >= 800.0 && *f <= 1250.0 {
                sum += level;
                n += 1;
            }
        }
        if n == 0 {
            return CurveInterpolator::new(self.clone()).level(1_000.0);
        }
        sum / n as f32
    }

    /// The same curve shifted so that `reference_level_db` becomes 0 dB.
    pub fn normalized_to_1khz(&self) -> HeadphoneCurve {
        let reference = self.reference_level_db();
        let mut out = self.clone();
        if reference != 0.0 {
            for level in &mut out.levels_db {
                *level -= reference;
            }
        }
        out
    }
}

/// Linear-in-dB interpolation over log frequency. Flat (clamped) beyond the measured ends.
///
/// The log of every measured frequency is computed once, so a whole display
/// grid is resampled in one pass with a moving cursor.
#[derive(Debug, Clone)]
pub struct CurveInterpolator {
    curve: HeadphoneCurve,
    log_f: Vec<f32>,
}

fn safe_ln(hz: f32) -> f32 {
    hz.max(f32::MIN_POSITIVE).ln()
}

impl CurveInterpolator {
    pub fn new(curve: HeadphoneCurve) -> Self {
        let log_f = curve
            .frequencies_hz
            .iter()
            .map(|&f| if f > 0.0 { f } else { f32::MIN_POSITIVE }.ln())
            .collect();
        Self { curve, log_f }
    }

    pub fn curve(&self) -> &HeadphoneCurve {
        &self.curve
    }

    /// Points that have both a frequency and a level.
    fn point_count(&self) -> usize {
        self.log_f.len().min(self.curve.levels_db.len())
    }

    /// Level in dB at one frequency.
    pub fn level(&self, hz: f32) -> f32 {
        let levels = &self.curve.levels_db;
        let n = self.point_count();
        if n == 0 {
            return 0.0;
        }
        if n == 1 {
            return levels[0];
        }
        let log_f = &self.log_f;
        let x = safe_ln(hz);
        if x <= log_f[0] {
            return levels[0];
        }
        if x >= log_f[n - 1] {
            return levels[n - 1];
        }
        let (mut lo, mut hi) = (0, n - 1);
        while hi - lo > 1 {
            let mid = (lo + hi) / 2;
            if log_f[mid] <= x {
                lo = mid;
            } else {
                hi = mid;
            }
        }
        let span = log_f[hi] - log_f[lo];
        if !(span > 0.0) {
            return levels[lo];
        }
        let t = (x - log_f[lo]) / span;
        levels[lo] + t * (levels[hi] - levels[lo])
    }

    /// Level in dB on a whole ascending frequency grid.
    pub fn levels(&self, grid: &[f32]) -> Vec<f32> {
        let levels = &self.curve.levels_db;
        let n = self.point_count();
        if n == 0 {
            return vec![0.0; grid.len()];
        }
        if n == 1 {
            return vec![levels[0]; grid.len()];
        }
        let log_f = &self.log_f;
        let mut cursor = 0;
        grid.iter()
            .map(|&hz| {
                let x = safe_ln(hz);
                if x <= log_f[0] {
                    return levels[0];
                }
                if x >= log_f[n - 1] {
                    return levels[n - 1];
                }
                // Grids are ascending, so the cursor only moves forward.
                if log_f[cursor] > x {
                    cursor = 0;
                }
                while cursor + 1 < n - 1 && log_f[cursor + 1] <= x {
                    cursor += 1;
                }
                let span = log_f[cursor + 1] - log_f[cursor];
                if span > 0.0 {
                    let t = (x - log_f[cursor]) / span;
                    levels[cursor] + t * (levels[cursor + 1] - levels[cursor])
                } else {
                    levels[cursor]
                }
            })
            .collect()
    }
}

/// Parses an AutoEq-style CSV: a "frequency,raw,..." header, or two plain
/// columns "freq,dB", separated by a comma, a semicolon, a tab or spaces.
///
/// Rules:
/// - `#`, `*` and `//` start a comment line (REW exports use the first two).
/// - A header line is one that holds the word `frequency` (or `freq`, or `hz`).
///   The level column is then `raw` when present, never `smoothed`, `target` or
///   an `equalized_*` column. Otherwise it is the column after `frequency`.
/// - Rows that do not hold two finite numbers, or a positive frequency, are dropped.
/// - Points are sorted ascending. Duplicate frequencies are averaged.
/// - Fewer than 8 points left is `CurveParseError::NoData`.
pub fn parse_autoeq_csv(csv: &str, name: &str, source: &str) -> Result<HeadphoneCurve, CurveParseError> {
    let (mut freq_column, mut level_column) = (0, 1);
    let mut saw_header = false;
    let mut points: Vec<(f32, f32)> = Vec::new();

    // LF, CR and CRLF all end a line.
    for raw_line in csv.split(['\n', '\r', '\u{2028}', '\u{2029}', '\u{0B}', '\u{0C}', '\u{85}']) {
        let line = raw_line.trim();
        if line.is_empty() || line.starts_with('#') || line.starts_with('*') || line.starts_with("//") {
            continue;
        }
        let fields: Vec<&str> = line
            .split([',', ';', '\t', ' '])
            .filter(|field| !field.is_empty())
            .collect();
        if fields.len() < 2 {
            continue;
        }
        if !saw_header && points.is_empty() {
            let lowered: Vec<String> = fields.iter().map(|field| field.to_lowercase()).collect();
            if let Some(fi) = lowered
                .iter()
                .position(|f| f.starts_with("frequency") || f == "freq" || f == "hz")
            {
                saw_header = true;
                freq_column = fi;
                level_column = match lowered.iter().position(|f| f == "raw") {
                    Some(ri) => ri,
                    None if fi + 1 < fields.len() => fi + 1,
                    None => 1.min(fields.len() - 1),
                };
                continue;
            }
        }
        let (Some(f), Some(db)) = (fields.get(freq_column), fields.get(level_column)) else {
            continue;
        };
        let (Some(f), Some(db)) = (parse_number(f), parse_number(db)) else {
            continue;
        };
        if f.is_finite() && db.is_finite() && f > 0.0 {
            points.push((f, db));
        }
    }

    points.sort_by(|a, b| a.0.total_cmp(&b.0));

    // Duplicate frequencies are averaged, so the interpolator sees a strictly rising grid.
    let (mut frequencies_hz, mut levels_db) = (Vec::with_capacity(points.len()), Vec::with_capacity(points.len()));
    let mut i = 0;
    while i < points.len() {
        let mut j = i;
        let mut sum = 0.0f32;
        while j < points.len() && points[j].0 == points[i].0 {
            sum += points[j].1;
            j += 1;
        }
        frequencies_hz.push(points[i].0);
        levels_db.push(sum / (j - i) as f32);
        i = j;
    }

    if frequencies_hz.len() < 8 {
        return Err(CurveParseError::NoData);
    }
    Ok(HeadphoneCurve {
        name: name.to_string(),
        source: source.to_string(),
        frequencies_hz,
        levels_db,
    })
}

/// A number as Swift's `Float(String)` reads it: decimal or scientific, an
/// optional sign, and no text around it. "inf" and "nan" are numbers there and
/// here; the caller drops them.
fn parse_number(text: &str) -> Option<f32> {
    text.parse().ok()
}
