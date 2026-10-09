//! Port of `ThirdOctaveSupport.swift`, `ThirdOctaveTests.swift` and
//! `ThirdOctaveNoiseTests.swift`: `SpectrumAnalyzer` as a third-octave band meter.
//!
//! Every level here is dBFS **RMS**: a full-scale sine reads −3.01.

use super::*;
use crate::spectrum::third_octave::{lower_edge_hz, upper_edge_hz, BAND_COUNT, FAST_TAU, MIN_BINS_PER_BAND};
use realfft::RealFftPlanner;

const BAND_FLOOR: f32 = ThirdOctaveReading::FLOOR_DB;
/// A full-scale sine reads this, by the convention of the contract.
const FULL_SCALE_SINE_DB: f32 = -3.0103;

/// Index of a nominal centre.
fn band(nominal_hz: f32) -> usize {
    ThirdOctaveReading::NOMINAL_CENTERS_HZ
        .iter()
        .position(|&hz| hz == nominal_hz)
        .unwrap_or_else(|| panic!("no third-octave band with nominal centre {nominal_hz}"))
}

/// Feeds whole 800-frame blocks and takes a reading after every block.
fn trace(
    analyzer: &mut SpectrumAnalyzer,
    left: &[f32],
    right: &[f32],
    rate: f64,
    mut each: impl FnMut(f64, Option<ThirdOctaveReading>),
) {
    assert_eq!(left.len(), right.len());
    let mut fed = 0;
    for (l, r) in left.chunks_exact(800).zip(right.chunks_exact(800)) {
        analyzer.process(l, r, rate);
        fed += 800;
        each(fed as f64 / rate, analyzer.third_octave());
    }
}

/// Feeds whole 800-frame blocks, the way the engine drives the analyzer.
fn feed_bands(analyzer: &mut SpectrumAnalyzer, left: &[f32], right: &[f32], rate: f64) -> Option<ThirdOctaveReading> {
    for (l, r) in left.chunks_exact(800).zip(right.chunks_exact(800)) {
        analyzer.process(l, r, rate);
    }
    analyzer.third_octave()
}

fn tone_bands(hz: f64, amplitude: f32, rate: f64, seconds: f64) -> (SpectrumAnalyzer, ThirdOctaveReading) {
    let mut analyzer = SpectrumAnalyzer::default();
    let tone = test_signals::sine(hz, amplitude, rate, seconds, 0.0);
    let reading = feed_bands(&mut analyzer, &tone, &tone, rate).expect("a reading");
    (analyzer, reading)
}

/// Power-averages the band readings taken after `from_seconds`. Returns
/// `(left, right)` in dBFS RMS.
///
/// One 125 ms reading of a 4.6 Hz-wide band scatters by a couple of dB: that is
/// the chi-square of the estimate, not an error in the band level. The average
/// of the *power* of many readings is unbiased.
fn average_levels(
    analyzer: &mut SpectrumAnalyzer,
    left: &[f32],
    right: &[f32],
    rate: f64,
    from_seconds: f64,
) -> (Vec<f64>, Vec<f64>) {
    let (mut sum_l, mut sum_r, mut n) = (vec![0.0f64; BAND_COUNT], vec![0.0f64; BAND_COUNT], 0);
    trace(analyzer, left, right, rate, |seconds, reading| {
        let Some(reading) = reading.filter(|_| seconds >= from_seconds) else {
            return;
        };
        n += 1;
        for b in 0..BAND_COUNT {
            sum_l[b] += 10f64.powf(reading.left[b] as f64 / 10.0);
            sum_r[b] += 10f64.powf(reading.right[b] as f64 / 10.0);
        }
    });
    assert!(n > 0, "no readings were taken");
    let level = |sum: &[f64]| sum.iter().map(|s| 10.0 * (s / n as f64).max(1e-300).log10()).collect();
    (level(&sum_l), level(&sum_r))
}

/// Mean square of a buffer: the "known RMS" the noise tests measure against.
fn mean_square(x: &[f32]) -> f64 {
    x.iter().map(|&v| v as f64 * v as f64).sum::<f64>() / x.len() as f64
}

// MARK: The map

/// Which FFT feeds which band, and that the per-bin weights add up to at least
/// the smallest band width.
#[test]
fn band_map_and_edge_weights() {
    for rate in [44_100.0, 48_000.0, 96_000.0] {
        let mut analyzer = SpectrumAnalyzer::default();
        let quiet = vec![0.0; 4_096];
        feed_bands(&mut analyzer, &quiet, &quiet, rate);
        let layers = analyzer.third_octave_bank.layer_assignment();
        for b in 0..BAND_COUNT {
            if layers[b].is_none() {
                continue;
            }
            let (_, count, weight_sum) = analyzer.third_octave_bank.footprint(b);
            assert!(count > 0);
            assert!(
                weight_sum as f64 >= MIN_BINS_PER_BAND - 1e-3,
                "band {} Hz has too few bins at {rate}",
                ThirdOctaveReading::NOMINAL_CENTERS_HZ[b]
            );
            // The weight sum is the band width in bins of that resolution.
            let resolution = &analyzer.resolutions().unwrap()[layers[b].unwrap()];
            let width_bins = (upper_edge_hz(b) - lower_edge_hz(b)) / resolution.df as f64;
            assert!(
                (weight_sum as f64 - width_bins).abs() < 0.01,
                "band {b} at {rate}: {weight_sum} vs {width_bins}"
            );
        }
        // The three groups are contiguous and ordered: long FFT at the bottom, short at the top.
        let used: Vec<usize> = layers.iter().flatten().copied().collect();
        assert!(
            used.windows(2).all(|pair| pair[0] <= pair[1]),
            "band -> resolution assignment is not monotone at {rate}"
        );
        if rate == 48_000.0 {
            // 20–63 Hz from the long FFT, 80–250 Hz from the middle one, the rest from the short one.
            assert_eq!(layers[band(63.0)], Some(0));
            assert_eq!(layers[band(80.0)], Some(1));
            assert_eq!(layers[band(250.0)], Some(1));
            assert_eq!(layers[band(315.0)], Some(2));
            assert_eq!(layers[band(20_000.0)], Some(2));
        }
    }
}

/// Bands above 0.9 of Nyquist report the floor. At 44.1 kHz that is the 20 kHz
/// band (exact centre 19 953 Hz, above 0.9 * 22 050 = 19 845 Hz). At 48 kHz
/// none of them are.
#[test]
fn bands_above_nyquist_report_the_floor() {
    let rate = 44_100.0;
    let mut analyzer = SpectrumAnalyzer::default();
    let noise = white(0.5, (rate * 2.0) as usize);
    let r = feed_bands(&mut analyzer, &noise, &noise, rate).unwrap();
    assert_eq!(r.left[band(20_000.0)], BAND_FLOOR);
    assert_eq!(r.right[band(20_000.0)], BAND_FLOOR);
    assert!(r.left[band(16_000.0)] > -80.0);

    let mut analyzer = SpectrumAnalyzer::default();
    let noise = white(0.5, 96_000);
    let r = feed_bands(&mut analyzer, &noise, &noise, 48_000.0).unwrap();
    assert!(r.left[band(20_000.0)] > -80.0, "48 kHz keeps the 20 kHz band");
}

// MARK: Tones

/// 1 kHz sine at −20 dBFS peak. RMS is 3.01 dB under the peak, so the band reads −23.01.
#[test]
fn one_kilohertz_sine() {
    let (_, r) = tone_bands(1_000.0, 0.1, 48_000.0, 3.0);
    let target = band(1_000.0);
    assert_near(r.left[target], -23.01, 0.2, "left");
    assert_near(r.right[target], -23.01, 0.2, "right");
    let (mut worst, mut worst_neighbour) = (f32::MIN, f32::MIN);
    for b in (0..BAND_COUNT).filter(|&b| b != target) {
        let v = r.left[b].max(r.right[b]);
        if b + 1 == target || b == target + 1 {
            worst_neighbour = worst_neighbour.max(v);
        } else {
            worst = worst.max(v);
        }
    }
    assert!(
        r.left[target] - worst > 40.0,
        "a non-neighbour band is within 40 dB of the tone"
    );
    assert!(
        r.left[target] - worst_neighbour > 40.0,
        "window leakage into the neighbour bands is over budget"
    );
}

/// The same for 50 Hz, which the 32768-point FFT serves.
#[test]
fn fifty_hertz_sine() {
    let (_, r) = tone_bands(50.0, 0.1, 48_000.0, 4.0);
    let target = band(50.0);
    assert_near(r.left[target], -23.01, 0.2, "left");
    assert_near(r.right[target], -23.01, 0.2, "right");
    let worst = (0..BAND_COUNT)
        .filter(|&b| b != target)
        .map(|b| r.left[b].max(r.right[b]))
        .fold(f32::MIN, f32::max);
    assert!(
        r.left[target] - worst > 40.0,
        "another band is {} dB down",
        r.left[target] - worst
    );
}

/// A full-scale sine reads −3.01, which is the calibration the contract names.
#[test]
fn full_scale_sine_reads_minus_three_db() {
    let (_, r) = tone_bands(1_000.0, 1.0, 48_000.0, 3.0);
    assert_near(r.left[band(1_000.0)], FULL_SCALE_SINE_DB, 0.2, "full scale");
}

/// Same answer at every sample rate the app supports.
#[test]
fn same_level_at_every_sample_rate() {
    let levels = [44_100.0, 48_000.0, 96_000.0].map(|rate| tone_bands(1_000.0, 0.1, rate, 3.0).1.left[band(1_000.0)]);
    for level in levels {
        assert_near(level, -23.01, 0.3, "1 kHz band");
    }
    let spread = levels.iter().copied().fold(f32::MIN, f32::max) - levels.iter().copied().fold(f32::MAX, f32::min);
    assert!(
        spread < 0.3,
        "the same tone reads differently at different sample rates: {levels:?}"
    );
}

/// A sample-rate change between calls reconfigures cleanly and the level comes back.
#[test]
fn sample_rate_change_reconfigures() {
    let (mut analyzer, _) = tone_bands(1_000.0, 0.1, 48_000.0, 3.0);
    let tone = test_signals::sine(1_000.0, 0.1, 96_000.0, 3.0, 0.0);
    let r = feed_bands(&mut analyzer, &tone, &tone, 96_000.0).unwrap();
    assert_near(r.left[band(1_000.0)], -23.01, 0.3, "after the rate change");
}

// MARK: Channels, silence, warm-up

/// A left-only signal leaves the right channel at the floor in every band.
#[test]
fn left_only_signal_leaves_right_at_the_floor() {
    let rate = 48_000.0;
    let mut analyzer = SpectrumAnalyzer::default();
    let tone = test_signals::sine(1_000.0, 0.5, rate, 3.0, 0.0);
    let r = feed_bands(&mut analyzer, &tone, &vec![0.0; tone.len()], rate).unwrap();
    assert_near(r.left[band(1_000.0)], -9.03, 0.2, "left");
    assert!(
        r.right.iter().all(|&v| v == BAND_FLOOR),
        "a right band is not at the floor"
    );
}

/// Digital silence: every band at the floor, nothing NaN or infinite.
#[test]
fn silence_is_the_floor_and_finite() {
    let rate = 48_000.0;
    let mut analyzer = SpectrumAnalyzer::default();
    let quiet = silence(rate, 2.0);
    let r = feed_bands(&mut analyzer, &quiet, &quiet, rate).unwrap();
    assert!(r.left.iter().chain(&r.right).all(|&v| v == BAND_FLOOR));
    assert_eq!(r.left.len(), BAND_COUNT);
    assert_eq!(r.centers_hz, ThirdOctaveReading::NOMINAL_CENTERS_HZ);
}

/// `None` until every resolution that serves a band has transformed once. The
/// 32768-point FFT is the last to fill, at 683 ms.
#[test]
fn none_until_every_resolution_has_run() {
    let rate = 48_000.0;
    let mut analyzer = SpectrumAnalyzer::default();
    assert!(analyzer.third_octave().is_none(), "a fresh analyzer must not answer");
    let tone = test_signals::sine(1_000.0, 0.5, rate, 1.5, 0.0);
    let mut first_answer = f64::INFINITY;
    trace(&mut analyzer, &tone, &tone, rate, |seconds, reading| {
        if reading.is_some() {
            first_answer = first_answer.min(seconds);
        }
    });
    // 683 ms of window, and the low FFT only transforms on its own hop grid
    // (5461 samples), so the first full window lands at the seventh hop, 0.796 s.
    assert!(
        first_answer > 0.68,
        "answered before the 683 ms window was full: {first_answer}"
    );
    assert!(first_answer < 0.9, "first answer at {first_answer} s");
}

// MARK: Time weighting

/// Fast weighting: a one-pole on band power with tau = 125 ms. A step from
/// silence is then `1 - exp(-t/tau)` in power, so it is within 2 dB of final at
/// t = 1.0 tau. Measured on a high band, where the 43 ms FFT window adds almost
/// nothing and the one-pole is what is seen.
#[test]
fn fast_time_constant_on_a_step() {
    let rate = 48_000.0;
    let mut analyzer = SpectrumAnalyzer::default();
    // Fill the FFTs with silence first, so the step is the only thing that moves.
    let quiet = silence(rate, 1.5);
    feed_bands(&mut analyzer, &quiet, &quiet, rate);
    let target = band(4_000.0);
    let tone = test_signals::sine(4_000.0, 0.5, rate, 4.0, 0.0);
    let mut samples = Vec::new();
    trace(&mut analyzer, &tone, &tone, rate, |seconds, reading| {
        if let Some(reading) = reading {
            samples.push((seconds, reading.left[target]));
        }
    });
    let last = samples.last().unwrap().1;
    assert_near(last, -9.03, 0.2, "the steady level of the step");
    let crossing = samples
        .iter()
        .find(|(_, level)| *level >= last - 2.0)
        .map_or(f64::INFINITY, |(t, _)| *t);
    // The theory value is 1.0 tau; the FFT window and the block size add a little.
    assert!(
        crossing < 2.3 * FAST_TAU,
        "the integrator is slower than fast weighting: {crossing} s"
    );
    assert!(
        crossing > 0.4 * FAST_TAU,
        "the integrator is not integrating at all: {crossing} s"
    );
    let late = samples.iter().find(|(t, _)| *t >= 2.3 * FAST_TAU).unwrap().1;
    assert!(late > last - 1.0);
}

/// `reset` clears the integrators: the bands drop to the floor and fill again.
#[test]
fn reset_clears_the_integrators() {
    let rate = 48_000.0;
    let (mut analyzer, before) = tone_bands(1_000.0, 0.5, rate, 3.0);
    assert_near(before.left[band(1_000.0)], -9.03, 0.2, "before reset");
    analyzer.reset();
    let cleared = analyzer.third_octave().unwrap();
    assert!(
        cleared.left.iter().chain(&cleared.right).all(|&v| v == BAND_FLOOR),
        "a band survived reset"
    );
    let tone = test_signals::sine(1_000.0, 0.5, rate, 3.0, 0.0);
    let again = feed_bands(&mut analyzer, &tone, &tone, rate).unwrap();
    assert_near(again.left[band(1_000.0)], -9.03, 0.2, "the meter refills after reset");
}

// MARK: The band normalisation, proved on noise
//
// A tone-calibrated analyzer can read a tone correctly and still be several dB
// wrong on broadband content. These tests pin the constant from the other side:
// white noise of a known RMS, checked as an absolute total and as a shape
// across the bands.
//
// `sum_b 10^(L_b / 10)` is the mean square of the signal inside the summed
// bands. For white noise of mean square `ms` spread flat over `0 ... fs/2`, the
// mean square inside `[f1, f2]` is `ms * (f2 - f1) / (fs / 2)`.

/// How long the noise runs. The 20 Hz band is 4.6 Hz wide, and a power estimate
/// over `T` seconds of a `B` Hz band scatters by `1 / sqrt(B T)` whatever the
/// analyzer does. Forty seconds gives `B T = 184` and 0.31 dB, so the tolerance
/// measures the analyzer and not the physics.
const NOISE_SECONDS: f64 = 40.0;

/// White noise: the power sum of the bands equals the noise power over the same
/// frequency range. An absolute check with no free constant.
#[test]
fn white_noise_power_sum_matches_the_noise_power() {
    for rate in [48_000.0, 96_000.0] {
        let mut analyzer = SpectrumAnalyzer::default();
        let noise = white(0.5, (rate * NOISE_SECONDS) as usize);
        let ms = mean_square(&noise);
        let (levels, _) = average_levels(&mut analyzer, &noise, &noise, rate, 1.0);
        // The bands that report at this rate.
        let layers = analyzer.third_octave_bank.layer_assignment();
        let used: Vec<usize> = (0..BAND_COUNT).filter(|&b| layers[b].is_some()).collect();
        let (lower, upper) = (lower_edge_hz(used[0]), upper_edge_hz(*used.last().unwrap()));
        let measured: f64 = used.iter().map(|&b| 10f64.powf(levels[b] / 10.0)).sum();
        let expected = ms * (upper - lower) / (rate / 2.0);
        let error_db = 10.0 * (measured / expected).log10();
        println!("white noise at {rate} Hz: band power sum error {error_db:.3} dB");
        assert!(
            error_db.abs() <= 0.3,
            "band power sum is off by {error_db} dB at {rate}"
        );
    }
}

/// White noise rises by 1 dB per third octave, because the band width does.
/// Checked against the absolute prediction per band, so a constant offset
/// cannot hide in it.
#[test]
fn white_noise_rises_one_db_per_third_octave() {
    let rate = 48_000.0;
    let mut analyzer = SpectrumAnalyzer::default();
    let noise = white(0.5, (rate * NOISE_SECONDS) as usize);
    let ms = mean_square(&noise);
    let (levels, _) = average_levels(&mut analyzer, &noise, &noise, rate, 1.0);
    let (mut worst, mut worst_band) = (0.0f64, 0);
    for b in 0..BAND_COUNT {
        let expected = 10.0 * (ms * (upper_edge_hz(b) - lower_edge_hz(b)) / (rate / 2.0)).log10();
        let error = levels[b] - expected;
        if error.abs() > worst.abs() {
            (worst, worst_band) = (error, b);
        }
    }
    println!(
        "white noise: worst band {} Hz, {worst:+.2} dB",
        ThirdOctaveReading::NOMINAL_CENTERS_HZ[worst_band]
    );
    assert!(
        worst.abs() < 0.7,
        "band {} Hz is {worst} dB off the white-noise slope",
        ThirdOctaveReading::NOMINAL_CENTERS_HZ[worst_band]
    );
    for b in 1..BAND_COUNT {
        let step = levels[b] - levels[b - 1];
        assert!(
            (step - 1.0).abs() <= 0.9,
            "step into band {} Hz is {step} dB",
            ThirdOctaveReading::NOMINAL_CENTERS_HZ[b]
        );
    }
}

/// White noise shaped to an exact `1/f` power with one inverse FFT. Deterministic.
///
/// The table is longer than the test runs, so it never repeats: a repeat makes
/// the signal periodic, and its line spacing has to stay well under the 1.46 Hz
/// bins of the 32768-point FFT.
fn exact_pink_noise(count: usize, amplitude: f32) -> Vec<f32> {
    let n = 1 << 21;
    assert!(count <= n);
    let mut table = test_signals::white_noise(1.0, n, 0x5049_4E4B);
    let mut planner = RealFftPlanner::<f32>::new();
    let mut spectrum = planner.plan_fft_forward(n).make_output_vec();
    planner.plan_fft_forward(n).process(&mut table, &mut spectrum).unwrap();
    // Amplitude ~ 1 / sqrt(f) gives power ~ 1 / f, which is constant power per
    // octave and so constant power per third octave.
    let last = spectrum.len() - 1;
    for (k, bin) in spectrum.iter_mut().enumerate() {
        *bin = if k == 0 || k == last {
            Default::default()
        } else {
            *bin / (k as f32).sqrt()
        };
    }
    planner.plan_fft_inverse(n).process(&mut spectrum, &mut table).unwrap();
    let peak = table.iter().fold(1e-9f32, |peak, v| peak.max(v.abs()));
    table.truncate(count);
    table.iter_mut().for_each(|v| *v *= amplitude / peak);
    table
}

/// Pink noise is flat across the bands: constant power per third octave is what
/// "pink" means. `test_signals::pink_noise` is only pink over a finite range,
/// so the test builds an exact reference. Flatness is then a property of the
/// analyzer alone.
#[test]
fn pink_noise_is_flat_across_bands() {
    let rate = 48_000.0;
    let mut analyzer = SpectrumAnalyzer::default();
    let noise = exact_pink_noise((rate * NOISE_SECONDS) as usize, 0.3);
    let (levels, _) = average_levels(&mut analyzer, &noise, &noise, rate, 1.0);
    let mean = levels.iter().sum::<f64>() / BAND_COUNT as f64;
    let (mut worst, mut worst_band) = (0.0f64, 0);
    for b in 0..BAND_COUNT {
        if (levels[b] - mean).abs() > worst.abs() {
            (worst, worst_band) = (levels[b] - mean, b);
        }
    }
    println!(
        "pink flatness: worst {worst:+.2} dB at {} Hz",
        ThirdOctaveReading::NOMINAL_CENTERS_HZ[worst_band]
    );
    assert!(
        worst.abs() < 1.0,
        "pink noise is not flat across the bands: {worst} dB at band {worst_band}"
    );
}

/// Left and right are measured independently: two different noises, two different answers.
#[test]
fn channels_are_independent() {
    let rate = 48_000.0;
    let mut analyzer = SpectrumAnalyzer::default();
    let count = (rate * 4.0) as usize;
    let (left, right) = (
        test_signals::white_noise(0.5, count, 1),
        test_signals::white_noise(0.25, count, 2),
    );
    let (level_l, level_r) = average_levels(&mut analyzer, &left, &right, rate, 1.0);
    let target = band(1_000.0);
    let difference = level_l[target] - level_r[target];
    assert!(
        (difference - 6.02).abs() <= 0.3,
        "left − right at 1 kHz is {difference} dB"
    );
}
