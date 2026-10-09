//! Ported from `Tests/JoseonHeadphonesTests/MeasurementCalibrationTests.swift`.

use super::*;
use crate::calibration::{from_measured_tone, from_specs};
use crate::curve::CurveParseError;
use crate::measurement::mic_calibration::sens_factor_value;
use crate::measurement::sweep_analysis::standard_grid;
use crate::measurement::{
    derive_sensitivity, drive_voltage_uncertainty_db, AbsoluteLevel, CouplerCorrection, Derivation,
    HeadphoneMeasurement, MeasurementRig, MicCalibration, Reference, SensitivityContext,
};
use joseon_core::contracts::{CalibrationMethod, PlaybackCalibration};
use std::collections::HashSet;

// MARK: Parsing

/// A miniDSP UMIK-1 file: quoted sensitivity header, tabs, three columns, CRLF endings.
#[test]
fn umik_file() {
    let text = "\"Sens Factor =-1.2dB, SERNO: 7012345\"\r\n\
                20.000\t-2.4345\t0.0000\r\n\
                25.000\t-1.9876\t1.2000\r\n\
                1000.000\t0.0000\t0.0000\r\n\
                10000.000\t1.5000\t-3.4000\r\n\
                20000.000\t4.2000\t-8.0000\r\n";
    let mic = MicCalibration::parse(text, "UMIK-1 7012345", "").unwrap();
    assert_eq!(mic.frequencies_hz.len(), 5);
    near(mic.sens_factor_db.unwrap(), -1.2, 1e-9);
    assert!(mic.has_phase_column);
    near(mic.levels_db[0], -2.4345, 1e-9);
    near(mic.levels_db[4], 4.2, 1e-9);
    assert!(mic.frequencies_hz.iter().all(|f| f.is_finite()));
    near(
        mic.mic_sensitivity_dbfs_per_pascal().unwrap(),
        MicCalibration::SENS_FACTOR_REFERENCE_DBFS - 1.2,
        1e-9,
    );
    assert_eq!(mic.source, "UMIK-1 7012345", "an empty source becomes the name");
}

#[test]
fn sens_factor_spellings() {
    near(sens_factor_value("Sens Factor =-.1dB, SERNO: 1").unwrap(), -0.1, 1e-9);
    near(sens_factor_value("sens factor = 1.25 dB").unwrap(), 1.25, 1e-9);
    near(sens_factor_value("SensFactor=-12dB").unwrap(), -12.0, 1e-9);
    assert!(sens_factor_value("20.0 -2.4").is_none());
    assert!(sens_factor_value("Sens Factor without a number").is_none());
}

#[test]
fn two_column_rew_style_with_comments_and_bad_rows() {
    let text = "* Microphone calibration file\n\
                # exported by something\n\
                // another comment style\n\
                ; and one more\n\
                0.0, 0.0\n\
                -5.0, 1.0\n\
                rubbish, 2.0\n\
                20.0, -2.0\n\
                50.0, -1.0\n\
                100.0, -0.5\n\
                1000.0, 0.0\n\
                5000.0, 1.0\n\
                10000.0, 2.5\n\
                20000.0\n\
                20000.0, 5.0";
    let mic = MicCalibration::parse(text, "REW", "").unwrap();
    assert_eq!(
        mic.frequencies_hz,
        [20.0, 50.0, 100.0, 1_000.0, 5_000.0, 10_000.0, 20_000.0]
    );
    assert!(!mic.has_phase_column);
    assert!(mic.sens_factor_db.is_none());
}

#[test]
fn space_separated_and_duplicate_frequencies() {
    let text = "20   -2.0\n20   -4.0\n100  -1.0\n1000  0.0\n10000 2.0";
    let mic = MicCalibration::parse(text, "spaces", "").unwrap();
    assert_eq!(mic.frequencies_hz, [20.0, 100.0, 1_000.0, 10_000.0]);
    near(mic.levels_db[0], -3.0, 1e-9); // duplicate frequencies are averaged
}

#[test]
fn too_few_rows_is_an_error() {
    assert_eq!(
        MicCalibration::parse("20,1\n100,2\n1000,3", "short", "").unwrap_err(),
        CurveParseError::NoData
    );
    assert!(MicCalibration::parse("", "empty", "").is_err());
    assert!(CouplerCorrection::parse("nothing here", "junk", "").is_err());
}

#[test]
fn correction_is_flat_beyond_the_ends_of_the_file() {
    let mic = MicCalibration::parse("100,1.0\n200,2.0\n1000,3.0\n5000,4.0\n10000,5.0", "short range", "").unwrap();
    let grid = [20.0, 100.0, 1_000.0, 10_000.0, 20_000.0];
    let correction = mic.correction_db(&grid);
    near(correction[0], 1.0, 1e-6); // flat below the first point
    near(correction[4], 5.0, 1e-6); // flat above the last point
    near(correction[2], 3.0, 1e-6);
    // To apply it is to subtract.
    near(mic.apply(&[0.0; 5], &grid)[2], -3.0, 1e-6);
}

#[test]
fn coupler_correction_uses_the_same_parser() {
    let coupler = CouplerCorrection::parse(
        "# flat plate, characterised 2026-09-21\n20,3.0\n100,2.0\n1000,0.0\n8000,-4.0\n20000,-6.0",
        "Flat plate",
        "",
    )
    .unwrap();
    assert_eq!(coupler.frequencies_hz.len(), 5);
    let applied = coupler.apply(&[0.0, 0.0], &[20.0, 8_000.0]);
    near(applied[0], -3.0, 1e-6);
    near(applied[1], 4.0, 1e-6);
}

// MARK: Absolute level

#[test]
fn calibrator_gives_an_offset() {
    let result = AbsoluteLevel::for_reference(&Reference::calibrator(-32.04));
    let scale = result.scale().unwrap();
    near(scale.offset_db, 94.0 + 32.04, 1e-9);
    near(scale.spl_db(-32.04), 94.0, 1e-9);
    assert!(result.is_available());
    assert!(result.unavailable_reason().is_none());
}

#[test]
fn mic_sensitivity_gives_the_same_offset() {
    let sensitivity = AbsoluteLevel::for_reference(&Reference::mic_sensitivity(-32.04));
    near(sensitivity.scale().unwrap().offset_db, 94.0 + 32.04, 1e-9);
}

/// The point of the enum: with nothing to connect dBFS to pascals there is no
/// number to use by accident.
#[test]
fn no_reference_means_no_absolute_level() {
    let none = AbsoluteLevel::for_reference(&Reference::from_mic_calibration(None));
    assert!(none.scale().is_none());
    assert!(!none.is_available());
    assert!(none.unavailable_reason().unwrap().contains("pascals"));

    let headerless = MicCalibration::parse("20,1\n100,2\n1000,3\n10000,4", "no header", "").unwrap();
    let without_header = AbsoluteLevel::for_reference(&Reference::from_mic_calibration(Some(&headerless)));
    assert!(without_header.scale().is_none());
    assert!(without_header
        .unavailable_reason()
        .unwrap()
        .contains("sensitivity header"));

    assert!(AbsoluteLevel::for_reference(&Reference::calibrator(3.0))
        .scale()
        .is_none());
}

/// The drive term is what the active calibration says it is: the same figure
/// the calibration window prints.
#[test]
fn drive_voltage_uncertainty_is_the_figure_of_the_calibration() {
    let measured = from_measured_tone("m", 0.1, -6.0);
    let specs = from_specs("s", 2.0, 10.0, 20.0, true);
    assert_eq!(drive_voltage_uncertainty_db(&measured), measured.uncertainty_db);
    assert_eq!(drive_voltage_uncertainty_db(&measured), 2.0);
    assert_eq!(drive_voltage_uncertainty_db(&specs), specs.uncertainty_db);
    assert_eq!(specs.uncertainty_db, 6.0);
}

// MARK: Sensitivity derivation

/// 0.1 V RMS into a headphone of known sensitivity, measured with a 12.5 mV/Pa
/// microphone through an interface of known gain. The derivation must give back
/// the number the simulation started from.
#[test]
fn sensitivity_derivation() {
    let grid = standard_grid();
    let headphone = FilterCascade::headphone(SAMPLE_RATE);
    let target_sensitivity = 100.0; // dB SPL per volt at 1 kHz, the answer wanted
    let drive_vrms: f64 = 0.1;
    let sweep_level_dbfs = -6.0;
    let full_scale_vrms = drive_vrms * 10.0f64.powf(-sweep_level_dbfs / 20.0);
    let digital_per_pascal: f64 = 0.0125 * 2.0;
    let mic_sensitivity_dbfs_per_pascal = 20.0 * digital_per_pascal.log10();

    // The linear gain of everything between the digital drive and the digital
    // recording, so that the whole chain really has `target_sensitivity` at 1 kHz.
    let gain = 10.0f64.powf(-headphone.response_db(1_000.0) / 20.0)
        * 2.0f64.sqrt()
        * full_scale_vrms
        * 10.0f64.powf((target_sensitivity - 94.0) / 20.0)
        * digital_per_pascal;

    let mut chain = SimulatedChain::new(SAMPLE_RATE, headphone);
    chain.gain = gain;
    chain.snr_db = Some(60.0);
    let sweep = sweep(3.0, sweep_level_dbfs);
    let playback = from_measured_tone("WA33 at 10 o'clock", drive_vrms, sweep_level_dbfs);
    near(playback.full_scale_vrms, full_scale_vrms, 1e-9);

    // Route 1: a UMIK file whose Sens Factor states the sensitivity of the microphone.
    let sens_factor = mic_sensitivity_dbfs_per_pascal - MicCalibration::SENS_FACTOR_REFERENCE_DBFS;
    let cal_text = [
        format!("\"Sens Factor ={sens_factor:.4}dB, SERNO: 7012345\""),
        "20\t0.0\t0.0".to_string(),
        "100\t0.0\t0.0".to_string(),
        "1000\t0.0\t0.0".to_string(),
        "20000\t0.0\t0.0".to_string(),
    ];
    let mic = MicCalibration::parse(&cal_text.join("\r\n"), "UMIK-1", "").unwrap();
    let mut session =
        HeadphoneMeasurement::with(sweep.clone(), Default::default(), Some(mic.clone()), None, grid.clone());
    session.set_noise_segment(chain.silence(&sweep));
    for run in 0..3u64 {
        session.add_run(&chain.seeded(run * 101 + 3).record(&sweep), false);
    }
    let context = SensitivityContext::new(
        playback.clone(),
        AbsoluteLevel::for_reference(&Reference::from_mic_calibration(Some(&mic))),
        45.0,
    );
    let measured = session
        .result("simulated headphone", "Joseon measurement", Some(&context))
        .unwrap();
    let derived = measured.derived_sensitivity.as_ref().unwrap();
    println!(
        "  derived {:.3} dB SPL/V (simulated {target_sensitivity:.3}), ± {:.2} dB; SPL at 1 kHz in the run {:.1} dB, drive {:.4} Vrms",
        derived.sensitivity.db_spl_per_volt, derived.uncertainty_db, derived.measured_spl_at_1khz, derived.drive_volts_rms
    );
    expect(
        (derived.sensitivity.db_spl_per_volt - target_sensitivity).abs(),
        0.2,
        "derived sensitivity against the simulated 1 kHz gain",
    );
    near(derived.drive_volts_rms, drive_vrms, 1e-6);
    near(
        derived.measured_spl_at_1khz,
        target_sensitivity + 20.0 * drive_vrms.log10(),
        0.2,
    );
    assert_eq!(derived.sensitivity.impedance_ohms, 45.0);
    assert!(derived.sensitivity.source.starts_with("Measured with Joseon"));
    assert_eq!(measured.sensitivity.as_ref(), Some(&derived.sensitivity));

    // The uncertainty is the pieces in quadrature, and every piece is named.
    let quadrature = derived
        .uncertainty_components
        .iter()
        .fold(0.0, |sum, c| sum + c.db * c.db)
        .sqrt();
    near(derived.uncertainty_db, quadrature, 1e-9);
    assert_eq!(derived.uncertainty_components.len(), 5);
    let names: HashSet<&str> = derived.uncertainty_components.iter().map(|c| c.name.as_str()).collect();
    assert_eq!(
        names,
        HashSet::from([
            "Absolute level reference",
            "Drive voltage",
            "Rig",
            "Microphone response at 1 kHz",
            "Measurement repeatability",
        ])
    );
    assert!(derived.uncertainty_db > MicCalibration::SENS_FACTOR_UNCERTAINTY_DB);
    println!("  budget: {}", derived.uncertainty_headline());

    // Route 2: an acoustic calibrator reading, which must give the same answer.
    let via_calibrator = session.derived_sensitivity(&SensitivityContext::new(
        playback.clone(),
        AbsoluteLevel::for_reference(&Reference::calibrator(mic_sensitivity_dbfs_per_pascal)), // 94 dB SPL is 1 Pa
        45.0,
    ));
    let calibrated = via_calibrator.value().unwrap();
    expect(
        (calibrated.sensitivity.db_spl_per_volt - target_sensitivity).abs(),
        0.2,
        "derived sensitivity through an acoustic calibrator",
    );
    assert!(
        calibrated.uncertainty_db < derived.uncertainty_db,
        "a calibrator is a better reference than a sensitivity header"
    );

    // Route 3: nothing. The curve still comes back; the sensitivity does not.
    let none = SensitivityContext::new(
        playback,
        AbsoluteLevel::for_reference(&Reference::from_mic_calibration(None)),
        45.0,
    );
    let without_reference = session.result("x", "Joseon measurement", Some(&none)).unwrap();
    assert!(without_reference.sensitivity.is_none());
    assert!(without_reference.derived_sensitivity.is_none());
    assert!(without_reference
        .quality
        .warnings
        .iter()
        .any(|w| w.contains("No sensitivity was derived")));
    assert_eq!(without_reference.curve.frequencies_hz.len(), grid.len());
}

#[test]
fn derivation_refuses_a_zero_voltage_calibration() {
    let broken = PlaybackCalibration {
        name: "none".to_string(),
        method: CalibrationMethod::EnteredSpecs,
        full_scale_vrms: 0.0,
        uncertainty_db: 8.0,
    };
    let result = derive_sensitivity(Derivation {
        magnitude_at_1khz_db: -20.0,
        absolute_level: &AbsoluteLevel::for_reference(&Reference::calibrator(-32.0)),
        playback: &broken,
        sweep_level_dbfs: -6.0,
        impedance_ohms: 45.0,
        drive_voltage_uncertainty_db: None,
        measurement_uncertainty_db: 0.3,
        mic_calibration_uncertainty_db: 0.5,
        rig: MeasurementRig::FlatPlate,
        source: "test".to_string(),
    });
    assert!(result.value().is_none());
    assert!(result.unavailable_reason().is_some());
}
