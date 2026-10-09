//! The chain from dBFS bands to dB SPL at the eardrum, and the weightings on top of it.
//!
//! Ported from `Tests/JoseonHeadphonesTests/SPLEstimatorTests.swift`.

use super::spl_support::*;
use crate::spl_estimator::SplEstimator;
use crate::spl_weighting::{a_weighting, third_octave_bands};
use joseon_core::contracts::{CalibrationMethod, PlaybackCalibration, SplEstimating, SplReading};

// MARK: The chain

/// A 1 kHz band at −23.01 dBFS RMS, 1 V full scale, 100 dB/V flat headphone.
/// `−23.01 + 3.01 + 20·log10(1) + 100 + 0 = 80.0` dB SPL at the eardrum.
#[test]
fn eardrum_level_of_a_known_band() {
    let reading = frame(&mut estimator(), &one_band(1_000.0, -23.01));
    near(reading.band_levels_eardrum[band_index(1_000.0)] as f64, 80.0, 0.05);
}

/// The A-weighted, diffuse-field-equivalent level for the same band is the
/// eardrum level minus the diffuse-field term at 1 kHz.
#[test]
fn a_weighted_level_is_eardrum_minus_the_diffuse_field_term() {
    let reading = frame(&mut estimator(), &one_band(1_000.0, -23.01));
    let df = SplEstimator::diffuse_field_term_db(1_000.0);
    near(reading.level_a_fast as f64, 80.0 - df, 0.05);
}

/// Ten times the voltage is 20 dB more, everywhere in the chain.
#[test]
fn ten_times_the_voltage_is_twenty_db_more() {
    let mut one = estimator();
    let mut ten = Fixture {
        full_scale_vrms: 10.0,
        ..Default::default()
    }
    .estimator();
    let bands = one_band(1_000.0, -23.01);
    let a = frame(&mut one, &bands);
    let b = frame(&mut ten, &bands);
    near((b.level_a_fast - a.level_a_fast) as f64, 20.0, 0.05);
    let i = band_index(1_000.0);
    near((b.band_levels_eardrum[i] - a.band_levels_eardrum[i]) as f64, 20.0, 0.05);
}

/// The same voltage change made live gives the same 20 dB.
#[test]
fn a_new_calibration_takes_effect_live() {
    let mut estimator = estimator();
    let bands = one_band(1_000.0, -23.01);
    let before = frame(&mut estimator, &bands);
    estimator.set_calibration(calibration(10.0, 4.0));
    let after = frame(&mut estimator, &bands);
    near((after.level_a_fast - before.level_a_fast) as f64, 20.0, 0.05);
    assert_eq!(after.uncertainty_db, 4.0);
}

/// A curve that is +6 dB across the 4 kHz band raises that band by exactly 6 dB.
#[test]
fn curve_boost_raises_its_band() {
    // The 4 kHz third-octave band spans 3565–4490 Hz; the plateau covers it with room to spare.
    let mut boosted = estimator_with_curve(plateau_curve(6.0, 3_000.0, 5_500.0));
    let bands = one_band(4_000.0, -23.01);
    let a = frame(&mut estimator(), &bands);
    let b = frame(&mut boosted, &bands);
    let i = band_index(4_000.0);
    near((b.band_levels_eardrum[i] - a.band_levels_eardrum[i]) as f64, 6.0, 0.05);
}

/// The plateau leaves 1 kHz alone: normalization is the 800–1250 Hz mean.
#[test]
fn curve_boost_leaves_other_bands_alone() {
    let mut boosted = estimator_with_curve(plateau_curve(6.0, 3_000.0, 5_500.0));
    let bands = one_band(1_000.0, -23.01);
    let a = frame(&mut estimator(), &bands);
    let b = frame(&mut boosted, &bands);
    let i = band_index(1_000.0);
    near(b.band_levels_eardrum[i] as f64, a.band_levels_eardrum[i] as f64, 0.02);
}

// MARK: A-weighting

/// IEC 61672 values at the nominal centers, from the published table.
#[test]
fn a_weighting_matches_the_standard_table() {
    near(a_weighting::db(100.0), -19.1, 0.1);
    near(a_weighting::db(1_000.0), 0.0, 0.1);
    near(a_weighting::db(10_000.0), -2.5, 0.1);
}

/// 1 kHz is exactly 0 by construction, not only close.
#[test]
fn a_weighting_is_exactly_zero_at_one_kilohertz() {
    near(a_weighting::db(1_000.0), 0.0, 1e-12);
}

/// The whole published table, as `(nominal Hz, dB)`.
const PUBLISHED_A_TABLE: [(f64, f64); 31] = [
    (20.0, -50.5),
    (25.0, -44.7),
    (31.5, -39.4),
    (40.0, -34.6),
    (50.0, -30.2),
    (63.0, -26.2),
    (80.0, -22.5),
    (100.0, -19.1),
    (125.0, -16.1),
    (160.0, -13.4),
    (200.0, -10.9),
    (250.0, -8.6),
    (315.0, -6.6),
    (400.0, -4.8),
    (500.0, -3.2),
    (630.0, -1.9),
    (800.0, -0.8),
    (1_000.0, 0.0),
    (1_250.0, 0.6),
    (1_600.0, 1.0),
    (2_000.0, 1.2),
    (2_500.0, 1.3),
    (3_150.0, 1.2),
    (4_000.0, 1.0),
    (5_000.0, 0.5),
    (6_300.0, -0.1),
    (8_000.0, -1.1),
    (10_000.0, -2.5),
    (12_500.0, -4.3),
    (16_000.0, -6.6),
    (20_000.0, -9.3),
];

/// The printed values are defined at the EXACT base-10 midband frequencies, so
/// that is where they are checked.
#[test]
fn a_weighting_matches_the_whole_standard_table() {
    for (nominal, db) in PUBLISHED_A_TABLE {
        let exact = third_octave_bands::exact_center_hz(nominal);
        let ours = a_weighting::db(exact);
        assert!(
            (ours - db).abs() <= 0.1,
            "A-weighting at nominal {nominal} Hz (exact {exact} Hz): {ours} vs {db}"
        );
    }
}

/// The nominal center in place of the exact one is a real but small error.
#[test]
fn nominal_centers_cost_under_two_tenths_of_a_db() {
    let (mut worst, mut worst_hz) = (0.0f64, 0.0);
    for (nominal, db) in PUBLISHED_A_TABLE {
        let error = a_weighting::db(nominal) - db;
        if error.abs() > worst.abs() {
            (worst, worst_hz) = (error, nominal);
        }
    }
    println!("A-weighting at nominal centers vs the table: worst {worst:+.2} dB at {worst_hz} Hz");
    assert!(worst.abs() < 0.2);
}

/// The exact midband grid is the base-10 one IEC 61260 defines.
#[test]
fn exact_center_frequencies() {
    near(third_octave_bands::exact_center_hz(1_000.0), 1_000.0, 1e-9);
    near(third_octave_bands::exact_center_hz(16_000.0), 15_848.93, 0.01);
    near(third_octave_bands::exact_center_hz(31.5), 31.6228, 0.001);
}

// MARK: Time weighting

/// The slow weighting is a 1 s exponential on A-weighted energy. A step from
/// silence is within 1 dB after about 2.3 s and not before 1.6 s.
#[test]
fn slow_weighting_reaches_a_step_in_about_two_point_three_seconds() {
    let mut estimator = estimator();
    let bands = reading(80.0);
    let mut elapsed = 0.0;
    let mut reading = estimator.evaluate(&bands, 0.0, false);
    while elapsed < 1.0 - 1e-9 {
        reading = estimator.evaluate(&bands, 0.05, false);
        elapsed += 0.05;
    }
    // Still climbing at 1 s: 10·log10(1 − e^−1) = −1.99 dB.
    assert!((reading.level_a_slow as f64) < 80.0 - 1.0);
    while elapsed < 2.3 - 1e-9 {
        reading = estimator.evaluate(&bands, 0.05, false);
        elapsed += 0.05;
    }
    near(reading.level_a_slow as f64, 80.0, 1.0);
    near(reading.level_a_fast as f64, 80.0, 0.05);
}

/// The fast level needs no smoothing: the bands are already 125 ms averages.
#[test]
fn fast_level_follows_the_bands_immediately() {
    let mut estimator = estimator();
    let quiet = frame(&mut estimator, &reading(70.0));
    let loud = frame(&mut estimator, &reading(95.0));
    near(quiet.level_a_fast as f64, 70.0, 0.05);
    near(loud.level_a_fast as f64, 95.0, 0.05);
}

// MARK: Louder ear

/// The louder channel decides every reported value, per frame.
#[test]
fn louder_ear_is_chosen_per_frame() {
    let mut estimator = estimator();
    let left_loud = left_only(1_000.0, dbfs_for(90.0));
    let mut right_loud = left_loud.clone();
    std::mem::swap(&mut right_loud.left, &mut right_loud.right);
    let a = frame(&mut estimator, &left_loud);
    let b = frame(&mut estimator, &right_loud);
    near(a.level_a_fast as f64, 90.0, 0.05);
    near(b.level_a_fast as f64, 90.0, 0.05);
    let i = band_index(1_000.0);
    near(a.band_levels_eardrum[i] as f64, b.band_levels_eardrum[i] as f64, 0.001);
}

// MARK: Leq

/// 30 s at 80 dBA then 30 s at 90 dBA: `10·log10((30·10^8 + 30·10^9) / 60) = 87.40` dBA.
#[test]
fn leq_of_two_levels() {
    let mut estimator = estimator();
    run(&mut estimator, 80.0, 30.0, 1.0);
    let reading = run(&mut estimator, 90.0, 30.0, 1.0);
    near(reading.leq_a_track as f64, 87.4, 0.1);
    near(reading.leq_a_session as f64, 87.4, 0.1);
    near(reading.dose_seconds, 60.0, 1e-6);
}

/// `reset_measurement` clears the track Leq and the max, and leaves the dose alone.
#[test]
fn reset_measurement_clears_track_and_max_only() {
    let mut estimator = estimator();
    run(&mut estimator, 90.0, 30.0, 1.0);
    estimator.reset_measurement();
    let reading = run(&mut estimator, 80.0, 30.0, 1.0);
    near(reading.leq_a_track as f64, 80.0, 0.05);
    near(reading.max_a_fast as f64, 80.0, 0.05);
    // Session Leq and dose kept running across the track change.
    near(reading.leq_a_session as f64, 87.4, 0.1);
    near(reading.dose_seconds, 60.0, 1e-6);
}

/// `max_a_fast` holds the loudest frame, not the loudest average.
#[test]
fn max_a_fast_holds_the_peak_frame() {
    let mut estimator = estimator();
    run(&mut estimator, 70.0, 5.0, 1.0);
    frame(&mut estimator, &reading(101.0));
    let reading = run(&mut estimator, 70.0, 5.0, 1.0);
    near(reading.max_a_fast as f64, 101.0, 0.05);
}

/// Quiet music is not silence: it counts into the Leq and the dose clock.
#[test]
fn quiet_but_not_silent_time_counts() {
    let mut estimator = estimator();
    let reading = run(&mut estimator, 35.0, 10.0, 1.0);
    near(reading.dose_seconds, 10.0, 1e-6);
    near(reading.leq_a_track as f64, 35.0, 0.1);
    // 35 dBA is far under every dose threshold, so the dose stays at zero.
    near(reading.dose_niosh as f64, 0.0, 1e-9);
}

// MARK: Floors and bad input

/// Digital silence reads at the floor of the contract, not at −infinity.
#[test]
fn silence_reads_at_the_floor() {
    let mut estimator = estimator();
    let reading = run(&mut estimator, 90.0, 2.0, 1.0);
    assert!(reading.level_a_fast > 80.0);
    let mut last = reading;
    for _ in 0..200 {
        last = estimator.evaluate(&silence(), 0.125, true);
    }
    near(last.level_a_slow as f64, SplReading::FLOOR_DB as f64, 0.5);
    near(last.level_z_eardrum as f64, SplReading::FLOOR_DB as f64, 0.5);
    assert!(last.band_levels_eardrum.iter().all(|&v| v >= SplReading::FLOOR_DB));
}

/// A zero-volt calibration is not a level; it must not give NaN or −infinity.
#[test]
fn zero_volt_calibration_does_not_give_nan() {
    let mut estimator = SplEstimator::new(
        flat_curve(),
        sensitivity100(),
        PlaybackCalibration {
            name: "zero".to_string(),
            method: CalibrationMethod::EnteredSpecs,
            full_scale_vrms: 0.0,
            uncertainty_db: 4.0,
        },
    );
    let reading = frame(&mut estimator, &one_band(1_000.0, -20.0));
    assert!(reading.level_a_fast.is_finite());
    assert!(reading.level_a_slow.is_finite());
    assert!(reading.level_z_eardrum.is_finite());
    assert!(reading.band_levels_eardrum.iter().all(|v| v.is_finite()));
}

/// The reading carries the name and the uncertainty of the calibration.
#[test]
fn reading_carries_the_calibration_identity() {
    let reading = frame(&mut estimator(), &reading(80.0));
    assert_eq!(reading.calibration_name, "test bench");
    assert_eq!(reading.uncertainty_db, 2.0);
}

/// The Z level is the unweighted eardrum sum.
#[test]
fn z_level_is_the_unweighted_eardrum_sum() {
    let mut estimator = estimator();
    let bands = one_band(1_000.0, -23.01);
    let mut reading = estimator.evaluate(&bands, 0.0, false);
    for _ in 0..100 {
        reading = frame(&mut estimator, &bands);
    }
    near(reading.level_z_eardrum as f64, 80.0, 0.1);
}

// MARK: Band geometry

/// The band mean is a real band mean: a curve with a narrow peak inside one
/// band shows a smaller value there than the peak itself.
#[test]
fn band_mean_averages_across_the_band_not_at_the_center() {
    // A +12 dB plateau only 1/20 octave wide, centred on 1 kHz.
    let narrow = plateau_curve(12.0, 983.0, 1_017.0);
    let mut peaky = SplEstimator::new(narrow, sensitivity100(), calibration(1.0, 2.0));
    let bands = one_band(1_000.0, -23.01);
    let a = frame(&mut estimator(), &bands);
    let b = frame(&mut peaky, &bands);
    let i = band_index(1_000.0);
    assert!(b.band_levels_eardrum[i] - a.band_levels_eardrum[i] < 6.0);
}
