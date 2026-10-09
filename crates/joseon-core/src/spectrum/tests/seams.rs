//! Port of `SpectrumSeamTests.swift`: the displayed noise floor is one
//! continuous curve, and the bottom two octaves follow the data.
//!
//! The fixes these tests hold in place are a power-domain `noise + tone` curve,
//! an unbiased fill under a removed lobe, a fractional bandwidth ramp, a power
//! *mean* where a display bin holds several FFT bins, and a constant-Q
//! smoothing of the noise curve only.

use super::*;

/// Display bins that are part of a tonal peak: within 1/6 octave of any bin
/// that stands 6 dB above the median of the curve in a half-octave window
/// around it. The bar is about the noise floor between the peaks, not about the
/// peaks themselves.
fn tonal_mask(values: &[f32], frequencies: &[f32]) -> Vec<bool> {
    let n = values.len();
    let quarter = 2f32.powf(0.25);
    let mut is_peak = vec![false; n];
    let (mut lo, mut hi) = (0, 0);
    let mut window = Vec::new();
    for i in 0..n {
        let f = frequencies[i];
        while lo < n && frequencies[lo] < f / quarter {
            lo += 1;
        }
        while hi < n && frequencies[hi] <= f * quarter {
            hi += 1;
        }
        if hi <= lo {
            continue;
        }
        window.clear();
        window.extend_from_slice(&values[lo..hi]);
        window.sort_by(f32::total_cmp);
        if values[i] > window[window.len() / 2] + 6.0 {
            is_peak[i] = true;
        }
    }
    let mut mask = vec![false; n];
    for i in (0..n).filter(|&i| is_peak[i]) {
        for j in 0..n {
            if (frequencies[j] / frequencies[i]).log2().abs() <= 1.0 / 6.0 {
                mask[j] = true;
            }
        }
    }
    mask
}

/// The largest step between neighbour bins from `from` to `to` Hz, and where.
fn worst_step(values: &[f32], frequencies: &[f32], from: f32, to: f32, mask: &[bool]) -> (f32, f32) {
    let mut worst = (0.0f32, 0.0f32);
    for i in 1..values.len() {
        if !(frequencies[i] >= from && frequencies[i] <= to) || mask[i] || mask[i - 1] {
            continue;
        }
        let step = (values[i] - values[i - 1]).abs();
        if step > worst.0 {
            worst = (step, frequencies[i]);
        }
    }
    worst
}

fn assert_no_step(reading: &SpectrumReading, rate: f64) {
    for (name, curve) in [
        ("mid", &reading.mid),
        ("left", &reading.left),
        ("right", &reading.right),
    ] {
        let mask = tonal_mask(curve, &reading.frequencies);
        let (step, hz) = worst_step(curve, &reading.frequencies, 100.0, 5_000.0, &mask);
        assert!(step <= 1.5, "{name} at {rate} Hz: {step} dB step at {hz} Hz");
    }
}

// MARK: 1. No seam

/// Pink noise alone: every display bin from 100 Hz to 5 kHz is a noise-floor
/// bin, so the whole range has to be continuous. Both crossovers (200 Hz,
/// 2 kHz) and both bandwidth ramps are inside it.
#[test]
fn noise_floor_has_no_step_at_any_sample_rate() {
    for rate in [44_100.0, 48_000.0, 96_000.0] {
        let mut analyzer = SpectrumAnalyzer::default();
        let count = (rate * 4.0) as usize;
        let left = test_signals::pink_noise(0.5, count, 0x5EED);
        let right = test_signals::pink_noise(0.5, count, 0xC0FFEE);
        feed(&mut analyzer, &left, &right, rate);
        assert_no_step(&analyzer.read().spectrum, rate);
    }
}

/// Pink noise under the demo signal: the same bar, with the demo's partials and
/// their skirts excluded.
#[test]
fn demo_signal_noise_floor_has_no_step() {
    for rate in [44_100.0, 48_000.0, 96_000.0] {
        let mut analyzer = SpectrumAnalyzer::default();
        let count = (rate * 4.0) as usize;
        let (demo_left, demo_right) = test_signals::demo_block(0, count, rate);
        let left = add(&demo_left, &test_signals::pink_noise(0.05, count, 0x5EED));
        let right = add(&demo_right, &test_signals::pink_noise(0.05, count, 0xC0FFEE));
        feed(&mut analyzer, &left, &right, rate);
        assert_no_step(&analyzer.read().spectrum, rate);
    }
}

/// The analysis bandwidth has to be a continuous function of frequency, or the
/// long-term average, which has no frame-to-frame scatter left in it, shows the
/// ledge: one step per integer tap count.
#[test]
fn long_term_average_is_smooth() {
    let rate = 48_000.0;
    let mut analyzer = SpectrumAnalyzer::default();
    let count = (rate * 8.0) as usize;
    feed(
        &mut analyzer,
        &test_signals::pink_noise(0.5, count, 0x11),
        &test_signals::pink_noise(0.5, count, 0x22),
        rate,
    );
    let reading = analyzer.read().spectrum;
    let mask = vec![false; reading.average.len()];
    let (step, hz) = worst_step(&reading.average, &reading.frequencies, 30.0, 15_000.0, &mask);
    assert!(step <= 0.8, "average stepped {step} dB at {hz} Hz");
}

// MARK: 2. The bottom two octaves follow the data

fn longest_run(reading: &SpectrumReading, from: f32, to: f32) -> usize {
    let (mut run, mut longest, mut previous) = (1, 1, f32::NAN);
    for (f, &v) in reading.frequencies.iter().zip(&reading.mid) {
        if !(*f >= from && *f <= to) {
            continue;
        }
        if v == previous {
            run += 1;
            longest = longest.max(run);
        } else {
            run = 1;
        }
        previous = v;
    }
    longest
}

/// A 30 Hz and a 60 Hz tone. Below the 30 Hz peak the curve has to fall away,
/// not sit on a shelf.
#[test]
fn low_end_follows_a_tone_pair() {
    let rate = 48_000.0;
    let mut analyzer = SpectrumAnalyzer::default();
    let mixed = add(
        &test_signals::sine(30.0, 0.5, rate, 3.0, 0.0),
        &test_signals::sine(60.0, 0.25, rate, 3.0, 0.0),
    );
    feed_mono(&mut analyzer, &mixed, rate);
    let reading = analyzer.read().spectrum;
    let value = |hz: f32| peak_near_in(&reading.mid, &reading.frequencies, hz, 0.02);

    assert_near(value(30.0), -6.0, 0.5, "30 Hz tone");
    assert_near(value(60.0), -12.0, 0.5, "60 Hz tone");
    // 20 Hz is two thirds of an octave below the 30 Hz tone, far outside its main lobe.
    assert!(
        value(20.0) < value(30.0) - 40.0,
        "20 Hz sits on the skirt of the 30 Hz tone, not on a shelf"
    );
    // The skirt has to keep falling all the way down, not flatten out.
    assert!(value(20.0) < value(24.0) - 5.0, "curve is flat between 20 and 24 Hz");
    // No plateau: a run of display bins with the same value is a staircase.
    let longest = longest_run(&reading, 20.0, 90.0);
    assert!(
        longest <= 3,
        "plateau of {longest} identical display bins between 20 and 90 Hz"
    );
}

/// Pink noise: the bottom two octaves have to show the FFT's own structure. A
/// 32768-point window at 48 kHz resolves 1.5 Hz, so 20–40 Hz is 13 independent
/// measurements and the curve moves by several dB across it.
#[test]
fn low_end_follows_noise() {
    for rate in [44_100.0, 48_000.0, 96_000.0] {
        let mut analyzer = SpectrumAnalyzer::default();
        feed_mono(
            &mut analyzer,
            &test_signals::pink_noise(0.5, (rate * 3.0) as usize, 0xBEEF),
            rate,
        );
        let reading = analyzer.read().spectrum;
        let in_range: Vec<f32> = reading
            .frequencies
            .iter()
            .zip(&reading.mid)
            .filter(|(f, _)| **f >= 20.0 && **f <= 40.0)
            .map(|(_, &v)| v)
            .collect();
        let lo = in_range.iter().copied().fold(f32::MAX, f32::min);
        let hi = in_range.iter().copied().fold(f32::MIN, f32::max);
        assert!(hi - lo > 2.0, "20-40 Hz is flat to within {} dB at {rate} Hz", hi - lo);
        let longest = longest_run(&reading, 20.0, 40.0);
        assert!(longest <= 3, "plateau of {longest} identical display bins at {rate} Hz");
    }
}

/// Below the first usable FFT bin there is no data at all. The curve fades
/// towards the floor and does not hold the value of bin 1 across the range.
#[test]
fn below_the_first_fft_bin_fades_to_the_floor() {
    let rate = 48_000.0;
    // The first bin of the 32768-point window is 1.46 Hz.
    let settings = SpectrumSettings {
        min_hz: 0.2,
        max_hz: 20_000.0,
        ..Default::default()
    };
    let mut analyzer = SpectrumAnalyzer::new(settings);
    feed_mono(&mut analyzer, &pink(0.5, (rate * 2.0) as usize), rate);
    let reading = analyzer.read().spectrum;

    let (mut previous, mut rising) = (f32::MIN, 0);
    for (f, &v) in reading.frequencies.iter().zip(&reading.mid) {
        if *f < 1.4 {
            if v > previous + 0.001 {
                rising += 1;
            }
            previous = v;
        }
    }
    let bottom = reading.mid[0];
    let edge = reading.mid[reading.frequencies.iter().position(|&f| f >= 1.46).unwrap()];
    assert!(
        bottom < edge - 20.0,
        "0.2 Hz should be far below the first real bin, not a plateau"
    );
    assert!(rising > 0, "the fade should rise towards the first real bin");
}
