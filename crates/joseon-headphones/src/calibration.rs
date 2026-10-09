//! How to make a playback calibration, and the sanity line of the calibration window.
//!
//! Port source: `Sources/JoseonHeadphones/PlaybackCalibration+Factories.swift`.
//!
//! Joseon can not see the amplifier gain. The whole SPL chain hangs off one
//! number: the RMS volts at the headphone for a full-scale sine.

use crate::curve::{CurveInterpolator, HeadphoneCurve};
use joseon_core::contracts::{CalibrationMethod, HeadphoneSensitivity, PlaybackCalibration};

// The table of voltage terms. One-sided, in dB. No other file states these numbers.

/// A multimeter on the test tone, with the headphones plugged in while measuring.
pub const MEASURED_TONE_UNCERTAINTY_DB: f64 = 2.0;
/// The same measurement WITHOUT the headphones plugged in: an amplifier with a
/// high output impedance gives less voltage into the load than into the meter alone.
pub const MEASURED_OPEN_CIRCUIT_UNCERTAINTY_DB: f64 = 3.0;
/// The volts come from data sheets, and the volume setting is known in dB.
pub const SPECS_UNCERTAINTY_DB: f64 = 4.0;
/// The volume attenuation is a guess as well: an analog knob with no scale.
pub const SPECS_WITH_GUESSED_ATTENUATION_UNCERTAINTY_DB: f64 = 6.0;
/// A known maximum output voltage, scaled with the system volume of the output device.
pub const SYSTEM_VOLUME_UNCERTAINTY_DB: f64 = 3.0;

/// From a measured test tone: play a sine at `tone_level_dbfs`, measure the RMS
/// volts at the headphone, and scale back up to full scale.
///
/// `full_scale_vrms = measured_vrms · 10^(−tone_level_dbfs / 20)`
///
/// `tone_level_dbfs` is the **peak** level of the tone, the number the tone
/// generator shows (a −20 dBFS sine reads −23.01 dBFS RMS).
pub fn from_measured_tone(name: impl Into<String>, measured_vrms: f64, tone_level_dbfs: f64) -> PlaybackCalibration {
    let full_scale = measured_vrms * 10.0f64.powf(-tone_level_dbfs / 20.0);
    PlaybackCalibration {
        name: name.into(),
        method: CalibrationMethod::MeasuredVoltage,
        full_scale_vrms: full_scale.max(0.0),
        uncertainty_db: MEASURED_TONE_UNCERTAINTY_DB,
    }
}

/// From data sheets: the full-scale output of the DAC, the gain of the
/// amplifier, and how far the volume control is turned down.
///
/// `full_scale_vrms = dac_full_scale_vrms · 10^((amp_gain_db − volume_attenuation_db) / 20)`
///
/// `volume_attenuation_db` is positive dB of attenuation. `attenuation_is_estimated`
/// is `true` when that number is read off an unmarked analog knob. The macOS app
/// uses `true` as the default: nobody knows where a knob is.
pub fn from_specs(
    name: impl Into<String>,
    dac_full_scale_vrms: f64,
    amp_gain_db: f64,
    volume_attenuation_db: f64,
    attenuation_is_estimated: bool,
) -> PlaybackCalibration {
    let full_scale = dac_full_scale_vrms * 10.0f64.powf((amp_gain_db - volume_attenuation_db) / 20.0);
    PlaybackCalibration {
        name: name.into(),
        method: CalibrationMethod::EnteredSpecs,
        full_scale_vrms: full_scale.max(0.0),
        uncertainty_db: if attenuation_is_estimated {
            SPECS_WITH_GUESSED_ATTENUATION_UNCERTAINTY_DB
        } else {
            SPECS_UNCERTAINTY_DB
        },
    }
}

/// SPL at the eardrum for a steady sine at `dbfs` (peak level), at 1 kHz.
///
/// `SPL = db_spl_per_volt + 20·log10(full_scale_vrms) + dbfs`
///
/// The calibration window shows this as one line the user can check: "a −20 dBFS
/// 1 kHz tone ≈ 78 dB SPL". If that line reads 110, the calibration is wrong.
pub fn expected_spl(sine_dbfs: f64, sensitivity: &HeadphoneSensitivity, calibration: &PlaybackCalibration) -> f64 {
    let volts = calibration.full_scale_vrms.max(1e-12);
    sensitivity.db_spl_per_volt + 20.0 * volts.log10() + sine_dbfs
}

/// The same value at a frequency other than 1 kHz, shaped by the response of the
/// headphone. `curve` is normalized here, so pass the raw curve from the library.
pub fn expected_spl_at(
    sine_dbfs: f64,
    hz: f64,
    sensitivity: &HeadphoneSensitivity,
    calibration: &PlaybackCalibration,
    curve: &HeadphoneCurve,
) -> f64 {
    let response = CurveInterpolator::new(curve.normalized_to_1khz()).level(hz as f32);
    expected_spl(sine_dbfs, sensitivity, calibration) + response as f64
}
