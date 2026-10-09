//! One headphone curve (and an optional target) applied to the live spectrum,
//! plus the stress flags.
//!
//! Port source: `HeadphoneModel` in `Sources/JoseonHeadphones/HeadphoneModel.swift`.
//!
//! Port note: the Swift class has its own lock, because the engine and the app
//! reach it from two threads. Here the engine holds a model as
//! `Arc<Mutex<dyn HeadphoneModeling>>`, so that lock does the job.

use crate::curve::{CurveInterpolator, HeadphoneCurve};
use crate::stress::{MonotonicClock, StressContext, StressDetector, StressThresholds};
use joseon_core::contracts::{
    BandEnergy, HeadphoneModeling, HeadphoneReading, LoudnessReading, SpectrumReading, StressFlag,
};
use std::collections::HashMap;

pub struct HeadphoneModel {
    curve: HeadphoneCurve,
    target: Option<HeadphoneCurve>,
    /// Thresholds for the stress flags. Change them at any time; the detector picks them up on the next call.
    pub thresholds: StressThresholds,

    curve_interpolator: CurveInterpolator,
    target_interpolator: Option<CurveInterpolator>,
    detector: StressDetector,
    stress_context: StressContext,

    // Resample cache, keyed on the display frequency grid.
    cached_frequencies: Vec<f32>,
    cached_response_db: Vec<f32>,
    cached_target_db: Vec<f32>,
}

impl HeadphoneModel {
    /// A model with the default thresholds on the system clock.
    pub fn new(curve: HeadphoneCurve, target: Option<HeadphoneCurve>) -> Self {
        Self::with(
            curve,
            target,
            StressThresholds::default(),
            Box::new(joseon_core::engine::uptime),
        )
    }

    /// `now` gives monotonic seconds. Tests inject it to drive the flag hysteresis.
    pub fn with(
        curve: HeadphoneCurve,
        target: Option<HeadphoneCurve>,
        thresholds: StressThresholds,
        now: MonotonicClock,
    ) -> Self {
        let normalized_curve = curve.normalized_to_1khz();
        let normalized_target = target.as_ref().map(HeadphoneCurve::normalized_to_1khz);
        let target_interpolator = normalized_target.map(CurveInterpolator::new);
        let stress_context = StressContext::new(
            normalized_curve.name.clone(),
            target_interpolator.as_ref().map(|target| target.curve().name.clone()),
            normalized_curve.frequencies_hz.clone(),
            normalized_curve.levels_db.clone(),
            target_interpolator
                .as_ref()
                .map(|target| target.levels(&normalized_curve.frequencies_hz)),
        );
        Self {
            curve,
            target,
            thresholds,
            curve_interpolator: CurveInterpolator::new(normalized_curve),
            target_interpolator,
            detector: StressDetector::new(now),
            stress_context,
            cached_frequencies: Vec::new(),
            cached_response_db: Vec::new(),
            cached_target_db: Vec::new(),
        }
    }

    /// The curve as it was given.
    pub fn curve(&self) -> &HeadphoneCurve {
        &self.curve
    }

    /// The target as it was given.
    pub fn target(&self) -> Option<&HeadphoneCurve> {
        self.target.as_ref()
    }

    /// The curve normalized to 0 dB at 1 kHz, on its own frequency grid.
    pub fn normalized_curve(&self) -> &HeadphoneCurve {
        self.curve_interpolator.curve()
    }

    /// The target normalized to 0 dB at 1 kHz, on its own frequency grid.
    pub fn normalized_target(&self) -> Option<&HeadphoneCurve> {
        self.target_interpolator.as_ref().map(CurveInterpolator::curve)
    }

    /// Curve, target and names in the shape the detector wants. Built once.
    pub fn stress_context(&self) -> &StressContext {
        &self.stress_context
    }

    /// Headphone response in dB on `grid`, normalized to 0 dB at 1 kHz.
    pub fn response_db(&self, grid: &[f32]) -> Vec<f32> {
        self.curve_interpolator.levels(grid)
    }

    /// Target in dB on `grid`, or zeros when there is no target.
    pub fn target_db(&self, grid: &[f32]) -> Vec<f32> {
        match &self.target_interpolator {
            Some(target) => target.levels(grid),
            None => vec![0.0; grid.len()],
        }
    }

    /// Flags that describe what this music asks of this headphone and of the chain.
    ///
    /// Each flag has hysteresis. A level flag goes up only after its condition
    /// has held for `thresholds.level_attack_seconds`, and comes down only
    /// after its value has stayed `thresholds.release_margin_db` under the
    /// threshold for `thresholds.hold_seconds`. So a value that sits on the
    /// threshold gives one continuous flag or none. Event flags (clipped
    /// samples, inter-sample overs) latch until `reset_flags`.
    pub fn stress_flags(
        &mut self,
        spectrum: &SpectrumReading,
        bands: &BandEnergy,
        loudness: &LoudnessReading,
    ) -> Vec<StressFlag> {
        self.detector
            .evaluate(spectrum, bands, loudness, &self.thresholds, &self.stress_context)
    }

    /// Clock time each flag that is up now was raised, by flag id, on the clock
    /// the model was built with. A flag that is still up keeps its first onset,
    /// through every dip inside the hysteresis band, until it really clears or
    /// `reset_flags` runs.
    pub fn flag_onsets(&self) -> HashMap<String, f64> {
        self.detector.flag_onsets()
    }

    /// Drops the hysteresis state (new track, new measurement).
    pub fn reset_flags(&mut self) {
        self.detector.reset();
    }
}

impl HeadphoneModeling for HeadphoneModel {
    fn model_name(&self) -> &str {
        &self.curve.name
    }

    fn evaluate(
        &mut self,
        spectrum: &SpectrumReading,
        bands: &BandEnergy,
        loudness: &LoudnessReading,
    ) -> HeadphoneReading {
        let grid = &spectrum.frequencies;
        if self.cached_frequencies != *grid {
            self.cached_frequencies.clone_from(grid);
            self.cached_response_db = self.response_db(grid);
            self.cached_target_db = self.target_db(grid);
        }
        let mut predicted = vec![SpectrumReading::FLOOR_DB; grid.len()];
        for ((at_ear, mid), response) in predicted.iter_mut().zip(&spectrum.mid).zip(&self.cached_response_db) {
            *at_ear = mid + response;
        }
        HeadphoneReading {
            model_name: self.curve.name.clone(),
            response_db: self.cached_response_db.clone(),
            target_db: self.cached_target_db.clone(),
            has_target: self.target_interpolator.is_some(),
            predicted_at_ear_db: predicted,
            stress_flags: self.stress_flags(spectrum, bands, loudness),
        }
    }

    /// The engine calls this when the measurement resets.
    fn reset(&mut self) {
        self.reset_flags();
    }
}
