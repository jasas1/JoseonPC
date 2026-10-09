//! The whole session, as the app will drive it, to a file the library can read back.
//!
//! Ported from `MeasurementPipelineTests.swift` and `MeasurementCostTests.swift`.

use super::*;
use crate::calibration::from_measured_tone;
use crate::embedded_curves::STANDARD_FREQUENCIES_HZ;
use crate::library::HeadphoneLibrary;
use crate::measurement::sweep_analysis::{standard_grid, OctaveSmoothing, Options};
use crate::measurement::sweep_signal::PinkNoiseOptions;
use crate::measurement::{
    AbsoluteLevel, DerivedSensitivityResult, HeadphoneMeasurement, MeasuredHeadphone, MicCalibration, Reference,
    SensitivityContext,
};
use crate::model::HeadphoneModel;
use std::time::Instant;

fn session(runs: u64, noise: bool) -> MeasuredHeadphone {
    let mut chain = SimulatedChain::standard(45.0);
    chain.second_harmonic = 0.005;
    let sweep = sweep(3.0, -6.0);
    let mic = MicCalibration::parse(
        "\"Sens Factor =-14.0412dB, SERNO: 7012345\"\r\n20\t0\t0\r\n1000\t0\t0\r\n20000\t0\t0\r\n10000\t0\t0",
        "UMIK-1 7012345 (0°)",
        "",
    )
    .unwrap();
    let mut session = HeadphoneMeasurement::with(
        sweep.clone(),
        Options::default(),
        Some(mic.clone()),
        None,
        standard_grid(),
    );
    if noise {
        session.set_noise_segment(chain.silence(&sweep));
    }
    for i in 0..runs {
        let summary = session.add_run(&chain.seeded(i * 17 + 1).record(&sweep), false);
        assert_eq!(summary.delay_samples, 3_733);
        assert!(!summary.clipped);
    }
    session
        .result(
            "HiFiMAN Susvara Unveiled (measured)",
            "Joseon measurement",
            Some(&SensitivityContext::new(
                from_measured_tone("WA33 at 10 o'clock", 0.1, -6.0),
                AbsoluteLevel::for_reference(&Reference::from_mic_calibration(Some(&mic))),
                45.0,
            )),
        )
        .unwrap()
}

#[test]
fn result_is_on_the_module_grid_and_normalized() {
    let measured = session(3, true);
    assert_eq!(measured.curve.frequencies_hz.len(), 200);
    assert_eq!(measured.curve.frequencies_hz, STANDARD_FREQUENCIES_HZ);
    near(measured.curve.reference_level_db() as f64, 0.0, 0.05);
    assert!(measured.curve.levels_db.iter().all(|v| v.is_finite()));
    assert_eq!(measured.quality.runs, 3);
    assert!(!measured.quality.snr_db.is_empty());
    assert!(measured.quality.snr_db_at(1_000.0).unwrap() > 30.0);
    near(measured.quality.thd_percent as f64, 0.5, 0.15);
    assert!(measured.quality.agreement_db < 0.2);
    near(measured.quality.lowest_resolved_hz as f64, 9.5, 1.0);
    println!("  method: {}", measured.method);
}

#[test]
fn warnings_say_what_is_missing() {
    let sweep = sweep(2.0, -6.0);
    let chain = SimulatedChain::standard(40.0);
    let mut session = HeadphoneMeasurement::new(sweep.clone()); // no microphone file, no coupler
    session.add_run(&chain.record(&sweep), false); // one run, no noise segment
    let measured = session.result("bare", "Joseon measurement", None).unwrap();
    for expected in [
        "No microphone calibration",
        "No coupler correction",
        "One run only",
        "No noise-only segment",
        "No sensitivity was derived",
        "Clock drift",
    ] {
        assert!(
            measured.quality.warnings.iter().any(|w| w.contains(expected)),
            "no warning about {expected}"
        );
    }
    assert!(measured.sensitivity.is_none());
    assert!(!session.saw_clipping());

    // Clipping is caught and said.
    let mut clipping = HeadphoneMeasurement::new(sweep.clone());
    let mut loud = chain.record(&sweep);
    loud[10_000] = 1.0;
    assert!(clipping.add_run(&loud, false).clipped);
    assert!(clipping.saw_clipping());
    let clipped = clipping.result("clipped", "Joseon measurement", None).unwrap();
    assert!(clipped.quality.warnings.iter().any(|w| w.contains("clipped")));
}

/// The point of `csv`: what comes out goes back in.
#[test]
fn csv_round_trips_through_the_library() {
    let measured = session(3, true);
    let csv = measured.csv();
    assert!(csv.contains("frequency,raw"));
    assert!(csv.starts_with("# HiFiMAN Susvara Unveiled (measured)"));

    // A name with no character that Windows refuses in a path.
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    let directory = std::env::temp_dir().join(format!("joseon-measure-{}-{stamp}", std::process::id()));
    let source = directory.join("source");
    std::fs::create_dir_all(&source).unwrap();
    let file = source.join("HiFiMAN Susvara Unveiled (measured).csv");
    std::fs::write(&file, &csv).unwrap();

    let library = HeadphoneLibrary::new(directory.join("Curves"));
    let reloaded = library.import_curve(&file).unwrap();
    assert_eq!(reloaded.frequencies_hz.len(), measured.curve.frequencies_hz.len());
    let mut worst = 0.0f32;
    for i in 0..reloaded.levels_db.len() {
        assert!((reloaded.frequencies_hz[i] - measured.curve.frequencies_hz[i]).abs() <= 0.001);
        worst = worst.max((reloaded.levels_db[i] - measured.curve.levels_db[i]).abs());
    }
    println!("  CSV round trip: worst level difference {worst:.6} dB");
    assert!(worst < 0.001);

    // It is a curve the other code can use like any other.
    assert_eq!(library.user_curves().len(), 1);
    assert_eq!(
        library
            .curve_named("HiFiMAN Susvara Unveiled (measured)")
            .unwrap()
            .levels_db
            .len(),
        200
    );
    let model = HeadphoneModel::new(reloaded, None);
    let response = model.response_db(&[30.0, 1_000.0, 3_000.0, 8_000.0]);
    near(response[1] as f64, 0.0, 0.05);
    assert!(response[2] > 4.0); // the 3 kHz peak is still there
    assert!(response[3] < -5.0); // and so is the 8 kHz notch
    let _ = std::fs::remove_dir_all(&directory);
}

#[test]
fn no_runs_means_no_result() {
    let session = HeadphoneMeasurement::new(sweep(2.0, -6.0));
    assert!(session.result("nothing", "Joseon measurement", None).is_none());
    assert_eq!(session.run_count(), 0);
    let derived = session.derived_sensitivity(&SensitivityContext::new(
        from_measured_tone("x", 0.1, -6.0),
        AbsoluteLevel::for_reference(&Reference::calibrator(-32.0)),
        45.0,
    ));
    assert!(
        matches!(derived, DerivedSensitivityResult::Unavailable { .. }),
        "a sensitivity was derived from no measurement"
    );
}

/// Pink noise as the stimulus: a coarser tool, and it says so.
#[test]
fn pink_noise_stimulus_also_recovers_the_curve() {
    let grid = standard_grid();
    let noise = SweepSignal::periodic_pink_noise(SAMPLE_RATE, PinkNoiseOptions::default());
    let chain = SimulatedChain::standard(50.0);
    let options = Options {
        smoothing: OctaveSmoothing::Sixth,
        ..Default::default()
    };
    let mut session = HeadphoneMeasurement::with(noise.clone(), options, None, None, grid.clone());
    for i in 0..4u64 {
        session.add_run(&chain.seeded(i * 31 + 5).record(&noise), false);
    }
    let measured = session.result("pink", "Joseon measurement", None).unwrap();
    let truth = normalized_to_1khz(&chain.true_response_db(&grid, false), &grid);
    let worst = worst_difference(&to_f64(&measured.curve.levels_db), &truth, &grid, 50.0, 16_000.0);
    println!("  pink noise, 4 periods: worst {:.3} dB at {:.0} Hz", worst.0, worst.1);
    expect(worst.0, 1.5, "pink-noise measurement, 50 Hz – 16 kHz");
    assert_eq!(measured.quality.thd_percent, 0.0);
    assert!(measured
        .quality
        .warnings
        .iter()
        .any(|w| w.contains("Pink noise can not separate distortion")));
}

// MARK: Cost

/// The budget of the macOS app is 150 ms for one 5 s sweep at 48 kHz, in release.
#[test]
fn one_five_second_sweep_under_150_milliseconds() {
    let chain = SimulatedChain::standard(45.0);
    let sweep = sweep(5.0, -6.0);
    let recording = chain.record(&sweep); // made outside the timed section
    let silence = chain.silence(&sweep);
    assert_eq!(recording.len(), 249_733);
    let mut best = f64::MAX;
    for _ in 0..5 {
        let start = Instant::now();
        let mut session = HeadphoneMeasurement::new(sweep.clone());
        session.set_noise_segment(silence.clone());
        session.add_run(&recording, false);
        let measured = session.result("cost", "Joseon measurement", None);
        let elapsed = start.elapsed().as_secs_f64() * 1000.0;
        assert!(measured.is_some());
        best = best.min(elapsed);
    }
    println!("  one 5 s sweep at 48 kHz: {best:.1} ms");
    if cfg!(debug_assertions) {
        return; // a debug build is several times slower
    }
    let slack: f64 = std::env::var("JOSEON_TIMING_SLACK")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(1.0);
    expect(best, 150.0 * slack, "analysis of one 5 s sweep, milliseconds");
}

/// The work that depends only on the sweep is done once: the session keeps one deconvolver.
#[test]
fn further_runs_use_the_same_deconvolver() {
    let chain = SimulatedChain::standard(45.0);
    let sweep = sweep(5.0, -6.0);
    let recording = chain.record(&sweep);
    let mut session = HeadphoneMeasurement::new(sweep);
    let first = session.add_run(&recording, false);
    let second = session.add_run(&recording, false);
    assert_eq!(first, second);
    assert_eq!(session.run_count(), 2);
    // Two equal runs agree exactly.
    let measured = session.result("twice", "Joseon measurement", None).unwrap();
    assert!(measured.quality.agreement_db < 1e-6);
}
