//! Tests of the headphones crate, ported from `Tests/JoseonHeadphonesTests` in the macOS repo.

mod curves;
mod model;
mod sensitivity;
mod spl_diffuse_field;
mod spl_dose;
mod spl_end_to_end;
mod spl_estimator;
mod spl_support;

use crate::curve::HeadphoneCurve;
use joseon_core::contracts::{BandEnergy, LoudnessReading, SpectrumReading, StressFlag};
use std::sync::{Arc, Mutex};

// Synthetic fixtures. These are test signals, never presented as measurements.

/// A log-spaced display grid like the one the spectrum analyzer makes, 10 Hz to 24 kHz.
pub(crate) fn grid(bins: usize) -> Vec<f32> {
    let n = bins.max(2);
    (0..n).map(|i| 10.0 * 2400f32.powf(i as f32 / (n - 1) as f32)).collect()
}

/// A spectrum whose `mid` is `level(hz)` and whose other channels copy it.
pub(crate) fn spectrum(bins: usize, level: impl Fn(f32) -> f32) -> SpectrumReading {
    let frequencies = grid(bins);
    let mid: Vec<f32> = frequencies.iter().map(|&hz| level(hz)).collect();
    SpectrumReading {
        frequencies,
        left: mid.clone(),
        right: mid.clone(),
        side: mid.clone(),
        peak_hold: mid.clone(),
        average: mid.clone(),
        mid,
        mid_layers: Vec::new(),
    }
}

/// Digital silence on every bin.
pub(crate) fn silent_spectrum(bins: usize) -> SpectrumReading {
    spectrum(bins, |_| SpectrumReading::FLOOR_DB)
}

/// A curve that is `level(hz)` on a 200-point log grid from 20 Hz to 20 kHz.
pub(crate) fn curve(name: &str, level: impl Fn(f32) -> f32) -> HeadphoneCurve {
    let frequencies_hz: Vec<f32> = (0..200).map(|i| 20.0 * 1_000f32.powf(i as f32 / 199.0)).collect();
    HeadphoneCurve {
        name: name.to_string(),
        source: "synthetic test fixture".to_string(),
        levels_db: frequencies_hz.iter().map(|&hz| level(hz)).collect(),
        frequencies_hz,
    }
}

pub(crate) fn flat_curve(name: &str) -> HeadphoneCurve {
    curve(name, |_| 0.0)
}

/// A loudness reading with only the numbers the flags read.
#[derive(Clone, Copy)]
pub(crate) struct Loud {
    pub true_peak_max: f32,
    pub plr: f32,
    pub clip_count: u64,
    pub measured_seconds: f64,
    pub integrated: f32,
}

impl Default for Loud {
    fn default() -> Self {
        Self {
            true_peak_max: -120.0,
            plr: 0.0,
            clip_count: 0,
            measured_seconds: 0.0,
            integrated: -120.0,
        }
    }
}

impl Loud {
    pub fn reading(self) -> LoudnessReading {
        LoudnessReading {
            true_peak_max_dbtp: self.true_peak_max,
            plr_db: self.plr,
            clip_count: self.clip_count,
            measured_seconds: self.measured_seconds,
            integrated_lufs: self.integrated,
            is_integrated_valid: self.integrated > -119.0,
            ..Default::default()
        }
    }
}

pub(crate) fn quiet_loudness() -> LoudnessReading {
    Loud::default().reading()
}

pub(crate) fn bands(sub_bass: f32) -> BandEnergy {
    BandEnergy {
        sub_bass,
        ..Default::default()
    }
}

/// A clock the test moves by hand.
#[derive(Clone, Default)]
pub(crate) struct TestClock(Arc<Mutex<f64>>);

impl TestClock {
    pub fn reader(&self) -> crate::stress::MonotonicClock {
        let time = Arc::clone(&self.0);
        Box::new(move || *time.lock().unwrap())
    }

    pub fn advance(&self, seconds: f64) {
        *self.0.lock().unwrap() += seconds;
    }

    pub fn now(&self) -> f64 {
        *self.0.lock().unwrap()
    }

    /// Holds a condition long enough for a level flag to go up: evaluate, move
    /// past the attack time, evaluate again. Level flags need 3 s of a true
    /// condition before they appear.
    pub fn settled(&self, evaluate: impl FnMut() -> Vec<StressFlag>) -> Vec<StressFlag> {
        self.settled_for(3.01, evaluate)
    }

    /// The same with another wait: 5.01 s is past the release time of a level flag.
    pub fn settled_for(&self, seconds: f64, mut evaluate: impl FnMut() -> Vec<StressFlag>) -> Vec<StressFlag> {
        evaluate();
        self.advance(seconds);
        evaluate()
    }
}

#[track_caller]
pub(crate) fn assert_near(actual: f32, expected: f32, accuracy: f32, what: &str) {
    assert!(
        (actual - expected).abs() <= accuracy,
        "{what}: {actual}, expected {expected} ± {accuracy}"
    );
}
