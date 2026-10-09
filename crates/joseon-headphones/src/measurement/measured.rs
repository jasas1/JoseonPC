//! The result of a measurement: the curve, its quality, the leak test, the CSV.
//!
//! Port source: `Sources/JoseonHeadphones/Measurement/MeasuredHeadphone.swift`.

use super::absolute_level::DerivedSensitivity;
use super::fft::fixed;
use crate::curve::{CurveInterpolator, HeadphoneCurve};
use joseon_core::contracts::HeadphoneSensitivity;

/// How much to believe a measurement.
///
/// The user can act on every field: a low `snr_db` in the bass means measure
/// again in a quiet room, a high `thd_percent` means lower the level, a large
/// `agreement_db` means the headphone moved between runs.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct MeasurementQuality {
    /// The grid of `snr_db` and `agreement_spread_db`: the 200-point grid.
    pub frequencies_hz: Vec<f32>,
    /// Signal-to-noise ratio per grid point, in dB, after the processing gain
    /// of the sweep. Empty when no noise-only segment was given.
    pub snr_db: Vec<f32>,
    /// Total harmonic distortion (2nd + 3rd) as a percentage, over the whole sweep.
    pub thd_percent: f32,
    /// How many runs were averaged.
    pub runs: usize,
    /// Mean run-to-run spread of the magnitude over 100 Hz ... 10 kHz, in dB. 0 for one run.
    pub agreement_db: f32,
    /// Run-to-run spread per grid point. Empty for one run.
    pub agreement_spread_db: Vec<f32>,
    /// Lowest frequency the analysis window can resolve. `lowest_reliable_hz` is the number to show.
    pub lowest_resolved_hz: f32,
    /// Lowest frequency with at least 10 dB of signal-to-noise. 0 when no
    /// noise-only segment was recorded: "not known", not "good down to DC".
    pub noise_limited_hz: f32,
    /// Highest frequency the stimulus drove at full level. 0 when not stated.
    /// Above it the curve is held flat, so the UI stops the line there.
    pub highest_reliable_hz: f32,
    /// Length of the analysis window in milliseconds.
    pub window_milliseconds: f32,
    /// Plain sentences about what this measurement does and does not show.
    pub warnings: Vec<String>,
}

impl MeasurementQuality {
    /// Signal-to-noise ratio at one frequency, or `None` when there is no noise segment.
    pub fn snr_db_at(&self, hz: f32) -> Option<f32> {
        if self.snr_db.is_empty() || self.frequencies_hz.len() != self.snr_db.len() {
            return None;
        }
        let (mut best, mut best_distance) = (0, f32::MAX);
        for (i, &f) in self.frequencies_hz.iter().enumerate() {
            let d = (f.max(1.0).ln() - hz.max(1.0).ln()).abs();
            if d < best_distance {
                (best, best_distance) = (i, d);
            }
        }
        Some(self.snr_db[best])
    }

    /// Lowest frequency where the signal-to-noise ratio is at least
    /// `minimum_db` (the macOS default is 10). `None` when there is no noise
    /// segment, or when no point reaches it.
    pub fn lowest_trusted_hz(&self, minimum_db: f32) -> Option<f32> {
        if self.snr_db.is_empty() || self.frequencies_hz.len() != self.snr_db.len() {
            return None;
        }
        self.snr_db
            .iter()
            .position(|&snr| snr >= minimum_db)
            .map(|i| self.frequencies_hz[i])
    }

    /// **The** lower limit of the measurement: the higher of the noise-limited
    /// frequency and the window-limited one. Only this one is shown.
    pub fn lowest_reliable_hz(&self) -> f32 {
        self.noise_limited_hz.max(self.lowest_resolved_hz)
    }

    /// Which limit won. A tie goes to the window: it is always there.
    pub fn low_limit_cause(&self) -> LowLimitCause {
        if self.noise_limited_hz > self.lowest_resolved_hz {
            LowLimitCause::Noise
        } else {
            LowLimitCause::Window
        }
    }
}

/// What sets the bottom of a measurement.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LowLimitCause {
    /// Room noise. The fix is a quieter room, more runs, or more level.
    Noise,
    /// The analysis window. The fix is a longer window, which needs a quieter room.
    Window,
}

impl LowLimitCause {
    /// The clause the warning uses after the number.
    pub fn reason(self) -> &'static str {
        match self {
            Self::Noise => "room noise",
            Self::Window => "the length of the analysis window",
        }
    }
}

/// How far the measured bass is under a reference curve: the leak test.
///
/// A headphone that has lost its seal on the rig looks like a first-order
/// high-pass. Against a published curve of the same model that shows as a large
/// negative mean over 30–100 Hz.
#[derive(Debug, Clone, PartialEq)]
pub struct BassShortfall {
    /// Mean of (measured − reference) over the range, in dB, both curves
    /// normalized at 1 kHz. Negative: the measurement has less bass.
    pub mean_difference_db: f64,
    /// The range used: 30–100 Hz cut to what both curves cover.
    pub low_hz: f64,
    pub high_hz: f64,
    /// How many grid points went into the mean.
    pub points_used: usize,
    /// True when the shortfall is worse than `LEAK_THRESHOLD_DB`.
    pub likely_leak: bool,
    /// The sentence for the UI, and for the confirmation sheet before Save.
    pub summary: String,
}

impl BassShortfall {
    /// More than 6 dB under the reference is called a likely leak.
    pub const LEAK_THRESHOLD_DB: f64 = -6.0;
}

/// The result of one measurement session: a curve, possibly a sensitivity, and
/// the quality behind both.
#[derive(Debug, Clone, PartialEq)]
pub struct MeasuredHeadphone {
    /// Normalized to 0 dB at 1 kHz, on the 200-point grid: the same shape as
    /// every embedded curve.
    pub curve: HeadphoneCurve,
    /// The measured response **before** normalization, in dB relative to the
    /// digital drive, after the microphone and coupler corrections.
    pub absolute_magnitude_db: Vec<f32>,
    /// Derived sensitivity, when there was an absolute level reference and a playback calibration.
    pub sensitivity: Option<HeadphoneSensitivity>,
    /// The full derivation, with its uncertainty and workings.
    pub derived_sensitivity: Option<DerivedSensitivity>,
    pub quality: MeasurementQuality,
    /// One sentence that names the method, for the `source` of the curve and for the report.
    pub method: String,
}

impl MeasuredHeadphone {
    // MARK: Leak detection

    /// `bass_shortfall_in` over 30–100 Hz.
    pub fn bass_shortfall(&self, reference: &HeadphoneCurve) -> Option<BassShortfall> {
        self.bass_shortfall_in(reference, 30.0, 100.0)
    }

    /// Mean of (measured − reference) over a range, both curves normalized at 1 kHz.
    ///
    /// `None` when the two curves do not overlap over enough of the range. A
    /// reference that stops at 50 Hz is extrapolated flat by the interpolator,
    /// and a comparison against an extrapolation would invent a leak or hide
    /// one. At least three grid points of real overlap are necessary.
    pub fn bass_shortfall_in(
        &self,
        reference: &HeadphoneCurve,
        requested_low_hz: f64,
        requested_high_hz: f64,
    ) -> Option<BassShortfall> {
        let measured = self.curve.normalized_to_1khz();
        let published = reference.normalized_to_1khz();
        let (measured_low, measured_high) = (*measured.frequencies_hz.first()?, *measured.frequencies_hz.last()?);
        let (reference_low, reference_high) = (*published.frequencies_hz.first()?, *published.frequencies_hz.last()?);

        // Only where both curves have data.
        let low = requested_low_hz.max(measured_low as f64).max(reference_low as f64);
        let high = requested_high_hz.min(measured_high as f64).min(reference_high as f64);
        if !(high > low) {
            return None;
        }

        let name = if published.name.is_empty() {
            "the published curve".to_string()
        } else {
            format!("\"{}\"", published.name)
        };
        let interpolator = CurveInterpolator::new(published);
        let (mut sum, mut used) = (0.0, 0usize);
        for (i, &f) in measured.frequencies_hz.iter().enumerate() {
            if !(f as f64 >= low && f as f64 <= high) {
                continue;
            }
            let difference = measured.levels_db[i] as f64 - interpolator.level(f) as f64;
            if !difference.is_finite() {
                continue;
            }
            sum += difference;
            used += 1;
        }
        if used < 3 {
            return None;
        }

        let mean = sum / used as f64;
        let leak = mean < BassShortfall::LEAK_THRESHOLD_DB;
        let range = format!("{} Hz and {} Hz", fixed(low, 0), fixed(high, 0));
        let size = fixed(mean.abs(), 0);
        let summary = if leak {
            format!("Bass is {size} dB under {name} between {range}: likely a seal leak on the rig. Reseat the headphone and measure again before saving this curve.")
        } else if mean < 0.0 {
            format!("Bass is {size} dB under {name} between {range}.")
        } else {
            format!("Bass is {size} dB over {name} between {range}.")
        };
        Some(BassShortfall {
            mean_difference_db: mean,
            low_hz: low,
            high_hz: high,
            points_used: used,
            likely_leak: leak,
            summary,
        })
    }

    /// What the measured curve alone says about the bass, with no reference
    /// curve. `None` when the curve does not fall enough for a sentence.
    pub fn bass_roll_off_note(&self) -> Option<String> {
        let levels: Vec<f64> = self.curve.levels_db.iter().map(|&v| v as f64).collect();
        let grid: Vec<f64> = self.curve.frequencies_hz.iter().map(|&v| v as f64).collect();
        Self::bass_roll_off_note_for(
            &levels,
            &grid,
            self.quality.snr_db_at(40.0).map(f64::from),
            10.0,
            Self::SNR_THRESHOLD_DB,
        )
    }

    /// The signal-to-noise ratio at 40 Hz from which a bass fall may be called a
    /// seal leak, in dB. THE one number for "is the bass measured, or is it the
    /// noise floor".
    pub const SNR_THRESHOLD_DB: f64 = 15.0;

    /// True when there is enough signal at 40 Hz to say "seal leak". A missing
    /// figure (no noise-only segment) is not enough.
    pub fn bass_is_measured(snr_at_40hz: Option<f64>) -> bool {
        snr_at_40hz.is_some_and(|snr| snr.is_finite() && snr >= Self::SNR_THRESHOLD_DB)
    }

    /// How every roll-off note starts. `is_bass_roll_off_note` matches on it.
    pub(crate) const BASS_ROLL_OFF_NOTE_PREFIX: &'static str = "The measured curve falls ";

    /// True for a sentence that `bass_roll_off_note` wrote (any of its three verdicts).
    pub fn is_bass_roll_off_note(warning: &str) -> bool {
        warning.starts_with(Self::BASS_ROLL_OFF_NOTE_PREFIX)
    }

    /// A leak looks like a first-order high-pass, so the test is the fall from
    /// 100 Hz down to 30 Hz. More than 10 dB (`fall_threshold_db`) is more than
    /// any sealed over-ear headphone does.
    ///
    /// The sentence says "seal leak" only when there is signal down there: at
    /// least `snr_threshold_db` of signal-to-noise at 40 Hz.
    pub fn bass_roll_off_note_for(
        levels: &[f64],
        grid: &[f64],
        snr_at_40hz: Option<f64>,
        fall_threshold_db: f64,
        snr_threshold_db: f64,
    ) -> Option<String> {
        if levels.len() != grid.len() || grid.is_empty() {
            return None;
        }
        let level_at = |hz: f64| -> Option<f64> {
            let (mut best, mut best_distance) = (None, f64::MAX);
            for (i, &f) in grid.iter().enumerate() {
                if !(f > 0.0) {
                    continue;
                }
                let d = (f.ln() - hz.ln()).abs();
                if d < best_distance {
                    (best, best_distance) = (Some(i), d);
                }
            }
            // Only trust a grid point within a sixth of an octave of the frequency asked for.
            let best = best?;
            (best_distance < 2.0f64.powf(1.0 / 6.0).ln()).then(|| levels[best])
        };
        let fall = level_at(100.0)? - level_at(30.0)?;
        if !(fall.is_finite() && fall > fall_threshold_db) {
            return None;
        }

        let start = format!(
            "{}{} dB from 100 Hz to 30 Hz",
            Self::BASS_ROLL_OFF_NOTE_PREFIX,
            fixed(fall, 0)
        );
        let Some(snr) = snr_at_40hz else {
            return Some(start + ". With no noise-only segment there is no signal-to-noise figure down there, so Joseon can not tell a seal leak from the noise floor. Record silence and measure again.");
        };
        if snr >= snr_threshold_db {
            return Some(format!("{start}, with {} dB of signal-to-noise at 40 Hz. That looks like a seal leak on the rig rather than the headphone: reseat it and measure again.", fixed(snr, 0)));
        }
        Some(format!("{start}, but signal-to-noise at 40 Hz is only {} dB, so the bass here is limited by noise rather than measured. Measure again in a quieter room, or at a higher level if there is headroom.", fixed(snr, 0)))
    }

    // MARK: Average of two sides

    /// The sentence that opens the warnings of an averaged result.
    pub const AVERAGE_NOTE: &'static str = "This curve is the average of the left and the right side, measured one after the other. Agreement, distortion and signal to noise are the worse value of the two sides.";

    /// The mean of two sides: the mean of the two dB curves, with the worse
    /// quality figure of the two. The derived sensitivity is NOT averaged: the
    /// result carries the left one until the caller sets its own.
    ///
    /// The warnings are the union of both sides, except the bass roll-off note.
    /// The average gets exactly one, from the averaged curve and the worse
    /// signal-to-noise of the two sides, or none.
    pub fn average(l: &MeasuredHeadphone, r: &MeasuredHeadphone) -> MeasuredHeadphone {
        let mean = |a: &[f32], b: &[f32]| -> Vec<f32> { a.iter().zip(b).map(|(a, b)| (a + b) / 2.0).collect() };
        let mut m = l.clone();
        m.curve.levels_db = mean(&l.curve.levels_db, &r.curve.levels_db);
        m.curve = m.curve.normalized_to_1khz();
        m.absolute_magnitude_db = mean(&l.absolute_magnitude_db, &r.absolute_magnitude_db);
        let mut q = l.quality.clone();
        let rq = &r.quality;
        q.runs += rq.runs;
        q.agreement_db = q.agreement_db.max(rq.agreement_db);
        q.thd_percent = q.thd_percent.max(rq.thd_percent);
        q.lowest_resolved_hz = q.lowest_resolved_hz.max(rq.lowest_resolved_hz);
        if q.snr_db.len() == rq.snr_db.len() {
            q.snr_db = q.snr_db.iter().zip(&rq.snr_db).map(|(a, b)| a.min(*b)).collect();
        }

        // Where the left side had its roll-off note, the note of the average goes.
        let note_index = q.warnings.iter().position(|w| Self::is_bass_roll_off_note(w));
        let mut warnings: Vec<String> = q
            .warnings
            .iter()
            .filter(|w| !Self::is_bass_roll_off_note(w))
            .cloned()
            .collect();
        for warning in &rq.warnings {
            if !Self::is_bass_roll_off_note(warning) && !warnings.contains(warning) {
                warnings.push(warning.clone());
            }
        }
        m.quality = q;
        if let Some(note) = m.bass_roll_off_note() {
            let at = note_index.unwrap_or(warnings.len()).min(warnings.len());
            warnings.insert(at, note);
        }
        m.quality.warnings = std::iter::once(Self::AVERAGE_NOTE.to_string())
            .chain(warnings)
            .collect();
        m.method = format!("Average of left and right; {}", l.method);
        m
    }

    /// The curve in the AutoEq-compatible `frequency,raw` format, ready to save.
    ///
    /// `HeadphoneLibrary::import_curve` reads this back as the same curve.
    /// Comment lines carry the method and the quality: a bare column of numbers
    /// a year from now is a curve nobody can judge.
    pub fn csv(&self) -> String {
        let q = &self.quality;
        let mut lines = vec![
            format!("# {}", self.curve.name),
            format!("# {}", self.method),
            format!(
                "# Runs: {}; run-to-run agreement: {} dB; THD: {} %",
                q.runs,
                fixed(q.agreement_db as f64, 2),
                fixed(q.thd_percent as f64, 2)
            ),
            format!(
                "# Analysis window {} ms; valid from {} Hz (limit set by {}) to {} Hz",
                fixed(q.window_milliseconds as f64, 0),
                fixed(q.lowest_reliable_hz() as f64, 0),
                q.low_limit_cause().reason(),
                fixed(q.highest_reliable_hz as f64, 0)
            ),
        ];
        for warning in &q.warnings {
            lines.push(format!("# Note: {warning}"));
        }
        lines.push("frequency,raw".to_string());
        for (f, db) in self.curve.frequencies_hz.iter().zip(&self.curve.levels_db) {
            lines.push(format!("{},{}", fixed(*f as f64, 4), fixed(*db as f64, 4)));
        }
        lines.join("\n") + "\n"
    }
}
