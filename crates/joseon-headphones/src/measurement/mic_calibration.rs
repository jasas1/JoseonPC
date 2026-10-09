//! Microphone calibration files and coupler corrections.
//!
//! Port source: `Sources/JoseonHeadphones/Measurement/MicCalibration.swift`.

use crate::curve::{CurveInterpolator, CurveParseError, HeadphoneCurve};

/// A parsed calibration file: frequency / dB pairs, plus the sensitivity header
/// when the file has one.
///
/// Reads the three things people have:
///
/// - **miniDSP UMIK-1 / UMIK-2**: a quoted header line
///   `"Sens Factor =-1.2dB, SERNO: 7012345"`, then tab-separated frequency, dB, phase.
/// - **REW and similar**: `*` or `#` comment lines, then two or three columns
///   separated by commas, tabs or spaces.
/// - Anything else with two numbers per line.
///
/// A 0° file and a 90° file are only different files. Joseon does not know
/// which one is loaded and says so in the warnings of the measurement.
#[derive(Debug, Clone, PartialEq)]
pub struct MicCalibration {
    pub name: String,
    /// Where it came from: a file name, for the method note of the measurement.
    pub source: String,
    /// Ascending, strictly rising, positive.
    pub frequencies_hz: Vec<f64>,
    /// The deviation of the microphone in dB. Subtract it from a measurement to remove the microphone.
    pub levels_db: Vec<f64>,
    /// The `Sens Factor` value in dB, when the file had one.
    pub sens_factor_db: Option<f64>,
    /// True when the file had a third (phase) column. The phase is read and discarded.
    pub has_phase_column: bool,
}

impl MicCalibration {
    /// The dBFS level a UMIK-style microphone is taken to give at 94 dB SPL
    /// when its `Sens Factor` is 0 dB.
    ///
    /// **This is a convention, not a measurement.** It was never checked against
    /// a real microphone. Anything derived from it carries
    /// `SENS_FACTOR_UNCERTAINTY_DB`, and the app must offer the acoustic
    /// calibrator as the route to trust.
    pub const SENS_FACTOR_REFERENCE_DBFS: f64 = -18.0;

    /// Uncertainty Joseon gives to the `Sens Factor` route, in dB, one side.
    pub const SENS_FACTOR_UNCERTAINTY_DB: f64 = 2.0;

    /// Parses the text of a calibration file. An empty `source` becomes the name.
    ///
    /// Gives `CurveParseError::NoData` when fewer than four usable rows are left.
    pub fn parse(text: &str, name: &str, source: &str) -> Result<Self, CurveParseError> {
        let parsed = parse_calibration_file(text)?;
        Ok(Self {
            name: name.to_string(),
            source: if source.is_empty() { name } else { source }.to_string(),
            frequencies_hz: parsed.frequencies_hz,
            levels_db: parsed.levels_db,
            sens_factor_db: parsed.sens_factor_db,
            has_phase_column: parsed.has_phase_column,
        })
    }

    /// The same data as a `HeadphoneCurve`, for the interpolator.
    pub fn curve(&self) -> HeadphoneCurve {
        as_curve(&self.name, &self.source, &self.frequencies_hz, &self.levels_db)
    }

    /// The correction in dB on a frequency grid: flat beyond the ends of the
    /// file, linear in dB over log frequency between its points.
    pub fn correction_db(&self, grid: &[f64]) -> Vec<f64> {
        correction_on(self.curve(), grid)
    }

    /// Removes the microphone from a measured magnitude response: subtracts the correction.
    pub fn apply(&self, magnitude_db: &[f64], grid: &[f64]) -> Vec<f64> {
        subtract(magnitude_db, &self.correction_db(grid))
    }

    /// dBFS per pascal from the `Sens Factor` header, or `None` when the file had none.
    ///
    /// 1 Pa is 94 dB SPL, so this is the digital RMS level (a full-scale sine
    /// reads −3.01 dBFS) that 94 dB SPL at 1 kHz gives.
    pub fn mic_sensitivity_dbfs_per_pascal(&self) -> Option<f64> {
        self.sens_factor_db.map(|sens| Self::SENS_FACTOR_REFERENCE_DBFS + sens)
    }
}

/// An optional correction for the rig the headphone is on.
///
/// A standards-type ear simulator (IEC 60318-4) needs none. A flat plate or a
/// home-made coupler needs one, and the only honest source is a curve from
/// whoever characterised the rig. Same file format as a microphone calibration.
#[derive(Debug, Clone, PartialEq)]
pub struct CouplerCorrection {
    pub name: String,
    pub source: String,
    pub frequencies_hz: Vec<f64>,
    pub levels_db: Vec<f64>,
}

impl CouplerCorrection {
    pub fn parse(text: &str, name: &str, source: &str) -> Result<Self, CurveParseError> {
        let parsed = parse_calibration_file(text)?;
        Ok(Self {
            name: name.to_string(),
            source: if source.is_empty() { name } else { source }.to_string(),
            frequencies_hz: parsed.frequencies_hz,
            levels_db: parsed.levels_db,
        })
    }

    pub fn curve(&self) -> HeadphoneCurve {
        as_curve(&self.name, &self.source, &self.frequencies_hz, &self.levels_db)
    }

    pub fn correction_db(&self, grid: &[f64]) -> Vec<f64> {
        correction_on(self.curve(), grid)
    }

    pub fn apply(&self, magnitude_db: &[f64], grid: &[f64]) -> Vec<f64> {
        subtract(magnitude_db, &self.correction_db(grid))
    }
}

fn as_curve(name: &str, source: &str, frequencies_hz: &[f64], levels_db: &[f64]) -> HeadphoneCurve {
    HeadphoneCurve {
        name: name.to_string(),
        source: source.to_string(),
        frequencies_hz: frequencies_hz.iter().map(|&v| v as f32).collect(),
        levels_db: levels_db.iter().map(|&v| v as f32).collect(),
    }
}

fn correction_on(curve: HeadphoneCurve, grid: &[f64]) -> Vec<f64> {
    let grid: Vec<f32> = grid.iter().map(|&hz| hz as f32).collect();
    CurveInterpolator::new(curve)
        .levels(&grid)
        .into_iter()
        .map(f64::from)
        .collect()
}

fn subtract(magnitude: &[f64], correction: &[f64]) -> Vec<f64> {
    magnitude.iter().zip(correction).map(|(m, c)| m - c).collect()
}

// MARK: The shared parser

pub(crate) struct ParsedCalibration {
    pub frequencies_hz: Vec<f64>,
    pub levels_db: Vec<f64>,
    pub sens_factor_db: Option<f64>,
    pub has_phase_column: bool,
}

/// One tolerant reader for every calibration file shape.
pub(crate) fn parse_calibration_file(text: &str) -> Result<ParsedCalibration, CurveParseError> {
    let mut points: Vec<(f64, f64)> = Vec::new();
    let mut sens_factor: Option<f64> = None;
    let mut has_phase = false;

    // LF, CR and CRLF all end a line, so a Windows file needs no special case.
    let is_newline = |c: char| {
        matches!(
            c,
            '\n' | '\r' | '\u{0B}' | '\u{0C}' | '\u{85}' | '\u{2028}' | '\u{2029}'
        )
    };
    for raw_line in text.split(is_newline) {
        let mut line = raw_line.trim();
        if line.is_empty() {
            continue;
        }

        // A quoted line is a header (miniDSP quotes theirs). Remove the quotes before the look inside.
        if line.starts_with('"') {
            line = line.trim_start_matches('"');
            if let Some(end) = line.rfind('"') {
                line = &line[..end];
            }
        }
        if sens_factor.is_none() {
            if let Some(value) = sens_factor_value(line) {
                sens_factor = Some(value);
                continue;
            }
        }
        if line.starts_with('#') || line.starts_with('*') || line.starts_with("//") || line.starts_with(';') {
            continue;
        }

        let fields: Vec<&str> = line
            .split([',', ';', '\t', ' '])
            .filter(|field| !field.is_empty())
            .collect();
        if fields.len() < 2 {
            continue;
        }
        if fields.len() >= 3 && fields[2].parse::<f64>().is_ok() {
            has_phase = true;
        }
        let (Ok(f), Ok(db)) = (fields[0].parse::<f64>(), fields[1].parse::<f64>()) else {
            continue;
        };
        if !(f.is_finite() && db.is_finite() && f > 0.0) {
            continue;
        }
        points.push((f, db));
    }

    points.sort_by(|a, b| a.0.total_cmp(&b.0));
    let (mut frequencies, mut levels) = (Vec::new(), Vec::new());
    let mut i = 0;
    while i < points.len() {
        let mut j = i;
        let mut sum = 0.0;
        while j < points.len() && points[j].0 == points[i].0 {
            sum += points[j].1;
            j += 1;
        }
        frequencies.push(points[i].0);
        levels.push(sum / (j - i) as f64);
        i = j;
    }

    if frequencies.len() < 4 {
        return Err(CurveParseError::NoData);
    }
    Ok(ParsedCalibration {
        frequencies_hz: frequencies,
        levels_db: levels,
        sens_factor_db: sens_factor,
        has_phase_column: has_phase,
    })
}

/// `Sens Factor =-1.2dB, SERNO: 7012345` → `-1.2`. Case and spaces do not
/// matter, and the number may be written `-.1`.
pub(crate) fn sens_factor_value(line: &str) -> Option<f64> {
    let lowered = line.to_lowercase();
    if !(lowered.contains("sens factor") || lowered.contains("sensfactor")) {
        return None;
    }
    let equals = line.find('=')?;
    let mut digits = String::new();
    for character in line[equals + 1..].chars() {
        if character == ' ' && digits.is_empty() {
            continue;
        }
        let exponent =
            (character == 'e' || character == 'E') && digits.chars().last().is_some_and(|c| c.is_ascii_digit());
        if character.is_ascii_digit() || matches!(character, '.' | '-' | '+') || exponent {
            digits.push(character);
        } else {
            break;
        }
    }
    digits.parse().ok()
}
