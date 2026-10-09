//! Sound level at the ear, from third-octave band levels plus a calibration and a headphone.
//!
//! Port source: `Sources/JoseonHeadphones/SPLEstimator.swift` in the macOS repo.
//!
//! Chain per band `b` and channel:
//!
//! ```text
//! eardrum_b = L_b + 3.01 + 20·log10(full_scale_vrms) + db_spl_per_volt + response(f_b)
//! ```
//!
//! `L_b` is dBFS RMS, where a full-scale sine reads −3.01. `response` is the
//! headphone curve normalized to 0 dB over 800–1250 Hz and averaged over the
//! band in the dB domain. The diffuse-field equivalent per band is
//! `eardrum_b − DF(f_b)`, then A-weighted (IEC 61672) at the nominal center and
//! summed as energy.
//!
//! Threading: the macOS class has its own lock. Here the estimator is a plain
//! value, and the app shares it as `Arc<Mutex<SplEstimator>>` (the engine takes
//! that as `SharedSplEstimator`). A change of curve and sensitivity made under
//! one lock is one atomic change for the analysis thread.
//!
//! Every number this type gives is an estimate. `SplReading::uncertainty_db`
//! carries the uncertainty of the calibration. The UI must show it.

use crate::curve::{CurveInterpolator, HeadphoneCurve};
use crate::dose::{LocalTime, SplDoseOptions, SplDoseState};
use crate::spl_weighting::{a_weighting, diffuse_field_reference, third_octave_bands};
use joseon_core::contracts::{
    HeadphoneSensitivity, PlaybackCalibration, SplEstimating, SplReading, ThirdOctaveReading,
};

/// The wall clock that stamps the dose with a day and a week.
pub type WallClock = Box<dyn Fn() -> LocalTime + Send>;

pub struct SplEstimator {
    curve: HeadphoneCurve,
    sensitivity: HeadphoneSensitivity,
    calibration: PlaybackCalibration,
    options: SplDoseOptions,

    // Made again when the curve or the band centers change.
    cached_centers: Vec<f32>,
    cached_response_db: Vec<f64>,
    cached_diffuse_db: Vec<f64>,
    cached_a_weight_db: Vec<f64>,
    cache_is_stale: bool,
    left_eardrum: Vec<f64>,
    right_eardrum: Vec<f64>,

    /// 1 s exponential averages, kept as linear energy (10^(L/10)) so the filter is on power.
    slow_a_energy: f64,
    slow_z_energy: f64,

    /// Track Leq accumulators. `reset_measurement` clears them.
    track_energy_seconds: f64,
    track_seconds: f64,
    max_a_fast: f64,

    /// Dose and session accumulators. `reset_dose` clears them. `dose_state` gives them to the app.
    dose: SplDoseState,
    now: WallClock,
}

impl SplEstimator {
    /// Time constant of the slow weighting, IEC 61672 "S".
    pub const SLOW_TIME_CONSTANT_SECONDS: f64 = 1.0;

    /// An estimator with the default dose options and the system clock in UTC.
    pub fn new(curve: HeadphoneCurve, sensitivity: HeadphoneSensitivity, calibration: PlaybackCalibration) -> Self {
        Self::with(
            curve,
            sensitivity,
            calibration,
            SplDoseOptions::default(),
            Box::new(LocalTime::now_utc),
        )
    }

    /// `now` is the wall clock. The app gives one with the local UTC offset; tests give a fixed one.
    pub fn with(
        curve: HeadphoneCurve,
        sensitivity: HeadphoneSensitivity,
        calibration: PlaybackCalibration,
        options: SplDoseOptions,
        now: WallClock,
    ) -> Self {
        let dose = SplDoseState::empty(now());
        Self {
            curve: curve.normalized_to_1khz(),
            sensitivity,
            calibration,
            options,
            cached_centers: Vec::new(),
            cached_response_db: Vec::new(),
            cached_diffuse_db: Vec::new(),
            cached_a_weight_db: Vec::new(),
            cache_is_stale: true,
            left_eardrum: Vec::new(),
            right_eardrum: Vec::new(),
            slow_a_energy: 0.0,
            slow_z_energy: 0.0,
            track_energy_seconds: 0.0,
            track_seconds: 0.0,
            max_a_fast: f64::NEG_INFINITY,
            dose,
            now,
        }
    }

    // MARK: Live configuration

    /// Replaces the headphone curve. The next frame already shows it.
    pub fn set_curve(&mut self, curve: &HeadphoneCurve) {
        self.curve = curve.normalized_to_1khz();
        self.cache_is_stale = true;
    }

    pub fn set_sensitivity(&mut self, sensitivity: HeadphoneSensitivity) {
        self.sensitivity = sensitivity;
    }

    pub fn set_calibration(&mut self, calibration: PlaybackCalibration) {
        self.calibration = calibration;
    }

    pub fn set_options(&mut self, options: SplDoseOptions) {
        self.options = options;
    }

    /// The headphone curve in use, normalized to 0 dB over 800–1250 Hz.
    pub fn normalized_curve(&self) -> &HeadphoneCurve {
        &self.curve
    }

    pub fn sensitivity(&self) -> &HeadphoneSensitivity {
        &self.sensitivity
    }

    pub fn calibration(&self) -> &PlaybackCalibration {
        &self.calibration
    }

    pub fn options(&self) -> SplDoseOptions {
        self.options
    }

    // MARK: Inspection helpers (for the calibration UI and for tests)

    /// The diffuse-field term subtracted from the eardrum level in the band at `center_hz`.
    pub fn diffuse_field_term_db(center_hz: f64) -> f64 {
        diffuse_field_reference::band_term_db(center_hz)
    }

    /// The constant part of the chain: `3.01 + 20·log10(full_scale_vrms) + db_spl_per_volt`.
    pub fn chain_offset_db(&self) -> f64 {
        // A calibration of zero volts is not a level, it is "no signal path".
        // The clamp makes a wrong calibration read as silence, not as NaN.
        let volts = self.calibration.full_scale_vrms.max(1e-12);
        3.01 + 20.0 * volts.log10() + self.sensitivity.db_spl_per_volt
    }

    fn refresh_cache_if_needed(&mut self, centers: &[f32]) {
        if !self.cache_is_stale && self.cached_centers == centers {
            return;
        }
        self.cached_centers = centers.to_vec();
        let interpolator = CurveInterpolator::new(self.curve.clone());
        self.cached_response_db = centers
            .iter()
            .map(|&hz| third_octave_bands::mean_db(&interpolator, hz as f64))
            .collect();
        self.cached_diffuse_db = centers
            .iter()
            .map(|&hz| Self::diffuse_field_term_db(hz as f64))
            .collect();
        self.cached_a_weight_db = a_weighting::db_on(centers);
        self.cache_is_stale = false;
    }

    // MARK: Dose persistence

    /// A snapshot the app can encode and write. The estimator never touches the disk.
    pub fn dose_state(&self) -> SplDoseState {
        self.dose.clone()
    }

    /// Puts a saved snapshot back. Call `SplDoseState::rolled` first to handle a
    /// day or week boundary. This method stores exactly what it gets.
    pub fn restore(&mut self, state: SplDoseState) {
        self.dose = state;
    }

    /// Time left at a steady `level_dba`, in seconds, before the NIOSH dose reaches 100 %.
    /// Infinite when the level can never reach the limit (under the threshold, or silence).
    pub fn seconds_to_niosh_limit(&self, level_dba: f64) -> f64 {
        if !level_dba.is_finite() {
            return f64::INFINITY;
        }
        if self.options.apply_niosh_threshold && level_dba < SplDoseOptions::NIOSH_THRESHOLD_DBA {
            return f64::INFINITY;
        }
        let remaining = 1.0 - self.dose.niosh_dose;
        if remaining <= 0.0 {
            return 0.0;
        }
        let allowed = SplDoseOptions::niosh_allowed_seconds(level_dba);
        if !allowed.is_finite() {
            return f64::INFINITY;
        }
        remaining * allowed
    }

    fn accumulate(&mut self, level: f64, dt: f64) {
        if !level.is_finite() {
            return;
        }
        let energy = energy(level);
        self.track_energy_seconds += dt * energy;
        self.track_seconds += dt;
        self.dose.session_energy_seconds += dt * energy;
        self.dose.session_seconds += dt;
        self.dose.dose_seconds += dt;

        // NIOSH: dose = ∫ dt / T(L), T(L) = 8 h · 2^((85 − L)/3), with an 80 dBA threshold.
        if !self.options.apply_niosh_threshold || level >= SplDoseOptions::NIOSH_THRESHOLD_DBA {
            let allowed = SplDoseOptions::niosh_allowed_seconds(level);
            if allowed > 0.0 && allowed.is_finite() {
                self.dose.niosh_dose += dt / allowed;
            }
        }

        // WHO / ITU-T H.870 adult weekly allowance: Σ dt · 10^((L − 80)/10), over 40 h.
        // H.870 states no threshold level, so every frame with signal counts.
        self.dose.who_weekly_energy_seconds += dt * 10.0f64.powf((level - SplDoseOptions::WHO_CRITERION_DBA) / 10.0);
    }
}

impl SplEstimating for SplEstimator {
    fn evaluate(&mut self, third_octave: &ThirdOctaveReading, dt: f64, is_silent: bool) -> SplReading {
        let centers = &third_octave.centers_hz;
        self.refresh_cache_if_needed(centers);
        let offset = self.chain_offset_db();
        let band_count = centers.len().min(third_octave.left.len()).min(third_octave.right.len());

        // Band energies per channel. The louder ear is decided per frame, by the A-weighted total.
        self.left_eardrum.clear();
        self.right_eardrum.clear();
        let (mut left_a, mut right_a) = (0.0f64, 0.0f64);
        let (mut left_z, mut right_z) = (0.0f64, 0.0f64);
        for b in 0..band_count {
            let response = self.cached_response_db[b];
            let diffuse = self.cached_diffuse_db[b];
            let weight = self.cached_a_weight_db[b];
            let el = third_octave.left[b] as f64 + offset + response;
            let er = third_octave.right[b] as f64 + offset + response;
            self.left_eardrum.push(el);
            self.right_eardrum.push(er);
            left_z += energy(el);
            right_z += energy(er);
            left_a += energy(el - diffuse + weight);
            right_a += energy(er - diffuse + weight);
        }

        let left_is_louder = left_a >= right_a;
        let louder_a = if left_is_louder { left_a } else { right_a };
        let louder_z = if left_is_louder { left_z } else { right_z };
        let level_a_fast = level(louder_a);

        // Time weightings. The bands that come in are already "fast" (125 ms).
        // "Slow" is a 1 s exponential average on A-weighted energy. The slow
        // filter moves on every frame that carried audio time, silent or not,
        // so the meter falls through digital silence as a real meter does.
        if dt > 0.0 {
            let alpha = 1.0 - (-dt / Self::SLOW_TIME_CONSTANT_SECONDS).exp();
            self.slow_a_energy += alpha * (louder_a - self.slow_a_energy);
            self.slow_z_energy += alpha * (louder_z - self.slow_z_energy);
        }
        let level_a_slow = level(self.slow_a_energy);

        if level_a_fast > self.max_a_fast {
            self.max_a_fast = level_a_fast;
        }

        // Leq and dose count audio time that carried signal. Digital silence is
        // skipped, and so is a frame with no audio time. Quiet music DOES count.
        if dt > 0.0 && !is_silent {
            self.accumulate(level_a_fast, dt);
        }

        let floor = SplReading::FLOOR_DB as f64;
        let louder_bands = if left_is_louder {
            &self.left_eardrum
        } else {
            &self.right_eardrum
        };
        let mut band_levels = vec![SplReading::FLOOR_DB; centers.len()];
        for b in 0..band_count {
            band_levels[b] = louder_bands[b].max(floor) as f32;
        }

        SplReading {
            calibration_name: self.calibration.name.clone(),
            uncertainty_db: self.calibration.uncertainty_db as f32,
            level_a_fast: reportable(level_a_fast),
            level_a_slow: reportable(level_a_slow),
            level_z_eardrum: reportable(level(self.slow_z_energy)),
            leq_a_track: reportable(mean(self.track_energy_seconds, self.track_seconds)),
            leq_a_session: reportable(mean(self.dose.session_energy_seconds, self.dose.session_seconds)),
            max_a_fast: reportable(self.max_a_fast),
            band_levels_eardrum: band_levels,
            dose_niosh: self.dose.niosh_dose as f32,
            dose_who_weekly: self.dose.who_weekly_dose() as f32,
            dose_seconds: self.dose.dose_seconds,
            seconds_to_niosh_limit: self.seconds_to_niosh_limit(level_a_slow),
        }
    }

    /// New track: clears `leq_a_track` and `max_a_fast`. The dose keeps running.
    fn reset_measurement(&mut self) {
        self.track_energy_seconds = 0.0;
        self.track_seconds = 0.0;
        self.max_a_fast = f64::NEG_INFINITY;
    }

    /// Clears the dose and the session Leq. The day and week stamps move to now.
    fn reset_dose(&mut self) {
        self.dose = SplDoseState::empty((self.now)());
    }
}

fn energy(db: f64) -> f64 {
    if db.is_finite() {
        10.0f64.powf(db / 10.0)
    } else {
        0.0
    }
}

fn level(energy: f64) -> f64 {
    if energy > 0.0 {
        10.0 * energy.log10()
    } else {
        f64::NEG_INFINITY
    }
}

fn mean(energy_seconds: f64, seconds: f64) -> f64 {
    if seconds > 0.0 {
        level(energy_seconds / seconds)
    } else {
        f64::NEG_INFINITY
    }
}

/// Clamps to the floor of the contract, so the UI never gets −inf or a negative dB SPL.
fn reportable(db: f64) -> f32 {
    if !db.is_finite() {
        return SplReading::FLOOR_DB;
    }
    db.max(SplReading::FLOOR_DB as f64) as f32
}
