//! Synthetic third-octave readings, made by hand. Nothing here is a measurement.
//!
//! Ported from `Tests/JoseonHeadphonesTests/SPLTestSupport.swift`.

use crate::curve::HeadphoneCurve;
use crate::dose::{LocalTime, SplDoseOptions};
use crate::spl_estimator::SplEstimator;
use joseon_core::contracts::{
    CalibrationMethod, HeadphoneSensitivity, PlaybackCalibration, SplEstimating, SplReading, ThirdOctaveReading,
};

/// 2026-09-21 00:00 UTC, the fixed clock of the reference estimator.
pub(crate) const FIXTURE_CLOCK: f64 = 1_789_948_800.0;

/// Index of a nominal third-octave center, for example 1000 Hz -> 17.
pub(crate) fn band_index(center_hz: f32) -> usize {
    ThirdOctaveReading::NOMINAL_CENTERS_HZ
        .iter()
        .position(|&hz| hz == center_hz)
        .unwrap()
}

/// Every band at the reading floor (digital silence out of the analyzer).
pub(crate) fn floor_bands() -> Vec<f32> {
    vec![ThirdOctaveReading::FLOOR_DB; ThirdOctaveReading::NOMINAL_CENTERS_HZ.len()]
}

/// One band at `dbfs`, the rest at the floor, the same on both channels.
pub(crate) fn one_band(hz: f32, dbfs: f32) -> ThirdOctaveReading {
    let mut bands = floor_bands();
    bands[band_index(hz)] = dbfs;
    ThirdOctaveReading::new(bands.clone(), bands)
}

/// One band at `dbfs` on the left only; the right channel is at the floor.
pub(crate) fn left_only(hz: f32, dbfs: f32) -> ThirdOctaveReading {
    let mut left = floor_bands();
    left[band_index(hz)] = dbfs;
    ThirdOctaveReading::new(left, floor_bands())
}

pub(crate) fn silence() -> ThirdOctaveReading {
    ThirdOctaveReading::new(floor_bands(), floor_bands())
}

/// 100 dB SPL per volt, 300 Ω. A round number, not a real headphone.
pub(crate) fn sensitivity100() -> HeadphoneSensitivity {
    HeadphoneSensitivity {
        db_spl_per_volt: 100.0,
        impedance_ohms: 300.0,
        source: "synthetic test fixture".to_string(),
    }
}

/// `full_scale_vrms` = 1 makes the voltage term 0 dB.
pub(crate) fn calibration(full_scale_vrms: f64, uncertainty_db: f64) -> PlaybackCalibration {
    PlaybackCalibration {
        name: "test bench".to_string(),
        method: CalibrationMethod::MeasuredVoltage,
        full_scale_vrms,
        uncertainty_db,
    }
}

fn fixture_grid() -> Vec<f32> {
    (0..200).map(|i| 20.0 * 1_000f32.powf(i as f32 / 199.0)).collect()
}

/// A curve that is flat 0 dB everywhere.
pub(crate) fn flat_curve() -> HeadphoneCurve {
    plateau_curve(0.0, 0.0, 0.0).renamed("Flat test")
}

/// Flat 0 dB, with a +`boost_db` plateau from `low_hz` to `high_hz`.
pub(crate) fn plateau_curve(boost_db: f32, low_hz: f32, high_hz: f32) -> HeadphoneCurve {
    let frequencies_hz = fixture_grid();
    HeadphoneCurve {
        name: "Plateau test".to_string(),
        source: "synthetic test fixture".to_string(),
        levels_db: frequencies_hz
            .iter()
            .map(|&hz| if hz >= low_hz && hz <= high_hz { boost_db } else { 0.0 })
            .collect(),
        frequencies_hz,
    }
}

trait Renamed {
    fn renamed(self, name: &str) -> Self;
}

impl Renamed for HeadphoneCurve {
    fn renamed(mut self, name: &str) -> Self {
        self.name = name.to_string();
        self
    }
}

/// How to make the reference estimator. The default: flat curve, 100 dB/V, 1 V full scale.
pub(crate) struct Fixture {
    pub curve: Option<HeadphoneCurve>,
    pub full_scale_vrms: f64,
    pub options: SplDoseOptions,
    pub now: LocalTime,
}

impl Default for Fixture {
    fn default() -> Self {
        Self {
            curve: None,
            full_scale_vrms: 1.0,
            options: SplDoseOptions::default(),
            now: LocalTime::utc(FIXTURE_CLOCK),
        }
    }
}

impl Fixture {
    pub fn estimator(self) -> SplEstimator {
        let now = self.now;
        SplEstimator::with(
            self.curve.unwrap_or_else(flat_curve),
            sensitivity100(),
            calibration(self.full_scale_vrms, 2.0),
            self.options,
            Box::new(move || now),
        )
    }
}

/// The reference estimator.
pub(crate) fn estimator() -> SplEstimator {
    Fixture::default().estimator()
}

pub(crate) fn estimator_with_curve(curve: HeadphoneCurve) -> SplEstimator {
    Fixture {
        curve: Some(curve),
        ..Default::default()
    }
    .estimator()
}

/// The dBFS RMS a 1 kHz band needs so the reference chain reports exactly `target` dBA.
///
/// Solved from the chain itself, so the fixture stays correct whatever the
/// diffuse-field offset is. A(1 kHz) = 0 by definition.
pub(crate) fn dbfs_for(target_dba: f64) -> f32 {
    let chain = 3.01 + 20.0 * 1.0f64.log10() + sensitivity100().db_spl_per_volt;
    let df = SplEstimator::diffuse_field_term_db(1_000.0);
    (target_dba - chain + df) as f32
}

/// A 1 kHz reading that makes the reference chain report `dba`.
pub(crate) fn reading(dba: f64) -> ThirdOctaveReading {
    one_band(1_000.0, dbfs_for(dba))
}

/// Feeds a steady level for `seconds` of audio time in `frame_seconds` steps.
pub(crate) fn run(estimator: &mut SplEstimator, dba: f64, seconds: f64, frame_seconds: f64) -> SplReading {
    run_flagged(estimator, dba, seconds, frame_seconds, false)
}

pub(crate) fn run_flagged(
    estimator: &mut SplEstimator,
    dba: f64,
    seconds: f64,
    frame_seconds: f64,
    is_silent: bool,
) -> SplReading {
    let bands = reading(dba);
    let mut left = seconds;
    let mut last = estimator.evaluate(&bands, 0.0, is_silent);
    while left > 1e-9 {
        let dt = frame_seconds.min(left);
        last = estimator.evaluate(&bands, dt, is_silent);
        left -= dt;
    }
    last
}

/// One 125 ms frame with signal.
pub(crate) fn frame(estimator: &mut SplEstimator, bands: &ThirdOctaveReading) -> SplReading {
    estimator.evaluate(bands, 0.125, false)
}

#[track_caller]
pub(crate) fn near(actual: f64, expected: f64, accuracy: f64) {
    assert!(
        (actual - expected).abs() <= accuracy,
        "{actual}, expected {expected} ± {accuracy}"
    );
}
