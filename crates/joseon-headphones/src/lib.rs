//! L1. AutoEq curve parse, headphone library, headphone model, stress flags,
//! and the level at the ear (SPL estimate).
//!
//! No platform code lives here.
//!
//! Port source: `Sources/JoseonHeadphones` in the macOS repo. The sweep
//! measurement math (`Measurement/`) is in `measurement`; its sound input and
//! output are port step 15.

pub mod calibration;
pub mod curve;
pub mod dose;
pub mod embedded_curves;
pub mod library;
pub mod measurement;
pub mod model;
pub mod sensitivity;
pub mod spl_estimator;
pub mod spl_weighting;
pub mod stress;

#[cfg(test)]
mod tests;

pub use curve::{parse_autoeq_csv, CurveInterpolator, CurveParseError, HeadphoneCurve};
pub use dose::{LocalTime, SplDoseOptions, SplDoseState};
pub use library::HeadphoneLibrary;
pub use model::HeadphoneModel;
pub use spl_estimator::SplEstimator;
pub use stress::{StressContext, StressDetector, StressThresholds};
