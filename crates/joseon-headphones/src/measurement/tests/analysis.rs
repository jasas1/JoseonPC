//! The accuracy suite. Every test simulates the whole chain and compares the
//! recovered curve against the closed form of the filters that made it.
//!
//! Ported from `Tests/JoseonHeadphonesTests/MeasurementAnalysisTests.swift`.

use super::*;
use crate::measurement::sweep_analysis::{self as analysis, standard_grid, OctaveSmoothing, Options};
use crate::measurement::{HeadphoneMeasurement, MeasuredHeadphone, MicCalibration};

struct Measure {
    runs: usize,
    mic: Option<MicCalibration>,
    smoothing: OctaveSmoothing,
    with_noise_segment: bool,
}

impl Default for Measure {
    fn default() -> Self {
        Self {
            runs: 1,
            mic: None,
            smoothing: OctaveSmoothing::Twelfth,
            with_noise_segment: false,
        }
    }
}

/// Runs a whole measurement.
fn measure(chain: &SimulatedChain, sweep: &SweepSignal, how: Measure) -> MeasuredHeadphone {
    let options = Options {
        smoothing: how.smoothing,
        ..Default::default()
    };
    let mut session = HeadphoneMeasurement::with(sweep.clone(), options, how.mic, None, standard_grid());
    if how.with_noise_segment {
        session.set_noise_segment(chain.silence(sweep));
    }
    for run in 0..how.runs as u64 {
        let c = chain.seeded(chain.seed.wrapping_add(run.wrapping_mul(1_009)));
        session.add_run(&c.record(sweep), false);
    }
    session.result("test", "simulated", None).unwrap()
}

fn magnitude(chain: &SimulatedChain, sweep: &SweepSignal, how: Measure) -> Vec<f64> {
    to_f64(&measure(chain, sweep, how).absolute_magnitude_db)
}

// MARK: Alignment

#[test]
fn delay_is_found_to_the_sample() {
    let s = sweep(2.0, -6.0);
    for delay in [1usize, 977, 3_733, 12_001] {
        let mut recording = vec![0.0; delay + s.samples.len() + 4_000];
        recording[delay..delay + s.samples.len()].copy_from_slice(&s.samples);
        let noise = pink_noise(recording.len(), rms(&s.samples) * 0.03, delay as u64);
        for (v, n) in recording.iter_mut().zip(&noise) {
            *v += n;
        }
        let found = analysis::delay_samples(&recording, &s.samples);
        assert_eq!(found.samples, delay, "cross-correlation missed a {delay} sample delay");
        near(found.sub_sample, delay as f64, 0.05);
        assert!(found.confidence > 0.1);
        let ir = analysis::impulse_response(&recording, &s, &Options::default());
        assert_eq!(ir.peak_index, delay, "deconvolution missed a {delay} sample delay");
    }
}

/// With the headphone filter in the way the peak is still on the delay sample.
#[test]
fn delay_through_the_whole_chain() {
    let s = sweep(2.0, -6.0);
    let c = SimulatedChain::standard(40.0);
    let mut session = HeadphoneMeasurement::new(s.clone());
    let summary = session.add_run(&c.record(&s), true);
    assert_eq!(summary.delay_samples, 3_733);
    assert_eq!(summary.cross_correlation_delay_samples, Some(3_733));
    assert_eq!(session.run_summaries().len(), 1);
}

// MARK: Accuracy

#[test]
fn magnitude_within_03_db_at_60_db_snr() {
    let grid = standard_grid();
    let c = SimulatedChain::standard(60.0);
    let recovered = magnitude(&c, &sweep(5.0, -6.0), Measure::default());
    let truth = c.true_response_db(&grid, false);
    let worst = worst_difference(&recovered, &truth, &grid, 30.0, 16_000.0);
    println!("  60 dB SNR, 1/12 oct: worst {:.3} dB at {:.0} Hz", worst.0, worst.1);
    expect(worst.0, 0.3, "worst error 30 Hz – 16 kHz");
    // The shape is right where it matters: the peak and the notch come back at full height.
    let normalized = normalized_to_1khz(&recovered, &grid);
    let normalized_truth = normalized_to_1khz(&truth, &grid);
    expect(
        worst_difference(&normalized, &normalized_truth, &grid, 2_500.0, 3_500.0).0,
        0.3,
        "3 kHz peak error",
    );
    expect(
        worst_difference(&normalized, &normalized_truth, &grid, 7_000.0, 9_000.0).0,
        0.3,
        "8 kHz notch error",
    );
}

#[test]
fn magnitude_within_1_db_at_30_db_snr_with_four_runs() {
    let grid = standard_grid();
    let c = SimulatedChain::standard(30.0);
    let measured = measure(
        &c,
        &sweep(5.0, -6.0),
        Measure {
            runs: 4,
            ..Default::default()
        },
    );
    assert_eq!(measured.quality.runs, 4);
    let truth = c.true_response_db(&grid, false);
    let worst = worst_difference(&to_f64(&measured.absolute_magnitude_db), &truth, &grid, 30.0, 16_000.0);
    println!(
        "  30 dB SNR, 4 runs: worst {:.3} dB at {:.0} Hz, agreement {:.3} dB",
        worst.0, worst.1, measured.quality.agreement_db
    );
    expect(worst.0, 1.0, "worst error 30 Hz – 16 kHz");
}

/// The average must give something. Where noise sets the error, four
/// complex-averaged runs come near the 6 dB the theory promises.
#[test]
fn averaging_reduces_noise() {
    let grid = standard_grid();
    let c = SimulatedChain::standard(0.0); // the sweep is under the noise
    let s = sweep(2.0, -6.0);
    let truth = c.true_response_db(&grid, false);
    let rms_error = |measured: &[f64]| -> f64 {
        let (mut sum, mut used) = (0.0, 0usize);
        for (i, &f) in grid.iter().enumerate() {
            if f >= 100.0 && f <= 16_000.0 {
                let d = measured[i] - truth[i];
                sum += d * d;
                used += 1;
            }
        }
        (sum / used.max(1) as f64).sqrt()
    };
    let one = rms_error(&magnitude(&c, &s, Measure::default()));
    let four = rms_error(&magnitude(
        &c,
        &s,
        Measure {
            runs: 4,
            ..Default::default()
        },
    ));
    println!(
        "  0 dB SNR: one run {one:.3} dB RMS, four runs {four:.3} dB RMS ({:.1} dB better)",
        20.0 * (one / four).log10()
    );
    assert!(four < one * 0.75, "four runs should be clearly better than one");
}

// MARK: Microphone calibration

#[test]
fn microphone_calibration_is_removed() {
    let grid = standard_grid();
    let mut with_mic = SimulatedChain::standard(60.0);
    with_mic.mic_curve = vec![
        (10.0, 0.0),
        (100.0, 0.3),
        (500.0, -0.2),
        (1_000.0, 0.0),
        (3_000.0, 0.8),
        (5_000.0, 1.5),
        (10_000.0, 3.0),
        (15_000.0, 4.5),
        (20_000.0, 6.0),
    ];
    let without_mic = SimulatedChain::standard(60.0);
    // The calibration file states exactly what the microphone does.
    let mut text = vec!["\"Sens Factor =-1.20dB, SERNO: 7012345\"".to_string()];
    text.extend(
        with_mic
            .mic_curve
            .iter()
            .map(|(hz, db)| format!("{hz:.4}\t{db:.4}\t0.0")),
    );
    let mic = MicCalibration::parse(&text.join("\r\n"), "UMIK test", "").unwrap();
    near(mic.sens_factor_db.unwrap(), -1.2, 1e-9);
    assert!(mic.has_phase_column);

    let s = sweep(3.0, -6.0);
    let corrected = magnitude(
        &with_mic,
        &s,
        Measure {
            mic: Some(mic),
            ..Default::default()
        },
    );
    let clean = magnitude(&without_mic, &s, Measure::default());
    let worst = worst_difference(&corrected, &clean, &grid, 30.0, 16_000.0);
    println!("  microphone removal: worst {:.4} dB at {:.0} Hz", worst.0, worst.1);
    expect(worst.0, 0.05, "microphone correction residue");
    // And without the file the tilt is still in the answer.
    let uncorrected = magnitude(&with_mic, &s, Measure::default());
    assert!(worst_difference(&uncorrected, &clean, &grid, 30.0, 16_000.0).0 > 4.0);
}

// MARK: Distortion

fn distortion_of(chain: &SimulatedChain, s: &SweepSignal) -> analysis::HarmonicDistortion {
    let options = Options::default();
    let ir = analysis::impulse_response(&chain.record(s), s, &options);
    analysis::harmonic_distortion(&ir, s, &options)
}

#[test]
fn thd_estimate_matches_the_injected_second_harmonic() {
    let mut c = SimulatedChain::standard(70.0);
    c.second_harmonic = 0.01;
    let s = sweep(5.0, -6.0);
    let distortion = distortion_of(&c, &s);
    let second = distortion.per_harmonic_percent.get(&2).copied().unwrap_or(0.0);
    println!(
        "  injected 1.000 % second harmonic, measured {second:.3} % (total {:.3} %)",
        distortion.total_percent
    );
    expect(
        (second - 1.0).abs(),
        0.20,
        "relative error of the 2nd harmonic estimate",
    );
    // A clean chain reads near zero.
    let clean = distortion_of(&SimulatedChain::standard(70.0), &s);
    expect(clean.total_percent, 0.1, "distortion of a linear chain");
}

#[test]
fn third_harmonic_is_separated() {
    let mut c = SimulatedChain::standard(70.0);
    c.third_harmonic = 0.02;
    let distortion = distortion_of(&c, &sweep(5.0, -6.0));
    let third = distortion.per_harmonic_percent.get(&3).copied().unwrap_or(0.0);
    let second = distortion.per_harmonic_percent.get(&2).copied().unwrap_or(0.0);
    println!("  injected 2.000 % third harmonic, measured {third:.3} % (2nd reads {second:.3} %)");
    expect(
        (third - 2.0).abs() / 2.0,
        0.20,
        "relative error of the 3rd harmonic estimate",
    );
    expect(second, 0.3, "2nd harmonic leaking from the 3rd");
}

/// The point of the exponential sweep: distortion must not touch the linear response.
#[test]
fn harmonics_do_not_leak_into_the_linear_response() {
    let grid = standard_grid();
    let clean = SimulatedChain::standard(70.0);
    let mut distorted = SimulatedChain::standard(70.0);
    distorted.second_harmonic = 0.03;
    distorted.third_harmonic = 0.02;
    let s = sweep(5.0, -6.0);
    let a = magnitude(&clean, &s, Measure::default());
    let b = magnitude(&distorted, &s, Measure::default());
    let worst = worst_difference(&a, &b, &grid, 30.0, 16_000.0);
    println!(
        "  3 % 2nd + 2 % 3rd harmonic moves the curve by {:.4} dB at {:.0} Hz",
        worst.0, worst.1
    );
    expect(worst.0, 0.2, "harmonic leakage into the linear response");
}

// MARK: Clock drift

#[test]
fn twenty_ppm_drift_costs_less_than_a_decibel_in_the_top_octave() {
    let grid = standard_grid();
    let straight = SimulatedChain::standard(70.0);
    let mut drifting = SimulatedChain::standard(70.0);
    drifting.drift_ppm = 20.0;
    let s = sweep(5.0, -6.0);
    let a = magnitude(&straight, &s, Measure::default());
    let b = magnitude(&drifting, &s, Measure::default());
    let top = worst_difference(&a, &b, &grid, 10_000.0, 20_000.0);
    let rest = worst_difference(&a, &b, &grid, 30.0, 10_000.0);
    println!(
        "  20 ppm drift: top octave {:.3} dB at {:.0} Hz, below 10 kHz {:.3} dB",
        top.0, top.1, rest.0
    );
    expect(top.0, 1.0, "top-octave error from 20 ppm of clock drift");
    expect(rest.0, 0.3, "error below 10 kHz from 20 ppm of clock drift");
}

// MARK: Signal to noise

/// Ground truth for the SNR test: the noise that corrupted the recording is
/// found by subtraction of a noiseless run, and the analysis gets *that* as its
/// noise segment.
fn truth_snr_db(c: &SimulatedChain, s: &SweepSignal) -> Vec<f64> {
    let recorded = c.record(s);
    let mut quiet = c.clone();
    quiet.snr_db = None;
    let just_noise: Vec<f64> = recorded.iter().zip(&quiet.record(s)).map(|(a, b)| a - b).collect();
    let mut session = HeadphoneMeasurement::new(s.clone());
    session.add_run(&recorded, false);
    session.set_noise_segment(just_noise);
    to_f64(
        &session
            .result("truth", "Joseon measurement", None)
            .unwrap()
            .quality
            .snr_db,
    )
}

#[test]
fn snr_estimate_is_within_3_db() {
    let grid = standard_grid();
    let c = SimulatedChain::standard(30.0);
    let s = sweep(5.0, -6.0);
    let truth = truth_snr_db(&c, &s);
    let with_noise = || Measure {
        with_noise_segment: true,
        ..Default::default()
    };
    let estimate = to_f64(&measure(&c, &s, with_noise()).quality.snr_db);
    assert_eq!(estimate.len(), grid.len());
    let worst = worst_difference(&estimate, &truth, &grid, 30.0, 16_000.0);
    println!("  SNR estimate: worst {:.2} dB off at {:.0} Hz", worst.0, worst.1);
    expect(worst.0, 3.0, "per-band signal-to-noise estimate");
    // Ten dB more noise must read ten dB less signal-to-noise.
    let quieter = to_f64(
        &measure(&SimulatedChain::standard(20.0), &s, with_noise())
            .quality
            .snr_db,
    );
    let difference: Vec<f64> = estimate.iter().zip(&quieter).map(|(a, b)| a - b).collect();
    let drop = mean(&difference, &grid, 100.0, 10_000.0);
    println!("  10 dB more noise moves the estimate by {drop:.2} dB");
    expect((drop - 10.0).abs(), 1.5, "signal-to-noise tracks the noise level");
}

// MARK: Smoothing and window

#[test]
fn smoothing_widens_with_the_fraction() {
    let grid = standard_grid();
    let c = SimulatedChain::standard(60.0);
    let s = sweep(3.0, -6.0);
    let truth = c.true_response_db(&grid, false);
    let mut worse = Vec::new();
    for smoothing in [OctaveSmoothing::Twelfth, OctaveSmoothing::Sixth, OctaveSmoothing::Third] {
        let measured = magnitude(
            &c,
            &s,
            Measure {
                smoothing,
                ..Default::default()
            },
        );
        let worst = worst_difference(&measured, &truth, &grid, 30.0, 16_000.0);
        println!("  {}: worst {:.3} dB at {:.0} Hz", smoothing.as_str(), worst.0, worst.1);
        worse.push(worst.0);
    }
    // Broader smoothing rounds the 8 kHz notch off, so the error grows.
    assert!(worse[0] < worse[2]);
    expect(worse[2], 2.0, "1/3 octave smoothing error on a Q = 3 notch");
}

#[test]
fn window_rejects_what_comes_before_the_impulse() {
    let mut c = SimulatedChain::standard(60.0);
    c.second_harmonic = 0.05;
    let s = sweep(5.0, -6.0);
    let options = Options::default();
    let ir = analysis::impulse_response(&c.record(&s), &s, &options);
    let windowed = analysis::window(&ir, &options);
    let harmonic_offset = ir.harmonic_offset_samples[&2];
    assert!(
        harmonic_offset > windowed.pre_samples,
        "the 2nd harmonic must be outside the leading edge of the window"
    );
    assert!(windowed.pre_samples >= (0.004 * SAMPLE_RATE) as usize);
    assert!(windowed.samples.len() <= (0.305 * SAMPLE_RATE) as usize + windowed.pre_samples);
    assert!(windowed.post_samples >= (0.099 * SAMPLE_RATE) as usize);
    assert_eq!(windowed.peak_offset, windowed.pre_samples);
    near(
        windowed.lowest_resolved_hz(),
        SAMPLE_RATE / windowed.samples.len() as f64,
        1e-12,
    );
    near(ir.delay_seconds(), 3_733.0 / SAMPLE_RATE, 1e-12);
    near(ir.peak_sub_sample, 3_733.0, 1.0);
}
