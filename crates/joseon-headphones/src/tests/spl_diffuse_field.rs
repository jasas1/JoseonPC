//! The diffuse-field reference, and the calibration helpers built on the same chain.
//!
//! Ported from `Tests/JoseonHeadphonesTests/SPLDiffuseFieldTests.swift`.

use super::spl_support::*;
use crate::calibration::{self, expected_spl, expected_spl_at, from_measured_tone, from_specs};
use crate::spl_estimator::SplEstimator;
use crate::spl_weighting::{diffuse_field_offset_decision, diffuse_field_reference};
use joseon_core::contracts::{CalibrationMethod, HeadphoneSensitivity};

/// The offset is the published one, not a rounded stand-in.
#[test]
fn offset_is_the_published_value() {
    near(diffuse_field_reference::ABSOLUTE_OFFSET_AT_1KHZ_DB, 4.1, 1e-12);
    assert!(diffuse_field_reference::ABSOLUTE_OFFSET_SOURCE.contains("Hammershøi"));
    assert!(diffuse_field_reference::ABSOLUTE_OFFSET_SOURCE.contains("11904-1"));
}

/// The embedded KEMAR diffuse-field target, normalized and shifted by the one
/// published constant, must reproduce an INDEPENDENT measurement of the same
/// quantity: Hammershøi & Møller, Table II.
#[test]
fn constructed_response_matches_the_published_table() {
    let table = diffuse_field_offset_decision::PUBLISHED_EARDRUM_DIFFUSE_FIELD_DB;
    let (mut worst, mut worst_hz, mut sum_squares) = (0.0f64, 0.0, 0.0);
    for (hz, published) in table {
        let mine = SplEstimator::diffuse_field_term_db(hz);
        let error = mine - published;
        sum_squares += error * error;
        if error.abs() > worst.abs() {
            (worst, worst_hz) = (error, hz);
        }
        assert!(
            (mine - published).abs() <= 1.0,
            "diffuse-field term at {hz} Hz: {mine} vs {published}"
        );
    }
    let rms = (sum_squares / table.len() as f64).sqrt();
    println!(
        "diffuse-field term vs Hammershøi & Møller Table II: rms {rms:.2} dB, worst {worst:+.2} dB at {worst_hz} Hz"
    );
    assert!(rms < 0.6);
}

/// The shape peaks where the ear canal and concha resonate, well above 1 kHz.
#[test]
fn the_response_peaks_around_three_kilohertz() {
    let at_one_k = SplEstimator::diffuse_field_term_db(1_000.0);
    let at_three_k = SplEstimator::diffuse_field_term_db(3_150.0);
    assert!(at_three_k > at_one_k + 8.0);
    assert!(SplEstimator::diffuse_field_term_db(100.0) < 1.0);
}

/// The point response and the band term agree at 1 kHz to a fraction of a dB.
#[test]
fn point_response_is_near_the_band_term() {
    near(
        diffuse_field_reference::response_db(1_000.0),
        SplEstimator::diffuse_field_term_db(1_000.0),
        0.5,
    );
    assert_eq!(
        diffuse_field_reference::normalized_curve().name,
        "Diffuse field GRAS KEMAR"
    );
}

/// ISO 11904 subtracts: the diffuse-field-equivalent level is BELOW the eardrum
/// level where the response of the ear is positive.
#[test]
fn the_term_is_subtracted_not_added() {
    let reading = frame(&mut estimator(), &one_band(3_150.0, -23.01));
    let eardrum = reading.band_levels_eardrum[band_index(3_150.0)] as f64;
    // A-weighting at 3150 Hz is about +1.2 dB, the diffuse-field term about +15 dB.
    assert!((reading.level_a_fast as f64) < eardrum - 10.0);
}

// MARK: Calibration helpers

/// `full_scale_vrms = measured_vrms · 10^(−tone_level_dbfs / 20)`.
#[test]
fn from_measured_tone_scales_to_full_scale() {
    let c = from_measured_tone("WA33 at 10 o'clock", 0.1, -20.0);
    near(c.full_scale_vrms, 1.0, 1e-9);
    assert_eq!(c.method, CalibrationMethod::MeasuredVoltage);
    assert_eq!(c.uncertainty_db, 2.0);
    assert_eq!(c.name, "WA33 at 10 o'clock");
}

/// A tone measured at full scale needs no scaling at all.
#[test]
fn from_measured_tone_at_full_scale() {
    near(from_measured_tone("bench", 2.5, 0.0).full_scale_vrms, 2.5, 1e-9);
}

/// Specs: DAC output times the amplifier gain, less the attenuation.
#[test]
fn from_specs_with_a_known_attenuation() {
    let c = from_specs("DAC + amp", 2.0, 20.0, 20.0, false);
    near(c.full_scale_vrms, 2.0, 1e-9);
    assert_eq!(c.method, CalibrationMethod::EnteredSpecs);
    assert_eq!(c.uncertainty_db, 4.0);
}

/// A guessed knob position costs more uncertainty.
#[test]
fn from_specs_with_a_guessed_knob_is_less_certain() {
    let guessed = from_specs("WA33", 2.0, 20.0, 20.0, true);
    assert_eq!(guessed.uncertainty_db, 6.0);
    assert_eq!(
        guessed.uncertainty_db,
        calibration::SPECS_WITH_GUESSED_ATTENUATION_UNCERTAINTY_DB
    );
    assert!(guessed.uncertainty_db > calibration::SPECS_UNCERTAINTY_DB);
}

/// The table of voltage terms has the values of the macOS app.
#[test]
fn uncertainty_table() {
    assert_eq!(calibration::MEASURED_TONE_UNCERTAINTY_DB, 2.0);
    assert_eq!(calibration::MEASURED_OPEN_CIRCUIT_UNCERTAINTY_DB, 3.0);
    assert_eq!(calibration::SPECS_UNCERTAINTY_DB, 4.0);
    assert_eq!(calibration::SPECS_WITH_GUESSED_ATTENUATION_UNCERTAINTY_DB, 6.0);
    assert_eq!(calibration::SYSTEM_VOLUME_UNCERTAINTY_DB, 3.0);
}

/// The sanity line of the calibration sheet: a −20 dBFS 1 kHz tone at 98 dB SPL
/// per volt with a 1 V full scale is 78 dB SPL.
#[test]
fn expected_spl_sanity_line() {
    let sensitivity = HeadphoneSensitivity {
        db_spl_per_volt: 98.0,
        impedance_ohms: 300.0,
        source: "synthetic test fixture".to_string(),
    };
    let calibration = from_measured_tone("bench", 1.0, 0.0);
    near(expected_spl(-20.0, &sensitivity, &calibration), 78.0, 1e-9);
}

/// `expected_spl` and the band chain must agree. A −20 dBFS sine lands in the
/// 1 kHz band at −23.01 dBFS RMS.
#[test]
fn expected_spl_agrees_with_the_band_chain() {
    let reading = frame(&mut estimator(), &one_band(1_000.0, -23.01));
    let expected = expected_spl(-20.0, &sensitivity100(), &calibration(1.0, 2.0));
    near(expected, 80.0, 1e-9);
    near(reading.band_levels_eardrum[band_index(1_000.0)] as f64, expected, 0.05);
}

/// At a frequency away from 1 kHz the response of the headphone shapes the answer.
#[test]
fn expected_spl_at_another_frequency() {
    let at = |curve: &crate::curve::HeadphoneCurve| {
        expected_spl_at(-20.0, 4_000.0, &sensitivity100(), &calibration(1.0, 2.0), curve)
    };
    let plain = at(&flat_curve());
    let boosted = at(&plateau_curve(6.0, 3_000.0, 5_500.0));
    near(boosted - plain, 6.0, 0.05);
}
