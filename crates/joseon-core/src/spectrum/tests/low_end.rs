//! Port of `SpectrumLowEndTests.swift`: is the flat part of the demo render
//! below 27 Hz drawn by the analyzer, or is it in the signal? These tests
//! compare the curve with the continuous spectrum of the same window, computed
//! directly, and check that DC does not leak into the audible range.

use super::*;
use std::f64::consts::PI;

/// Power of the Hann-windowed signal at an arbitrary frequency, calibrated like
/// the analyzer (a full-scale sine reads 1.0 when its main lobe is summed).
fn windowed_power(x: &[f32], hz: f64, rate: f64) -> f64 {
    let n = x.len() as f64;
    let (mut re, mut im, mut sum_squares) = (0.0, 0.0, 0.0);
    for (i, &sample) in x.iter().enumerate() {
        let w = 0.5 - 0.5 * (2.0 * PI * i as f64 / n).cos();
        let phase = 2.0 * PI * hz * i as f64 / rate;
        re += sample as f64 * w * phase.cos();
        im -= sample as f64 * w * phase.sin();
        sum_squares += w * w;
    }
    4.0 * (re * re + im * im) / (n * sum_squares)
}

/// The log display grid from 10 Hz to 24 kHz and its step ratio.
pub(super) fn grid(bins: usize) -> (Vec<f32>, f32) {
    let ratio = 2400.0f32;
    let step = ratio.powf(1.0 / (bins - 1) as f32);
    (
        (0..bins)
            .map(|i| 10.0 * ratio.powf(i as f32 / (bins - 1) as f32))
            .collect(),
        step,
    )
}

#[test]
fn demo_curve_below_40_hz_is_the_spectrum_of_the_window() {
    let (frequencies, step) = grid(1024);
    let size = 32_768;
    let df = 48_000.0 / size as f64;
    let mut resolution = SpectrumResolution::new(size, size / 6, 48_000.0).unwrap();
    let weight: Vec<f32> = frequencies.iter().map(|&f| if f < 300.0 { 1.0 } else { 0.0 }).collect();
    let df32 = df as f32;
    resolution.update_map(&frequencies, step, &weight, &|_| 3.0 * df32, &|_| df32);

    let mut worst = 0.0f64;
    for end_seconds in [7.3, 7.6, 8.0] {
        let end = (end_seconds * 48_000.0) as i64;
        let (left, right) = test_signals::demo_block(end - size as i64, size, 48_000.0);
        resolution.transform(&left, &right, 0);
        let mid: Vec<f32> = left.iter().zip(&right).map(|(l, r)| (l + r) / 2.0).collect();
        for (i, &f) in frequencies.iter().enumerate() {
            if !(f >= 20.0 && f <= 40.0 && i % 4 == 0) {
                continue;
            }
            // The noise curve is a 3-bin power sum: the same sum on the continuous spectrum.
            let sum: f64 = (-1..=1)
                .map(|j| windowed_power(&mid, f as f64 + j as f64 * df, 48_000.0))
                .sum();
            let reference = 10.0 * sum.log10();
            let shown = resolution.display_db[CH_MID * frequencies.len() + i] as f64;
            worst = worst.max((shown - reference).abs());
        }
    }
    assert!(
        worst <= 3.0,
        "curve and spectrum differ by {worst} dB between 20 and 40 Hz"
    );
}

/// A DC offset of 0.01 (−40 dBFS, far over the noise) must not show anywhere in
/// the plot. A periodic Hann window puts DC into bins 0 and 1 only, and its
/// sidelobes are exact zeros at every other bin, so no DC blocker is needed in
/// front of the long FFT. This test is what would say otherwise.
#[test]
fn dc_offset_does_not_lift_the_low_end() {
    for rate in [44_100.0, 48_000.0, 96_000.0] {
        let noise = test_signals::pink_noise(0.02, (rate * 3.0) as usize, 0xD0C);
        let offset: Vec<f32> = noise.iter().map(|x| x + 0.01).collect();
        let curve = |x: &[f32]| {
            let mut analyzer = SpectrumAnalyzer::default();
            feed_mono(&mut analyzer, x, rate);
            analyzer.read().spectrum
        };
        let (clean, dirty) = (curve(&noise), curve(&offset));
        let (mut worst, mut worst_hz) = (0.0f32, 0.0f32);
        for (i, &f) in clean.frequencies.iter().enumerate() {
            if f < 10.0 {
                continue;
            }
            for (a, b) in [(clean.mid[i], dirty.mid[i]), (clean.left[i], dirty.left[i])] {
                if (a - b).abs() > worst {
                    (worst, worst_hz) = ((a - b).abs(), f);
                }
            }
        }
        assert!(
            worst <= 0.5,
            "DC moved the curve by {worst} dB at {worst_hz} Hz, rate {rate}"
        );
    }
}

/// The long-term line counts time per display bin, from the moment the FFT that
/// feeds the bin has data. With one clock for all bins, the bins under 40 Hz
/// averaged the floor for the 0.8 s before the long FFT had data, and the
/// long-term line of a steady 30 Hz sine read about 5 dB low after 1.12 s.
#[test]
fn long_term_line_matches_the_live_line_for_a_steady_thirty_hertz_sine() {
    let rate = 48_000.0;
    let x = sine(30.0, -12.0, rate, 1.12);
    let mut analyzer = SpectrumAnalyzer::default();
    feed_mono(&mut analyzer, &x, rate);
    let reading = analyzer.read().spectrum;
    let live = peak_near_in(&reading.mid, &reading.frequencies, 30.0, 0.05);
    let average = peak_near_in(&reading.average, &reading.frequencies, 30.0, 0.05);
    assert_near(live, -12.0, 1.0, "the live line reads the sine");
    assert_near(
        average,
        live,
        0.5,
        "the long-term line of a steady tone is the live line",
    );

    // A bin that has no measured time yet reads the floor, not a number.
    let mut early = SpectrumAnalyzer::default();
    early.process(&x[..800], &x[..800], rate);
    let first = early.read().spectrum;
    for (f, &v) in first.frequencies.iter().zip(&first.average) {
        if *f < 40.0 {
            assert_near(v, FLOOR, 0.01, &format!("{f} Hz before the long FFT has data"));
        }
    }
}

/// The shelf from 20 to 27 Hz in the demo render is the kick drum of the demo
/// signal: a decaying sine exp(-a t) sin(w t) has a flat spectrum below its
/// resonance, 20 log10(2a / w) under the peak, which is −18.5 dB for a = 18 /s
/// and w = 2π·48 Hz. Checked on the kick alone, against that closed form.
#[test]
fn kick_alone_has_its_plateau_below_resonance() {
    let rate = 48_000.0;
    let mut analyzer = SpectrumAnalyzer::default();
    let block = 800;
    for k in 0..60 * 4 {
        let x: Vec<f32> = (0..block)
            .map(|i| {
                let beat = ((k * block + i) as f64 / rate) % 0.5;
                ((-beat * 18.0).exp() * (2.0 * PI * 48.0 * beat).sin()) as f32 * 0.5
            })
            .collect();
        analyzer.process(&x, &x, rate);
        analyzer.read();
    }
    let reading = analyzer.read().spectrum;
    let level = |hz: f32| reading.average[reading.frequencies.iter().position(|&f| f >= hz).unwrap()];
    let peak = reading
        .frequencies
        .iter()
        .zip(&reading.average)
        .filter(|(f, _)| **f > 30.0 && **f < 70.0)
        .fold(f32::MIN, |best, (_, &v)| best.max(v));
    let expected = 20.0 * ((2.0 * 18.0 / (2.0 * PI * 48.0)) as f32).log10();
    for hz in [20.0, 22.0, 24.0] {
        let under = level(hz) - peak;
        assert_near(under, expected, 4.0, &format!("{hz} Hz under the kick's peak"));
    }
}
