//! Real analyzer bands -> real estimator, through the engine, the way the app runs it.
//!
//! Ported from `SPLEndToEndTests.swift` and `SPLThreadSafetyTests.swift`.

use super::spl_support::*;
use crate::embedded_curves::HD650;
use crate::sensitivity;
use crate::spl_estimator::SplEstimator;
use joseon_core::contracts::{
    CalibrationMethod, HeadphoneSensitivity, PlaybackCalibration, SplEstimating, ThirdOctaveReading,
};
use joseon_core::engine::AnalysisEngine;
use joseon_core::test_signals::sine;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

#[test]
fn sine_through_engine_gives_expected_level_at_the_ear() {
    let engine = AnalysisEngine::new();
    let estimator = SplEstimator::new(
        flat_curve(),
        HeadphoneSensitivity {
            db_spl_per_volt: 100.0,
            impedance_ohms: 32.0,
            source: "test".to_string(),
        },
        PlaybackCalibration {
            name: "test".to_string(),
            method: CalibrationMethod::MeasuredVoltage,
            full_scale_vrms: 1.0,
            uncertainty_db: 2.0,
        },
    );
    engine.set_spl_estimator(Some(Arc::new(Mutex::new(estimator))));
    let rate = 48_000.0;
    let tone = sine(1_000.0, 0.1, rate, 6.0, 0.0); // −20 dBFS peak
    let mut last = None;
    for block in tone.chunks_exact(800) {
        last = Some(engine.process_now(block, block, rate));
    }
    let frame = last.unwrap();
    let spl = frame.spl.as_ref().expect("an SPL reading: bands or estimator missing");
    // −20 dBFS sine at 1 Vrms full scale = 0.1 Vrms -> 100 dB/V − 20 dB = 80 dB SPL at the eardrum.
    near(spl.level_z_eardrum as f64, 80.0, 0.5);
    // A-weighting is 0 dB at 1 kHz; the diffuse-field equivalent is a few dB under the eardrum level.
    assert!(spl.level_a_slow < spl.level_z_eardrum);
    assert!(spl.level_a_slow > spl.level_z_eardrum - 8.0);
    assert!(spl.dose_seconds > 4.0);
    assert_eq!(spl.calibration_name, "test");
}

/// The app changes the curve, the sensitivity and the calibration from other
/// threads while frames keep coming. Every frame must stay finite and have the
/// correct shape.
#[test]
fn configure_while_evaluating() {
    let estimator = Arc::new(Mutex::new(estimator()));
    let bands = reading(88.0);
    let curves = Arc::new([flat_curve(), plateau_curve(6.0, 3_000.0, 5_500.0), HD650.curve()]);
    let running = Arc::new(AtomicBool::new(true));
    let workers: Vec<_> = (0..3usize)
        .map(|worker| {
            let (estimator, curves, running) = (estimator.clone(), curves.clone(), running.clone());
            std::thread::spawn(move || {
                let mut i = 0usize;
                while running.load(Ordering::Relaxed) {
                    let mut e = estimator.lock().unwrap();
                    match (i + worker) % 4 {
                        0 => e.set_curve(&curves[i % curves.len()]),
                        1 => e.set_sensitivity(sensitivity::hd600()),
                        2 => e.set_calibration(calibration(0.5 + (i % 5) as f64, 2.0)),
                        _ => {
                            e.set_curve(&curves[i % curves.len()]);
                            e.set_sensitivity(sensitivity::focal_utopia());
                            e.set_calibration(calibration(2.0, 2.0));
                        }
                    }
                    if i % 17 == 0 {
                        e.reset_measurement();
                    }
                    if i % 37 == 0 {
                        let _ = e.dose_state();
                    }
                    let _ = e.chain_offset_db();
                    drop(e);
                    i += 1;
                }
            })
        })
        .collect();

    let mut frames = 0;
    let band_count = ThirdOctaveReading::NOMINAL_CENTERS_HZ.len();
    let end = Instant::now() + Duration::from_millis(600);
    while Instant::now() < end {
        let reading = estimator.lock().unwrap().evaluate(&bands, 0.125, false);
        assert_eq!(reading.band_levels_eardrum.len(), band_count);
        assert!(reading.level_a_fast.is_finite());
        assert!(reading.level_a_slow.is_finite());
        assert!(reading.level_z_eardrum.is_finite());
        assert!(reading.dose_niosh.is_finite());
        frames += 1;
    }
    running.store(false, Ordering::Relaxed);
    for worker in workers {
        worker.join().unwrap();
    }
    assert!(frames > 100);
}

/// A curve change and a sensitivity change under one lock land together.
#[test]
fn a_headphone_change_lands_as_one() {
    let estimator = Mutex::new(estimator());
    let quiet = HeadphoneSensitivity {
        db_spl_per_volt: 60.0,
        impedance_ohms: 32.0,
        source: "synthetic test fixture".to_string(),
    };
    let bands = one_band(1_000.0, -23.01);
    let before = estimator.lock().unwrap().evaluate(&bands, 0.125, false);
    {
        let mut e = estimator.lock().unwrap();
        e.set_curve(&HD650.curve());
        e.set_sensitivity(quiet.clone());
    }
    let mut e = estimator.lock().unwrap();
    let after = e.evaluate(&bands, 0.125, false);
    // 100 dB/V down to 60 dB/V is 40 dB, plus what the HD 650 does at 1 kHz (near 0).
    near((before.level_a_fast - after.level_a_fast) as f64, 40.0, 0.5);
    assert_eq!(e.sensitivity(), &quiet);
    assert_eq!(e.normalized_curve().name, "Sennheiser HD 650");
}

/// Budget from the spec: under 0.05 ms per `evaluate` in release.
#[test]
fn evaluate_stays_under_budget() {
    let mut estimator = estimator_with_curve(HD650.curve());
    let bands = reading(88.0);
    for _ in 0..200 {
        estimator.evaluate(&bands, 0.125, false);
    }
    let runs = 20_000;
    // The fastest of five: other tests run at the same time.
    let mut best_ms = f64::INFINITY;
    for _ in 0..5 {
        let start = Instant::now();
        for _ in 0..runs {
            std::hint::black_box(estimator.evaluate(&bands, 0.125, false));
        }
        best_ms = best_ms.min(start.elapsed().as_secs_f64() * 1e3 / runs as f64);
    }
    println!("SplEstimator::evaluate mean {best_ms:.5} ms");
    let slack: f64 = std::env::var("JOSEON_TIMING_SLACK")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(1.0);
    let budget_ms = if cfg!(debug_assertions) { 0.5 } else { 0.05 };
    assert!(best_ms < budget_ms * slack, "{best_ms} ms per evaluate");
}

/// A curve change makes the band cache again, and the next frame already shows the new curve.
#[test]
fn curve_change_takes_effect_on_the_next_frame() {
    let mut estimator = estimator();
    let bands = one_band(4_000.0, -23.01);
    let i = band_index(4_000.0);
    let before = frame(&mut estimator, &bands);
    estimator.set_curve(&plateau_curve(6.0, 3_000.0, 5_500.0));
    let after = frame(&mut estimator, &bands);
    near(
        (after.band_levels_eardrum[i] - before.band_levels_eardrum[i]) as f64,
        6.0,
        0.05,
    );
}
