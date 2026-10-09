//! One measurement session, from the first sweep to the curve that gets saved.
//!
//! Port source: `Sources/JoseonHeadphones/Measurement/HeadphoneMeasurement.swift`.
//!
//! The order of the calls is the order of the work:
//!
//! 1. Make the stimulus with `SweepSignal::exponential_sweep`. The app plays
//!    `sweep.samples`, only after the user presses a button that says it will
//!    make sound.
//! 2. Load what is known about the rig: `MicCalibration`, `CouplerCorrection`.
//!    Both are optional, and the warnings say so when they are missing.
//! 3. Open the session with `HeadphoneMeasurement::new`.
//! 4. Record silence for as long as the sweep and call `set_noise_segment`.
//!    Without it there is no signal-to-noise figure.
//! 5. Play and record the sweep N times. Call `add_run` for each recording.
//! 6. Call `result`. The sensitivity context is optional.
//! 7. Save `MeasuredHeadphone::csv` and give the file to `HeadphoneLibrary::import_curve`.
//!
//! Nothing here opens a device, plays a sound or writes a file.

use super::absolute_level::{derive_sensitivity, AbsoluteLevel, Derivation, DerivedSensitivityResult, MeasurementRig};
use super::fft::{amplitude_db, fixed};
use super::measured::{LowLimitCause, MeasuredHeadphone, MeasurementQuality};
use super::mic_calibration::{CouplerCorrection, MicCalibration};
use super::sweep_analysis::{self as analysis, ComplexResponse, OctaveSmoothing, Options, SweepDeconvolver};
use super::sweep_signal::{SweepKind, SweepSignal};
use crate::curve::HeadphoneCurve;
use joseon_core::contracts::PlaybackCalibration;

/// What one run looked like, for the live read-out during a measurement.
#[derive(Debug, Clone, PartialEq)]
pub struct RunSummary {
    /// Play-to-record delay in samples, from the peak of the deconvolved impulse response.
    pub delay_samples: usize,
    pub delay_seconds: f64,
    /// The same delay from a plain cross-correlation against the sweep, when it was asked for.
    pub cross_correlation_delay_samples: Option<usize>,
    /// Impulse peak level in dB relative to full scale.
    pub peak_level_db: f64,
    /// Noise floor of this run in dB below the impulse peak.
    pub noise_floor_db: f64,
    /// Total harmonic distortion of this run, in percent.
    pub thd_percent: f64,
    /// True when a sample in the recording reached ±0.999.
    pub clipped: bool,
}

/// Everything the sensitivity derivation needs that is not in the measurement.
#[derive(Debug, Clone, PartialEq)]
pub struct SensitivityContext {
    pub playback: PlaybackCalibration,
    pub absolute_level: AbsoluteLevel,
    pub impedance_ohms: f64,
    /// Replaces `drive_voltage_uncertainty_db(playback)` when the app knows better.
    pub drive_voltage_uncertainty_db: Option<f64>,
    /// What the headphone is on. The default is the flat plate: most people
    /// have a flat plate, not an ear simulator.
    pub rig: MeasurementRig,
}

impl SensitivityContext {
    pub fn new(playback: PlaybackCalibration, absolute_level: AbsoluteLevel, impedance_ohms: f64) -> Self {
        Self {
            playback,
            absolute_level,
            impedance_ohms,
            drive_voltage_uncertainty_db: None,
            rig: MeasurementRig::FlatPlate,
        }
    }
}

/// Not thread safe by design: use it from one thread.
pub struct HeadphoneMeasurement {
    pub sweep: SweepSignal,
    pub options: Options,
    pub mic_calibration: Option<MicCalibration>,
    pub coupler: Option<CouplerCorrection>,
    /// The frequency grid of the result. The 200-point log grid by default.
    pub grid: Vec<f64>,
    saw_clipping: bool,

    deconvolver: Option<SweepDeconvolver>,
    responses: Vec<ComplexResponse>,
    summaries: Vec<RunSummary>,
    distortions: Vec<f64>,
    window_pre: usize,
    window_post: usize,
    noise_segment: Vec<f64>,
    truncated_run: bool,
}

impl HeadphoneMeasurement {
    /// A session with the default options, no corrections, and the standard grid.
    pub fn new(sweep: SweepSignal) -> Self {
        Self::with(sweep, Options::default(), None, None, analysis::standard_grid())
    }

    pub fn with(
        sweep: SweepSignal,
        options: Options,
        mic_calibration: Option<MicCalibration>,
        coupler: Option<CouplerCorrection>,
        grid: Vec<f64>,
    ) -> Self {
        Self {
            sweep,
            options,
            mic_calibration,
            coupler,
            grid,
            saw_clipping: false,
            deconvolver: None,
            responses: Vec::new(),
            summaries: Vec::new(),
            distortions: Vec::new(),
            window_pre: 0,
            window_post: 0,
            noise_segment: Vec::new(),
            truncated_run: false,
        }
    }

    /// True when a recording reached ±0.999 at the input.
    pub fn saw_clipping(&self) -> bool {
        self.saw_clipping
    }

    pub fn run_count(&self) -> usize {
        self.responses.len()
    }

    pub fn run_summaries(&self) -> &[RunSummary] {
        &self.summaries
    }

    /// Gives a recording made with nothing playing. Without it there is no
    /// signal-to-noise figure per band, and `MeasurementQuality::snr_db` is empty.
    ///
    /// Make it at least as long as the sweep. The regularized inverse is a
    /// filter as long as the sweep, so a short burst of silence comes out
    /// thinner than the noise that ran under the whole measurement.
    pub fn set_noise_segment(&mut self, noise: Vec<f64>) {
        self.noise_segment = noise;
    }

    /// Analyses one recording of the sweep and adds it to the average.
    ///
    /// The recording may start at any time. The delay comes out of the deconvolution.
    pub fn add_run(&mut self, recorded: &[f64], measure_cross_correlation: bool) -> RunSummary {
        let clipped = recorded.iter().any(|v| v.abs() >= 0.999);
        if clipped {
            self.saw_clipping = true;
        }

        let deconvolver = self
            .deconvolver
            .get_or_insert_with(|| SweepDeconvolver::new(&self.sweep, recorded.len(), &self.options));
        let mut run = recorded;
        if run.len() > deconvolver.length {
            run = &run[..deconvolver.length];
            self.truncated_run = true;
        }

        let ir = deconvolver.impulse_response(run);
        if self.responses.is_empty() {
            let adaptive = analysis::window(&ir, &self.options);
            self.window_pre = adaptive.pre_samples;
            self.window_post = adaptive.post_samples;
        }
        let windowed = analysis::window_fixed(
            &ir,
            self.window_pre,
            self.window_post,
            self.options.post_window_fade_fraction,
            None,
        );
        self.responses
            .push(analysis::complex_response(&windowed, &self.options));

        let distortion = analysis::harmonic_distortion(&ir, &self.sweep, &self.options);
        self.distortions.push(distortion.total_percent);

        let correlation_delay =
            measure_cross_correlation.then(|| analysis::delay_samples(run, &self.sweep.samples).samples);

        let summary = RunSummary {
            delay_samples: ir.peak_index,
            delay_seconds: ir.delay_seconds(),
            cross_correlation_delay_samples: correlation_delay,
            peak_level_db: amplitude_db(ir.samples[ir.peak_index].abs()),
            noise_floor_db: ir.noise_floor_db_relative_peak,
            thd_percent: distortion.total_percent,
            clipped,
        };
        self.summaries.push(summary.clone());
        summary
    }

    /// The measured curve, its quality, and (when the context is given and an
    /// absolute level exists) the sensitivity of the headphone.
    ///
    /// The macOS default for `source` is "Joseon measurement". `None` when no
    /// run was added.
    pub fn result(&self, name: &str, source: &str, context: Option<&SensitivityContext>) -> Option<MeasuredHeadphone> {
        let deconvolver = self.deconvolver.as_ref()?;
        if self.responses.is_empty() {
            return None;
        }
        let grid = &self.grid;
        let smoothing = self.options.smoothing;
        let averaged = analysis::average(&self.responses, grid, smoothing);
        let mut magnitude = averaged.mean.magnitude_db(grid, smoothing);

        if let Some(mic) = &self.mic_calibration {
            magnitude = mic.apply(&magnitude, grid);
        }
        if let Some(coupler) = &self.coupler {
            magnitude = coupler.apply(&magnitude, grid);
        }

        // The top of the log grid is not the top of the measurement. Hold the
        // curve flat above the last frequency the stimulus drove at full level.
        let upper_limit = self.highest_reliable_hz();
        self.hold_flat(&mut magnitude, upper_limit);

        // Signal to noise, through the same deconvolution and the same window.
        let mut snr: Vec<f32> = Vec::new();
        if !self.noise_segment.is_empty() {
            let noise_response = analysis::noise_floor_response(
                &deconvolver.deconvolve(&self.noise_segment),
                self.sweep.sample_rate,
                self.window_pre,
                self.window_post,
                self.noise_segment.len(),
                analysis::NOISE_SEGMENTS,
                &self.options,
            );
            // An average of N runs gives 10·log10(N) against uncorrelated noise.
            let averaging_gain = 10.0 * (self.responses.len() as f64).log10();
            let measured = analysis::signal_to_noise_db(&averaged.mean, &noise_response, grid, smoothing);
            let mut held: Vec<f64> = measured.iter().map(|v| ((v + averaging_gain) as f32) as f64).collect();
            self.hold_flat(&mut held, upper_limit);
            snr = held.iter().map(|&v| v as f32).collect();
        }

        let reference = self.reference_level(&magnitude);
        let normalized: Vec<f32> = magnitude.iter().map(|v| (v - reference) as f32).collect();
        let window_seconds = (self.window_pre + self.window_post) as f64 / self.sweep.sample_rate;
        let thd = if self.distortions.is_empty() {
            0.0
        } else {
            self.distortions.iter().sum::<f64>() / self.distortions.len() as f64
        };

        let mut quality = MeasurementQuality {
            frequencies_hz: grid.iter().map(|&f| f as f32).collect(),
            snr_db: snr,
            thd_percent: thd as f32,
            runs: self.responses.len(),
            agreement_db: averaged.agreement_db as f32,
            agreement_spread_db: averaged.spread_db.iter().map(|&v| v as f32).collect(),
            lowest_resolved_hz: (1.0 / window_seconds) as f32,
            window_milliseconds: (window_seconds * 1000.0) as f32,
            warnings: Vec::new(),
            noise_limited_hz: 0.0,
            highest_reliable_hz: upper_limit as f32,
        };
        quality.noise_limited_hz = self.noise_limited_hz(&quality);
        let normalized_f64: Vec<f64> = normalized.iter().map(|&v| v as f64).collect();
        quality.warnings = self.warnings(&quality, context, &normalized_f64);

        let method = self.method_sentence(context);
        let derived = context.and_then(|context| {
            derive_sensitivity(self.derivation(context, reference, &quality, &method))
                .value()
                .cloned()
        });

        Some(MeasuredHeadphone {
            curve: HeadphoneCurve {
                name: name.to_string(),
                source: source.to_string(),
                frequencies_hz: grid.iter().map(|&f| f as f32).collect(),
                levels_db: normalized,
            },
            absolute_magnitude_db: magnitude.iter().map(|&v| v as f32).collect(),
            sensitivity: derived.as_ref().map(|d| d.sensitivity.clone()),
            derived_sensitivity: derived,
            quality,
            method,
        })
    }

    /// Only the sensitivity, for a UI that derives it after the curve is on screen.
    pub fn derived_sensitivity(&self, context: &SensitivityContext) -> DerivedSensitivityResult {
        let Some(measured) = self.result("sensitivity", "Joseon measurement", None) else {
            return DerivedSensitivityResult::Unavailable {
                reason: "No runs have been measured yet.".to_string(),
            };
        };
        let magnitude: Vec<f64> = measured.absolute_magnitude_db.iter().map(|&v| v as f64).collect();
        let reference = self.reference_level(&magnitude);
        derive_sensitivity(self.derivation(
            context,
            reference,
            &measured.quality,
            &self.method_sentence(Some(context)),
        ))
    }

    // MARK: Private

    fn derivation<'a>(
        &self,
        context: &'a SensitivityContext,
        reference: f64,
        quality: &MeasurementQuality,
        method: &str,
    ) -> Derivation<'a> {
        Derivation {
            magnitude_at_1khz_db: reference,
            absolute_level: &context.absolute_level,
            playback: &context.playback,
            sweep_level_dbfs: self.sweep.level_dbfs,
            impedance_ohms: context.impedance_ohms,
            drive_voltage_uncertainty_db: context.drive_voltage_uncertainty_db,
            measurement_uncertainty_db: Self::repeatability_db(quality),
            mic_calibration_uncertainty_db: if self.mic_calibration.is_none() { 2.0 } else { 0.5 },
            rig: context.rig,
            source: format!("Measured with Joseon — {method}"),
        }
    }

    /// Mean level over 800–1250 Hz: the same reference `HeadphoneCurve` normalizes to.
    fn reference_level(&self, magnitude: &[f64]) -> f64 {
        let (mut sum, mut used) = (0.0, 0usize);
        for (i, &f) in self.grid.iter().enumerate() {
            if f >= 800.0 && f <= 1250.0 {
                sum += magnitude[i];
                used += 1;
            }
        }
        if used == 0 {
            return 0.0;
        }
        sum / used as f64
    }

    /// The highest frequency this result may claim: the last frequency the
    /// stimulus drove at full level, inside the grid and under Nyquist.
    fn highest_reliable_hz(&self) -> f64 {
        let grid_top = self.grid.last().copied().unwrap_or(self.sweep.end_hz);
        self.sweep
            .highest_full_energy_hz()
            .min(grid_top)
            .max(self.grid.first().copied().unwrap_or(20.0))
    }

    /// Holds a curve at its last good value above `hz`.
    fn hold_flat(&self, values: &mut [f64], hz: f64) {
        if values.len() != self.grid.len() {
            return;
        }
        let mut last: Option<f64> = None;
        for (i, &f) in self.grid.iter().enumerate() {
            if f <= hz {
                last = Some(values[i]);
            } else if let Some(last) = last {
                values[i] = last;
            }
        }
    }

    /// Lowest frequency with 10 dB of signal-to-noise, or 0 when noise does not limit anything.
    ///
    /// 0 means "noise is not the limit here": no noise-only segment was
    /// recorded, or the signal-to-noise ratio is already good at the bottom of
    /// the grid. When nothing on the grid reaches 10 dB, the limit is the top
    /// of the grid: none of this curve is signal.
    fn noise_limited_hz(&self, quality: &MeasurementQuality) -> f32 {
        if quality.snr_db.is_empty() {
            return 0.0;
        }
        let Some(trusted) = quality.lowest_trusted_hz(10.0) else {
            return self.grid.last().copied().unwrap_or(20_000.0) as f32;
        };
        match self.grid.first() {
            Some(&bottom) if trusted > bottom as f32 => trusted,
            _ => 0.0,
        }
    }

    /// How repeatable the measurement was, in dB: the run spread cut down by the average.
    fn repeatability_db(quality: &MeasurementQuality) -> f64 {
        if quality.runs <= 1 {
            return 1.0;
        }
        (quality.agreement_db as f64 / (quality.runs as f64).sqrt()).max(0.2)
    }

    fn method_sentence(&self, context: Option<&SensitivityContext>) -> String {
        let sweep = &self.sweep;
        let runs = self.responses.len();
        let mut parts = vec![
            if sweep.kind == SweepKind::ExponentialSweep {
                format!(
                    "{} s exponential sweep, {}–{} kHz at {} dBFS",
                    fixed(sweep.duration_seconds, 0),
                    fixed(sweep.start_hz, 0),
                    fixed(sweep.end_hz / 1000.0, 0),
                    fixed(sweep.level_dbfs, 0)
                )
            } else {
                format!(
                    "periodic pink noise, {} s period at {} dBFS",
                    fixed(sweep.duration_seconds, 0),
                    fixed(sweep.level_dbfs, 0)
                )
            },
            format!("{runs} run{} averaged", if runs == 1 { "" } else { "s" }),
            if self.options.smoothing == OctaveSmoothing::None {
                "no smoothing".to_string()
            } else {
                format!("{} smoothing", self.options.smoothing.as_str())
            },
            match &self.mic_calibration {
                Some(mic) => format!("microphone calibration \"{}\" applied", mic.name),
                None => "no microphone calibration".to_string(),
            },
        ];
        if let Some(coupler) = &self.coupler {
            parts.push(format!("coupler correction \"{}\" applied", coupler.name));
        }
        if let Some(context) = context {
            parts.push(format!("rig: {}", context.rig.label()));
            if let AbsoluteLevel::Available(scale) = &context.absolute_level {
                parts.push(format!("absolute level from {}", scale.method));
            }
        }
        parts.join("; ")
    }

    fn warnings(
        &self,
        quality: &MeasurementQuality,
        context: Option<&SensitivityContext>,
        normalized_levels_db: &[f64],
    ) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();

        match &self.mic_calibration {
            None => out.push("No microphone calibration was applied, so this curve is the headphone and the microphone together.".to_string()),
            Some(mic) => out.push(format!("Microphone calibration \"{}\" applied. Joseon can not tell a 0° file from a 90° one — use the file that matches how the microphone points.", mic.name)),
        }
        if self.coupler.is_none() {
            out.push("No coupler correction. On a flat plate or a home-made coupler the bass and the ear-canal resonance are the rig's, not the headphone's; a standards-type ear simulator needs no correction.".to_string());
        }
        if quality.runs == 1 {
            out.push("One run only, so there is no run-to-run agreement to report. Measure at least three times, lifting and reseating the headphone between runs.".to_string());
        } else if quality.agreement_db > 1.0 {
            out.push(format!("Runs disagree by {} dB on average between 100 Hz and 10 kHz. The headphone probably moved on the rig; the bass seal is the usual cause.", fixed(quality.agreement_db as f64, 1)));
        }
        if quality.snr_db.is_empty() {
            out.push("No noise-only segment was recorded, so the signal-to-noise ratio is unknown and nothing here says which parts of the curve are real.".to_string());
        } else if self.noise_segment.len() < self.sweep.samples.len() {
            out.push("The noise-only recording is shorter than the sweep, so the signal-to-noise figures are optimistic. Record silence for at least as long as the sweep.".to_string());
        }
        if self.saw_clipping {
            out.push("The input clipped (a sample reached ±0.999). The result is not trustworthy — lower the input gain and measure again.".to_string());
        }
        if quality.thd_percent > 3.0 {
            out.push(format!("Harmonic distortion is {} %. Some of that is the amplifier and the microphone, not the headphone; measure again 10 dB lower and see whether it follows the level.", fixed(quality.thd_percent as f64, 1)));
        }

        // One lower limit, with the one cause that set it.
        let lowest = fixed(quality.lowest_reliable_hz() as f64, 0);
        match quality.low_limit_cause() {
            LowLimitCause::Window => out.push(format!("Nothing under {lowest} Hz is the headphone: the {} ms analysis window sets the bottom of this measurement.", fixed(quality.window_milliseconds as f64, 0))),
            LowLimitCause::Noise => out.push(format!("Nothing under {lowest} Hz is the headphone: signal-to-noise falls under 10 dB there, so room noise sets the bottom of this measurement. Measure again in a quieter room, or at a higher level if there is headroom.")),
        }

        // The top: where the stimulus stopped driving at full level.
        let top = quality.highest_reliable_hz as f64;
        if let Some(&grid_top) = self.grid.last() {
            if top < grid_top * 0.999 {
                out.push(format!("The stimulus drove at full level only up to {} Hz (the fade-out covers the frequencies above it), so the curve is held flat from there to {} Hz rather than drawn from data.", fixed(top, 0), fixed(grid_top, 0)));
            }
        }

        if let Some(note) = MeasuredHeadphone::bass_roll_off_note_for(
            normalized_levels_db,
            &self.grid,
            quality.snr_db_at(40.0).map(f64::from),
            10.0,
            MeasuredHeadphone::SNR_THRESHOLD_DB,
        ) {
            out.push(note);
        }

        if self.truncated_run {
            out.push("A recording was longer than the analysis buffer and was truncated. Stop the recording within a second of the end of the sweep.".to_string());
        }
        if self.sweep.kind == SweepKind::PeriodicPinkNoise {
            out.push("Pink noise can not separate distortion from the response, so the distortion figure is zero by construction rather than by measurement.".to_string());
        }
        match context {
            Some(context) => {
                if let AbsoluteLevel::Unavailable { reason } = &context.absolute_level {
                    out.push(format!("No sensitivity was derived. {reason}"));
                }
            }
            None => out.push(
                "No sensitivity was derived: no playback calibration and no absolute level reference were given."
                    .to_string(),
            ),
        }
        out.push("Clock drift between the playback device and the recording device is not corrected. Two devices at 20 ppm move about 5 samples over a 5 s sweep, which costs a fraction of a dB in the top octave; a much larger drift shows up as a soft top end.".to_string());
        out
    }
}
