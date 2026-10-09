//! Headphone measurement: the sweep, its analysis, and the result.
//!
//! Port source: `Sources/JoseonHeadphones/Measurement` in the macOS repo.
//! This is the math only. The sound output and the microphone input are in the
//! app (port step 15).

pub mod absolute_level;
pub(crate) mod fft;
pub mod measured;
pub mod mic_calibration;
pub mod session;
pub mod sweep_analysis;
pub mod sweep_signal;

pub use absolute_level::{
    derive_sensitivity, drive_voltage_uncertainty_db, AbsoluteLevel, Component, ComponentKind, Derivation,
    DerivedSensitivity, DerivedSensitivityResult, MeasurementRig, Reference, Route, Scale,
};
pub use measured::{BassShortfall, LowLimitCause, MeasuredHeadphone, MeasurementQuality};
pub use mic_calibration::{CouplerCorrection, MicCalibration};
pub use session::{HeadphoneMeasurement, RunSummary, SensitivityContext};
pub use sweep_analysis::{OctaveSmoothing, Options};
pub use sweep_signal::{PinkNoiseOptions, SweepKind, SweepOptions, SweepSignal};

#[cfg(test)]
mod tests;
