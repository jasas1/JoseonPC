//! What the measurement code may claim: an uncertainty with every term in it,
//! a leak test, one lower limit with one cause, and no hook at the top of the grid.
//!
//! Ported from `Tests/JoseonHeadphonesTests/MeasurementHonestyTests.swift`.

use super::*;
use crate::calibration::{self, from_measured_tone, from_specs};
use crate::curve::HeadphoneCurve;
use crate::measurement::sweep_analysis::{standard_grid, Options};
use crate::measurement::sweep_signal::PinkNoiseOptions;
use crate::measurement::{
    derive_sensitivity, drive_voltage_uncertainty_db, AbsoluteLevel, BassShortfall, ComponentKind, Derivation,
    DerivedSensitivity, HeadphoneMeasurement, LowLimitCause, MeasuredHeadphone, MeasurementQuality, MeasurementRig,
    MicCalibration, Reference, Route, SensitivityContext,
};
use joseon_core::contracts::{CalibrationMethod, PlaybackCalibration};

// MARK: The uncertainty budget

struct Row {
    label: &'static str,
    level: Reference,
    playback: PlaybackCalibration,
    rig: MeasurementRig,
    mic_db: f64,
    expected_total_db: f64,
    expected_printed_db: f64,
    expected_headline: &'static str,
}

#[test]
fn uncertainty_is_every_term_in_quadrature_with_a_dominant_term() {
    let meter = from_measured_tone("WA33, multimeter", 0.1, -6.0);
    let specs = from_specs("WA33 from data sheets", 2.0, 10.0, 20.0, true);
    let calibrator = Reference::calibrator(-32.04);
    let sens_factor = Reference::mic_sensitivity(-32.04);
    let repeatability = 0.2;
    let row = |label,
               level: &Reference,
               playback: &PlaybackCalibration,
               rig,
               mic_db,
               squares: [f64; 5],
               printed,
               headline| Row {
        label,
        level: level.clone(),
        playback: playback.clone(),
        rig,
        mic_db,
        expected_total_db: squares.iter().sum::<f64>().sqrt(),
        expected_printed_db: printed,
        expected_headline: headline,
    };
    use MeasurementRig::{EarSimulator, FlatPlate, Other};
    let rows = [
        row(
            "calibrator · meter · ear simulator · mic file",
            &calibrator,
            &meter,
            EarSimulator,
            0.5,
            [0.25, 4.0, 1.0, 0.25, 0.04],
            2.5,
            "± 2.5 dB (voltage-limited)",
        ),
        row(
            "calibrator · meter · flat plate · mic file",
            &calibrator,
            &meter,
            FlatPlate,
            0.5,
            [0.25, 4.0, 6.25, 0.25, 0.04],
            3.5,
            "± 3.5 dB (rig-limited)",
        ),
        row(
            "Sens Factor · meter · flat plate · mic file",
            &sens_factor,
            &meter,
            FlatPlate,
            0.5,
            [4.0, 4.0, 6.25, 0.25, 0.04],
            4.0,
            "± 4 dB (rig-limited)",
        ),
        row(
            "Sens Factor · meter · other rig · no mic file",
            &sens_factor,
            &meter,
            Other,
            2.0,
            [4.0, 4.0, 9.0, 4.0, 0.04],
            5.0,
            "± 5 dB (rig-limited)",
        ),
        row(
            "calibrator · data sheets and a guessed knob · other rig · mic file",
            &calibrator,
            &specs,
            Other,
            0.5,
            [0.25, 36.0, 9.0, 0.25, 0.04],
            7.0,
            "± 7 dB (voltage-limited)",
        ),
        // Three terms tie at 2 dB. The first listed wins, so the answer is stable.
        row(
            "Sens Factor · meter · ear simulator · no mic file",
            &sens_factor,
            &meter,
            EarSimulator,
            2.0,
            [4.0, 4.0, 1.0, 4.0, 0.04],
            4.0,
            "± 4 dB (reference-limited)",
        ),
    ];
    for row in rows {
        let label = row.label;
        let level = AbsoluteLevel::for_reference(&row.level);
        let result = derive_sensitivity(Derivation {
            magnitude_at_1khz_db: -20.0,
            absolute_level: &level,
            playback: &row.playback,
            sweep_level_dbfs: -6.0,
            impedance_ohms: 45.0,
            drive_voltage_uncertainty_db: None,
            measurement_uncertainty_db: repeatability,
            mic_calibration_uncertainty_db: row.mic_db,
            rig: row.rig,
            source: "test".to_string(),
        });
        let derived = result.value().expect(label);
        println!(
            "  {label:<58} {:.3} dB -> {}",
            derived.uncertainty_db,
            derived.uncertainty_headline()
        );
        for (term, value, detail) in derived.uncertainty_table() {
            println!("      {term}: {value} — {detail}");
        }
        near(derived.uncertainty_db, row.expected_total_db, 1e-9);
        near(derived.printed_uncertainty_db(), row.expected_printed_db, 1e-9);
        assert_eq!(derived.uncertainty_headline(), row.expected_headline, "{label}");
        assert!(
            derived.printed_uncertainty_db() >= derived.uncertainty_db,
            "the printed figure must never be smaller than the total"
        );
        assert_eq!(derived.rig, row.rig);
        // Five named terms, every one of them in the total.
        let kinds: Vec<ComponentKind> = derived.uncertainty_components.iter().map(|c| c.kind).collect();
        assert_eq!(
            kinds,
            [
                ComponentKind::AbsoluteLevel,
                ComponentKind::DriveVoltage,
                ComponentKind::Rig,
                ComponentKind::Microphone,
                ComponentKind::Repeatability
            ],
            "{label}"
        );
        assert!(
            derived.uncertainty_components.iter().all(|c| !c.detail.is_empty()),
            "{label}"
        );
        let quadrature = derived
            .uncertainty_components
            .iter()
            .fold(0.0, |sum, c| sum + c.db * c.db)
            .sqrt();
        near(derived.uncertainty_db, quadrature, 1e-9);
        // The old "± 0.9 dB" can not come back out of any combination.
        assert!(derived.uncertainty_db > 2.0, "{label}");
    }
}

#[test]
fn drive_term_is_the_figure_of_the_calibration_and_the_rig_term_is_real() {
    let meter = from_measured_tone("meter", 0.1, -6.0);
    assert_eq!(
        meter.uncertainty_db, 2.0,
        "the calibration window prints 2 dB for a meter"
    );
    assert_eq!(drive_voltage_uncertainty_db(&meter), 2.0);
    let specs = from_specs("specs", 2.0, 10.0, 20.0, true);
    assert_eq!(drive_voltage_uncertainty_db(&specs), 6.0);
    assert_eq!(
        drive_voltage_uncertainty_db(&specs),
        calibration::SPECS_WITH_GUESSED_ATTENUATION_UNCERTAINTY_DB
    );
    assert_eq!(
        drive_voltage_uncertainty_db(&from_specs("stepped", 2.0, 10.0, 20.0, false)),
        4.0
    );
    // An optimistic calibration made by hand can not go under the floor of the method.
    let made = |name: &str, method, uncertainty_db| PlaybackCalibration {
        name: name.to_string(),
        method,
        full_scale_vrms: 1.0,
        uncertainty_db,
    };
    assert_eq!(
        drive_voltage_uncertainty_db(&made("wishful", CalibrationMethod::MeasuredVoltage, 0.2)),
        2.0
    );
    assert_eq!(
        drive_voltage_uncertainty_db(&made("Mac jack", CalibrationMethod::SystemVolume, 2.0)),
        3.0
    );

    assert_eq!(MeasurementRig::EarSimulator.uncertainty_db(), 1.0);
    assert_eq!(MeasurementRig::FlatPlate.uncertainty_db(), 2.5);
    assert_eq!(MeasurementRig::Other.uncertainty_db(), 3.0);
    for rig in MeasurementRig::ALL {
        assert!(!rig.label().is_empty() && !rig.note().is_empty() && !rig.as_str().is_empty());
    }

    // The Sens Factor route carries its stated 2 dB into the sum, whatever a caller passes.
    let talked_down = AbsoluteLevel::for_reference(&Reference::MicSensitivity {
        dbfs_per_pascal: -32.0,
        uncertainty_db: 0.1,
    });
    let scale = talked_down.scale().unwrap();
    assert_eq!(scale.uncertainty_db, MicCalibration::SENS_FACTOR_UNCERTAINTY_DB);
    assert_eq!(scale.route, Route::MicrophoneSensFactor);
    assert_eq!(
        AbsoluteLevel::for_reference(&Reference::calibrator(-32.0))
            .scale()
            .unwrap()
            .route,
        Route::AcousticCalibrator
    );
}

#[test]
fn total_rounds_up_to_the_next_half_decibel() {
    near(DerivedSensitivity::rounded_up_to_half_db(2.3537), 2.5, 1e-12);
    near(DerivedSensitivity::rounded_up_to_half_db(3.2848), 3.5, 1e-12);
    near(DerivedSensitivity::rounded_up_to_half_db(3.0), 3.0, 1e-12);
    near(DerivedSensitivity::rounded_up_to_half_db(2.5000000001), 2.5, 1e-12);
    near(DerivedSensitivity::rounded_up_to_half_db(2.51), 3.0, 1e-12);
}

/// A session with an absolute level and a playback calibration, for the budget tests.
fn measured_session(rig: Option<MeasurementRig>) -> MeasuredHeadphone {
    let chain = SimulatedChain::standard(50.0);
    let sweep = sweep(3.0, -6.0);
    let mic = MicCalibration::parse(
        "\"Sens Factor =-14.0412dB, SERNO: 7012345\"\r\n20\t0\t0\r\n100\t0\t0\r\n1000\t0\t0\r\n20000\t0\t0",
        "UMIK-1 7012345 (0°)",
        "",
    )
    .unwrap();
    let mut session = HeadphoneMeasurement::with(sweep.clone(), Options::default(), Some(mic), None, standard_grid());
    session.set_noise_segment(chain.silence(&sweep));
    for run in 0..3u64 {
        session.add_run(&chain.seeded(run * 23 + 5).record(&sweep), false);
    }
    let mut context = SensitivityContext::new(
        from_measured_tone("WA33 at 10 o'clock", 0.1, -6.0),
        AbsoluteLevel::for_reference(&Reference::calibrator(-32.04)),
        45.0,
    );
    if let Some(rig) = rig {
        context.rig = rig;
    }
    session
        .result("simulated", "Joseon measurement", Some(&context))
        .unwrap()
}

/// Through the session: the rig goes in the context, and the default is the pessimistic one.
#[test]
fn session_carries_the_rig_into_the_budget() {
    let measured = measured_session(None);
    let derived = measured.derived_sensitivity.as_ref().unwrap();
    assert_eq!(
        derived.rig,
        MeasurementRig::FlatPlate,
        "the default rig is the one most people have"
    );
    assert_eq!(derived.dominant_term().kind, ComponentKind::Rig);
    assert!(derived.uncertainty_headline().ends_with("(rig-limited)"));
    assert!(measured.method.contains("rig: Flat plate"), "{}", measured.method);

    let on_a_simulator = measured_session(Some(MeasurementRig::EarSimulator));
    let simulator = on_a_simulator.derived_sensitivity.as_ref().unwrap();
    assert!(
        simulator.uncertainty_db < derived.uncertainty_db,
        "an ear simulator must give something"
    );
}

// MARK: The leak

/// One simulated session, optionally with a first-order leak in front of the headphone.
pub(super) fn measure(leak_at_hz: Option<f64>, snr_db: f64, with_noise_segment: bool) -> MeasuredHeadphone {
    let mut headphone = FilterCascade::headphone(SAMPLE_RATE);
    if let Some(hz) = leak_at_hz {
        headphone.stages.push(Biquad::first_order_high_pass(hz, SAMPLE_RATE));
    }
    let mut chain = SimulatedChain::new(SAMPLE_RATE, headphone);
    chain.snr_db = Some(snr_db);
    let sweep = sweep(3.0, -6.0);
    let mut session = HeadphoneMeasurement::new(sweep.clone());
    if with_noise_segment {
        session.set_noise_segment(chain.silence(&sweep));
    }
    for run in 0..3u64 {
        session.add_run(&chain.seeded(run * 17 + 1).record(&sweep), false);
    }
    session.result("simulated", "Joseon measurement", None).unwrap()
}

/// A first-order leak at 80 Hz against the clean response of the same headphone.
#[test]
fn bass_shortfall_finds_a_simulated_leak() {
    let grid = standard_grid();
    let clean = FilterCascade::headphone(SAMPLE_RATE);
    let reference = HeadphoneCurve {
        name: "HiFiMAN Susvara Unveiled".to_string(),
        source: "AutoEq".to_string(),
        frequencies_hz: grid.iter().map(|&f| f as f32).collect(),
        levels_db: normalized_to_1khz(&clean.response_on(&grid), &grid)
            .iter()
            .map(|&v| v as f32)
            .collect(),
    };

    // A clean measurement of the same headphone: no shortfall, no flag.
    let tight = measure(None, 60.0, true).bass_shortfall(&reference).unwrap();
    println!(
        "  no leak: {:.2} dB over 30–100 Hz ({} points)",
        tight.mean_difference_db, tight.points_used
    );
    near(tight.mean_difference_db, 0.0, 0.3);
    assert!(!tight.likely_leak);
    near(tight.low_hz, 30.0, 1.0);
    near(tight.high_hz, 100.0, 3.0);
    assert!(tight.points_used >= 10);

    // The leak, against the closed form of the filter that made it.
    for leak_hz in [80.0, 150.0] {
        let leak = Biquad::first_order_high_pass(leak_hz, SAMPLE_RATE);
        let relative: Vec<f64> = grid
            .iter()
            .map(|&f| leak.response_db(f, SAMPLE_RATE) - leak.response_db(1_000.0, SAMPLE_RATE))
            .collect();
        let truth = mean(&relative, &grid, 30.0, 100.0);
        let shortfall = measure(Some(leak_hz), 60.0, true).bass_shortfall(&reference).unwrap();
        println!(
            "  {leak_hz:.0} Hz leak: measured {:.2} dB, closed form {truth:.2} dB, likely leak {}",
            shortfall.mean_difference_db, shortfall.likely_leak
        );
        expect(
            (shortfall.mean_difference_db - truth).abs(),
            0.5,
            "bass shortfall against the closed form of the leak",
        );
        assert_eq!(shortfall.likely_leak, truth < BassShortfall::LEAK_THRESHOLD_DB);
        if shortfall.likely_leak {
            assert!(shortfall.summary.contains("likely a seal leak on the rig"));
            assert!(shortfall.summary.contains("HiFiMAN Susvara Unveiled"));
        }
    }
}

/// A reference that stops above the range is not compared against.
#[test]
fn bass_shortfall_refuses_a_reference_that_does_not_reach_the_bass() {
    let measured = measure(Some(150.0), 60.0, true);
    let curve = |name: &str, frequencies_hz: Vec<f32>| HeadphoneCurve {
        name: name.to_string(),
        source: "test".to_string(),
        levels_db: vec![0.0; frequencies_hz.len()],
        frequencies_hz,
    };
    assert!(measured
        .bass_shortfall(&curve("stops at 200 Hz", vec![200.0, 500.0, 1_000.0, 5_000.0]))
        .is_none());
    assert!(measured.bass_shortfall(&curve("empty", vec![])).is_none());
}

/// The same bass fall means two different things, and only the signal-to-noise
/// ratio at 40 Hz says which.
#[test]
fn leak_wording_switches_on_the_signal_to_noise_ratio_at_40_hz() {
    let quiet = measure(Some(80.0), 45.0, true);
    let quiet_snr = quiet.quality.snr_db_at(40.0).unwrap();
    let quiet_note = quiet.bass_roll_off_note().unwrap();
    println!("  quiet room: SNR at 40 Hz {quiet_snr:.0} dB — {quiet_note}");
    assert!(quiet_snr >= 15.0);
    assert!(quiet_note.contains("looks like a seal leak"));
    assert!(quiet.quality.warnings.contains(&quiet_note));

    let noisy = measure(Some(80.0), 5.0, true);
    let noisy_snr = noisy.quality.snr_db_at(40.0).unwrap();
    let noisy_note = noisy.bass_roll_off_note().unwrap();
    println!("  noisy room: SNR at 40 Hz {noisy_snr:.0} dB — {noisy_note}");
    assert!(noisy_snr < 15.0);
    assert!(noisy_note.contains("limited by noise"));
    assert!(!noisy_note.contains("seal leak"));
    assert!(noisy.quality.warnings.contains(&noisy_note));

    // A sealed headphone says nothing at all.
    assert!(measure(None, 60.0, true).bass_roll_off_note().is_none());
    // And with no noise-only segment Joseon does not call it one or the other.
    let blind_note = measure(Some(80.0), 45.0, false).bass_roll_off_note().unwrap();
    assert!(blind_note.contains("can not tell a seal leak from the noise floor"));
}

// MARK: One lower limit, one cause

#[test]
fn lowest_reliable_hz_is_the_higher_of_the_two_limits() {
    let quality = |noise: f32, window: f32| MeasurementQuality {
        runs: 1,
        lowest_resolved_hz: window,
        noise_limited_hz: noise,
        ..Default::default()
    };
    assert_eq!(quality(28.0, 10.0).lowest_reliable_hz(), 28.0);
    assert_eq!(quality(28.0, 10.0).low_limit_cause(), LowLimitCause::Noise);
    assert_eq!(quality(8.0, 10.0).lowest_reliable_hz(), 10.0);
    assert_eq!(quality(8.0, 10.0).low_limit_cause(), LowLimitCause::Window);
    // No noise segment is not a noise limit.
    assert_eq!(quality(0.0, 10.0).low_limit_cause(), LowLimitCause::Window);
    // A tie goes to the window.
    assert_eq!(quality(10.0, 10.0).low_limit_cause(), LowLimitCause::Window);
    assert_eq!(quality(28.0, 10.0).lowest_resolved_hz, 10.0);
}

#[test]
fn the_warnings_name_one_limit_and_one_cause() {
    let quiet = measure(None, 60.0, true);
    assert_eq!(quiet.quality.low_limit_cause(), LowLimitCause::Window);
    assert_eq!(
        quiet.quality.noise_limited_hz, 0.0,
        "10 dB of signal-to-noise at the bottom of the grid is not a noise limit"
    );
    assert_eq!(quiet.quality.lowest_reliable_hz(), quiet.quality.lowest_resolved_hz);

    let noisy = measure(None, 5.0, true);
    assert_eq!(noisy.quality.low_limit_cause(), LowLimitCause::Noise);
    assert!(noisy.quality.noise_limited_hz > noisy.quality.lowest_resolved_hz);
    assert_eq!(noisy.quality.lowest_reliable_hz(), noisy.quality.noise_limited_hz);

    for measured in [&quiet, &noisy] {
        let limits: Vec<&String> = measured
            .quality
            .warnings
            .iter()
            .filter(|w| w.contains("Nothing under"))
            .collect();
        assert_eq!(limits.len(), 1, "the result page must print one lower limit, not two");
        let sentence = limits[0];
        println!("  {sentence}");
        assert!(sentence.contains(&format!("{:.0}", measured.quality.lowest_reliable_hz())));
        match measured.quality.low_limit_cause() {
            LowLimitCause::Window => {
                assert!(sentence.contains("analysis window"));
                assert!(!sentence.contains("signal-to-noise"));
            }
            LowLimitCause::Noise => {
                assert!(sentence.contains("signal-to-noise"));
                assert!(!sentence.contains("analysis window"));
            }
        }
    }
    // And the file says the same thing, once.
    assert!(quiet.csv().contains("limit set by the length of the analysis window"));
}

// MARK: The top of the band

#[test]
fn the_sweep_knows_where_it_stopped_driving() {
    // 48 kHz: 20 kHz is under Nyquist, so the fade-out sets the top.
    let five = sweep(5.0, -6.0);
    let l5 = five.duration_seconds / (five.end_hz / five.start_hz).ln();
    near(five.highest_full_energy_hz(), 20_000.0 * (-0.020 / l5).exp(), 1.0);
    near(five.highest_full_energy_hz(), 19_454.9, 1.0);
    assert!(
        sweep(3.0, -6.0).highest_full_energy_hz() < five.highest_full_energy_hz(),
        "a shorter sweep spends more of the top octave in the fade"
    );
    // 32 kHz: the Nyquist clamp sets the end frequency, and the fade still comes off it.
    let clamped = SweepSignal::exponential_sweep(32_000.0, SweepOptions::default());
    near(clamped.end_hz, 15_200.0, 1e-6);
    assert!(clamped.highest_full_energy_hz() < clamped.end_hz);
    near(clamped.highest_full_energy_hz(), 14_802.0, 5.0);
    // Pink noise has no fade: its band ends.
    let pink = SweepSignal::periodic_pink_noise(48_000.0, PinkNoiseOptions::default());
    near(pink.highest_full_energy_hz(), pink.end_hz, 1e-9);
}

/// The defect in the review picture: the curve had a hook at the last grid
/// point, where the fade-out of the sweep had taken the energy away.
#[test]
fn no_hook_at_the_top_of_the_log_grid() {
    let grid = standard_grid();
    let chain = SimulatedChain::standard(60.0);
    let sweep = sweep(5.0, -6.0);
    let mut session = HeadphoneMeasurement::new(sweep.clone());
    session.set_noise_segment(chain.silence(&sweep));
    session.add_run(&chain.record(&sweep), false);
    let measured = session.result("top of band", "Joseon measurement", None).unwrap();

    let upper = measured.quality.highest_reliable_hz as f64;
    near(upper, sweep.highest_full_energy_hz(), 0.01);
    assert!(upper < 20_000.0, "the grid goes higher than the sweep did");

    let truth = chain.true_response_db(&grid, false);
    let got = to_f64(&measured.absolute_magnitude_db);
    // The last octave under the reported limit is still a measurement.
    let worst = worst_difference(&got, &truth, &grid, upper / 2.0, upper);
    expect(worst.0, 0.5, "top octave up to the reported upper limit");

    // Above it the curve holds flat: no hook, up or down.
    let last_index = grid.iter().rposition(|&f| f <= upper).unwrap();
    let last_measured = got[last_index];
    for (i, &f) in grid.iter().enumerate() {
        if f > upper {
            near(got[i], last_measured, 1e-9);
            near(
                measured.curve.levels_db[i] as f64,
                measured.curve.levels_db[grid.len() - 2] as f64,
                1e-4,
            );
        }
    }
    // Before the fix the last grid point was 6 dB low.
    assert!(
        (got[grid.len() - 1] - truth[grid.len() - 1]).abs() < 0.5,
        "the last grid point must not hook away from the response"
    );
    assert!(measured.quality.warnings.iter().any(|w| w.contains("held flat")));
}
