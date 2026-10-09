//! Tests of the loudness meter, ported from `LoudnessFilterTests.swift`,
//! `LoudnessMeterTests.swift`, `LoudnessTruePeakBalanceTests.swift` and the
//! loudness half of `ContractFieldTests.swift`.
//!
//! EBU Tech 3341 / 3342 style: synthetic signals only, nothing read from disk.
//! The time budget is in `tests/performance.rs`.

use super::histogram::{self, LoudnessHistogram};
use super::k_weighting::{self, KWeightingFilter};
use super::true_peak::{factor_for_sample_rate, TruePeakDetector, TAPS_PER_PHASE};
use super::*;
use crate::test_signals;
use std::f64::consts::PI;

const SILENCE: f32 = LoudnessReading::SILENCE_LUFS;

/// Peak amplitude for a level in dBFS, where 0 dBFS is a full-scale sine.
fn amplitude(dbfs: f64) -> f32 {
    10f64.powf(dbfs / 20.0) as f32
}

fn sine(hz: f64, dbfs: f64, rate: f64, seconds: f64) -> Vec<f32> {
    test_signals::sine(hz, amplitude(dbfs), rate, seconds, 0.0)
}

/// One continuous sine whose level follows `segments` of (dBFS, seconds). The
/// phase never jumps, so no segment boundary adds a click.
fn stepped_sine(hz: f64, rate: f64, segments: &[(f64, f64)]) -> Vec<f32> {
    let total: f64 = segments.iter().map(|segment| segment.1).sum();
    let mut out = test_signals::sine(hz, 1.0, rate, total, 0.0);
    let mut start = 0;
    for &(dbfs, seconds) in segments {
        let end = (start + (rate * seconds) as usize).min(out.len());
        out[start..end].iter_mut().for_each(|x| *x *= amplitude(dbfs));
        start = end;
    }
    let last = amplitude(segments.last().map_or(0.0, |segment| segment.0));
    out[start..].iter_mut().for_each(|x| *x *= last);
    out
}

/// Pushes a signal through the meter in blocks.
fn feed_blocks(meter: &mut LoudnessMeter, left: &[f32], right: &[f32], rate: f64, block: usize) {
    assert_eq!(left.len(), right.len());
    for (l, r) in left.chunks(block).zip(right.chunks(block)) {
        meter.process(l, r, rate);
    }
}

/// Pushes a signal through the meter in capture-sized blocks of 800 frames.
fn feed(meter: &mut LoudnessMeter, left: &[f32], right: &[f32], rate: f64) {
    feed_blocks(meter, left, right, rate, 800);
}

fn feed_mono(meter: &mut LoudnessMeter, signal: &[f32], rate: f64) {
    feed(meter, signal, signal, rate);
}

/// The reading after a mono signal went through a new meter.
fn measured(signal: &[f32], rate: f64) -> LoudnessReading {
    let mut meter = LoudnessMeter::new();
    feed_mono(&mut meter, signal, rate);
    meter.read()
}

#[track_caller]
fn assert_close(value: f32, expected: f64, tolerance: f64, label: &str) {
    assert!(value.is_finite(), "{label} is not finite: {value}");
    assert!(
        (value as f64 - expected).abs() <= tolerance,
        "{label}: got {value}, want {expected} ±{tolerance}"
    );
}

// MARK: K-weighting

/// The derivation must reproduce the table printed in ITU-R BS.1770-4 at
/// 48 kHz. If it does not, the per-rate coefficients are wrong everywhere.
#[test]
fn k_weighting_matches_bs1770_table_at_48k() {
    let near = |a: f64, b: f64, accuracy: f64| assert!((a - b).abs() <= accuracy, "{a} vs {b}");
    let shelf = k_weighting::shelf(48_000.0);
    near(shelf.b0, 1.53512485958697, 1e-12);
    near(shelf.b1, -2.69169618940638, 1e-12);
    near(shelf.b2, 1.19839281085285, 1e-12);
    near(shelf.a1, -1.69065929318241, 1e-12);
    near(shelf.a2, 0.73248077421585, 1e-12);
    let high_pass = k_weighting::high_pass(48_000.0);
    assert_eq!((high_pass.b0, high_pass.b1, high_pass.b2), (1.0, -2.0, 1.0));
    near(high_pass.a1, -1.99004745483398, 1e-11);
    near(high_pass.a2, 0.99007225036621, 1e-11);
}

fn k_gain_db(hz: f64, rate: f64) -> f64 {
    let mut filter = KWeightingFilter::new();
    filter.configure(rate);
    20.0 * filter.magnitude(hz, rate).log10()
}

/// The +0.691 dB of K-weighting gain at 1 kHz is what cancels the −0.691
/// offset, so a −23 dBFS tone reads −23 LUFS. It must hold at every rate.
#[test]
fn k_weighting_gain_at_one_kilohertz() {
    for rate in [44_100.0, 48_000.0, 88_200.0, 96_000.0, 192_000.0] {
        let gain = k_gain_db(1_000.0, rate);
        assert!((gain - 0.691).abs() <= 0.03, "K gain at 1 kHz, {rate} Hz: {gain}");
    }
}

/// Shape checks: the RLB stage cuts the deep bass, the shelf lifts the top.
#[test]
fn k_weighting_shape() {
    let at = |hz: f64| k_gain_db(hz, 48_000.0);
    assert!(at(20.0) < -10.0);
    assert!(at(100.0) < at(1_000.0));
    assert!(at(10_000.0) > at(1_000.0) + 3.0);
    assert!((at(100.0) + 0.6).abs() <= 0.6);
}

/// A 60 s DC offset is the worst case for the high-pass: the output must decay
/// to zero and stay finite.
#[test]
fn k_weighting_is_stable_over_long_runs() {
    let mut filter = KWeightingFilter::new();
    filter.configure(48_000.0);
    let mut last = 0.0;
    for _ in 0..48_000 * 60 {
        last = filter.step(1.0);
    }
    assert!(last.is_finite());
    assert!(last.abs() <= 1e-6, "output after 60 s of DC: {last}");
}

/// `step` and `sum_of_squares` are the same filter.
#[test]
fn step_and_sum_of_squares_agree() {
    let noise = test_signals::white_noise(0.5, 4_000, test_signals::DEFAULT_SEED);
    let (mut by_step, mut by_block) = (KWeightingFilter::new(), KWeightingFilter::new());
    by_step.configure(48_000.0);
    by_block.configure(48_000.0);
    let mut expected = 0.0;
    for &x in &noise {
        let y = by_step.step(x as f64);
        expected += y * y;
    }
    let sum: f64 = noise.chunks(333).map(|chunk| by_block.sum_of_squares(chunk)).sum();
    assert!((sum - expected).abs() <= expected * 1e-12, "{sum} vs {expected}");
}

// MARK: True peak interpolator

#[test]
fn oversampling_factor_per_sample_rate() {
    let factors =
        [44_100.0, 48_000.0, 96_000.0, 176_400.0, 192_000.0, 384_000.0, 768_000.0].map(factor_for_sample_rate);
    assert_eq!(factors, [4, 4, 4, 2, 2, 1, 1]);
}

/// Phase 0 is an exact unit impulse, so the true peak can never read below the
/// sample peak, and every branch has unity gain at DC.
#[test]
fn polyphase_branch_properties() {
    for factor in [2, 4] {
        let mut detector = TruePeakDetector::new();
        detector.configure(factor);
        let phase0 = detector.phase_coefficients(0);
        assert!(
            (phase0.iter().sum::<f32>() - 1.0).abs() <= 1e-6,
            "phase 0 DC gain, factor {factor}"
        );
        assert_eq!(
            phase0.iter().filter(|&&tap| tap != 0.0).count(),
            1,
            "phase 0 is not an impulse, factor {factor}"
        );
        for p in 1..factor {
            let sum: f32 = detector.phase_coefficients(p).iter().sum();
            assert!((sum - 1.0).abs() <= 1e-5, "phase {p} DC gain, factor {factor}: {sum}");
        }
    }
}

/// The meter feeds whole blocks. The tests feed single samples. Both paths
/// must give the same answer.
#[test]
fn block_and_sample_detection_agree() {
    let noise = test_signals::white_noise(0.9, 5_000, test_signals::DEFAULT_SEED);
    let (mut by_sample, mut by_block) = (TruePeakDetector::new(), TruePeakDetector::new());
    let sample_wise = noise.iter().fold(0.0f32, |peak, &v| peak.max(by_sample.push(v)));
    let block_wise = noise
        .chunks(333)
        .fold(0.0f32, |peak, chunk| peak.max(by_block.max_abs(chunk)));
    assert_eq!(block_wise, sample_wise);
}

#[test]
fn detector_never_reads_below_sample_peak() {
    let mut detector = TruePeakDetector::new();
    let noise = test_signals::white_noise(0.8, 20_000, test_signals::DEFAULT_SEED);
    let true_peak = noise.iter().fold(0.0f32, |peak, &v| peak.max(detector.push(v)));
    let sample_peak = noise.iter().fold(0.0f32, |peak, v| peak.max(v.abs()));
    assert!(true_peak >= sample_peak - 1e-6);
    // Band-limited noise really does overshoot between samples.
    assert!(true_peak > sample_peak);
}

/// The true peak in dBTP of a full-scale sine, with the start-up of the filter left out.
fn sine_true_peak_db(hz: f64, phase: f64) -> f64 {
    let mut detector = TruePeakDetector::new();
    let signal = test_signals::sine(hz, 1.0, 48_000.0, 0.05, phase);
    let mut peak = 0.0f32;
    for (i, &v) in signal.iter().enumerate() {
        let p = detector.push(v);
        if i >= TAPS_PER_PHASE {
            peak = peak.max(p);
        }
    }
    20.0 * (peak as f64).log10()
}

/// A full-scale sine has a true peak of exactly 0 dBTP at any frequency. 4x
/// oversampling can under-read near Nyquist (the grid is 1/8 sample coarse)
/// but must never over-read.
#[test]
fn detector_accuracy_across_frequency() {
    for fraction in [0.01, 0.05, 0.1, 0.2, 0.25, 0.3, 0.35, 0.4] {
        for step in 0..8 {
            let phase = step as f64 * PI / 8.0;
            let dbtp = sine_true_peak_db(fraction * 48_000.0, phase);
            assert!(dbtp <= 0.02, "over-read at {fraction} fs, phase {phase}: {dbtp} dBTP");
            assert!(dbtp > -0.5, "under-read at {fraction} fs, phase {phase}: {dbtp} dBTP");
        }
    }
}

#[test]
fn detector_reconstructs_quarter_rate_peak_exactly() {
    let dbtp = sine_true_peak_db(12_000.0, PI / 4.0);
    assert!(dbtp.abs() <= 0.05, "{dbtp} dBTP");
}

// MARK: Histogram

fn power_of(lufs: f64) -> f64 {
    10f64.powf((lufs - LOUDNESS_OFFSET) / 10.0)
}

#[test]
fn histogram_gating_and_percentiles() {
    let mut histogram = LoudnessHistogram::new();
    for (lufs, times) in [(-20.0, 100), (-30.0, 100), (-80.0, 50)] {
        for _ in 0..times {
            histogram.add(lufs, power_of(lufs));
        }
    }
    // −80 is below the absolute gate: it must be ignored.
    assert_eq!(histogram.total_count(), 200);
    let low = histogram.percentile(0.10, -70.0).unwrap();
    let high = histogram.percentile(0.95, -70.0).unwrap();
    assert!((low + 30.0).abs() <= histogram::BIN_WIDTH, "10th percentile {low}");
    assert!((high + 20.0).abs() <= histogram::BIN_WIDTH, "95th percentile {high}");
    assert_eq!(histogram.gated(-25.0).0, 100);
}

#[test]
fn empty_histogram_is_safe() {
    let histogram = LoudnessHistogram::new();
    assert_eq!(histogram.total_count(), 0);
    assert!(histogram.percentile(0.5, -70.0).is_none());
    assert_eq!(histogram.gated(-70.0).0, 0);
}

/// Hours of listening must cost the same memory as one second: the gated
/// measures live in fixed-size histograms, never in a growing list.
#[test]
fn histogram_memory_is_constant() {
    let mut histogram = LoudnessHistogram::new();
    for i in 0..200_000 {
        let lufs = -40.0 + (i % 300) as f64 * 0.1;
        histogram.add(lufs, power_of(lufs));
    }
    assert_eq!(histogram.total_count(), 200_000);
    assert_eq!(histogram.bin_storage(), (histogram::BIN_COUNT, histogram::BIN_COUNT));
}

// MARK: Tech 3341 cases 1 and 2: steady tone

#[test]
fn steady_tone_reads_minus_23() {
    let r = measured(&sine(1_000.0, -23.0, 48_000.0, 20.0), 48_000.0);
    assert_close(r.integrated_lufs, -23.0, 0.1, "integrated");
    assert_close(r.momentary_lufs, -23.0, 0.1, "momentary");
    assert_close(r.short_term_lufs, -23.0, 0.1, "short-term");
    assert_close(r.momentary_max_lufs, -23.0, 0.1, "momentary max");
    assert_close(r.short_term_max_lufs, -23.0, 0.1, "short-term max");
    assert!((r.measured_seconds - 20.0).abs() <= 0.01);
}

#[test]
fn steady_tone_reads_minus_33() {
    let r = measured(&sine(1_000.0, -33.0, 48_000.0, 20.0), 48_000.0);
    assert_close(r.integrated_lufs, -33.0, 0.1, "integrated");
    assert_close(r.momentary_lufs, -33.0, 0.1, "momentary");
    assert_close(r.short_term_lufs, -33.0, 0.1, "short-term");
}

/// K-weighting is derived per rate, so the same tone must read the same at
/// every rate. A 48 kHz coefficient table would fail this.
#[test]
fn steady_tone_across_sample_rates() {
    for rate in [44_100.0, 48_000.0, 96_000.0] {
        let r = measured(&sine(1_000.0, -23.0, rate, 20.0), rate);
        assert_close(r.integrated_lufs, -23.0, 0.1, &format!("integrated at {rate} Hz"));
        assert_close(r.momentary_lufs, -23.0, 0.1, &format!("momentary at {rate} Hz"));
        assert_close(r.short_term_lufs, -23.0, 0.1, &format!("short-term at {rate} Hz"));
    }
}

// MARK: Tech 3341 case 3: relative gate

#[test]
fn relative_gate_rejects_quiet_sections() {
    let signal = stepped_sine(1_000.0, 48_000.0, &[(-36.0, 10.0), (-23.0, 60.0), (-36.0, 10.0)]);
    assert_close(
        measured(&signal, 48_000.0).integrated_lufs,
        -23.0,
        0.1,
        "gated integrated",
    );
}

/// Everything below the absolute gate must vanish from the integrated value.
#[test]
fn absolute_gate_rejects_very_quiet_sections() {
    let signal = stepped_sine(1_000.0, 48_000.0, &[(-80.0, 10.0), (-23.0, 10.0)]);
    assert_close(
        measured(&signal, 48_000.0).integrated_lufs,
        -23.0,
        0.1,
        "integrated with sub-gate lead-in",
    );
}

// MARK: Tech 3342: loudness range

#[test]
fn loudness_range_of_ten_lu() {
    let signal = stepped_sine(1_000.0, 48_000.0, &[(-20.0, 20.0), (-30.0, 20.0)]);
    assert_close(
        measured(&signal, 48_000.0).loudness_range_lu,
        10.0,
        1.0,
        "loudness range",
    );
}

#[test]
fn loudness_range_of_steady_tone_is_near_zero() {
    let r = measured(&sine(1_000.0, -23.0, 48_000.0, 8.0), 48_000.0);
    assert_close(r.loudness_range_lu, 0.0, 0.2, "loudness range of a steady tone");
}

// MARK: True peak

/// A sine at fs/4 with 45 degrees of phase has sample peaks at −3 dBFS but a
/// true peak of exactly 0 dBTP. This is the classic inter-sample case.
#[test]
fn true_peak_of_quarter_rate_sine() {
    let rate = 48_000.0;
    let signal = test_signals::sine(rate / 4.0, 1.0, rate, 2.0, PI / 4.0);
    let sample_peak = signal.iter().fold(0.0f32, |peak, v| peak.max(v.abs()));
    assert_close(
        (20.0 * (sample_peak as f64).log10()) as f32,
        -3.0103,
        0.01,
        "sample peak of the test signal",
    );
    let r = measured(&signal, rate);
    assert_close(r.true_peak_max_dbtp, 0.0, 0.3, "true peak max");
    assert_close(r.true_peak_left_dbtp, 0.0, 0.3, "true peak left");
    assert_close(r.true_peak_right_dbtp, 0.0, 0.3, "true peak right");
}

#[test]
fn true_peak_of_minus_six_sine() {
    let r = measured(&sine(1_000.0, -6.0, 48_000.0, 2.0), 48_000.0);
    assert_close(r.true_peak_max_dbtp, -6.0, 0.2, "true peak max");
    assert_close(r.true_peak_left_dbtp, -6.0, 0.2, "true peak left");
}

/// Tech 3341 also expects the display to fall back after the hold time.
#[test]
fn true_peak_display_holds_then_falls() {
    let rate = 48_000.0;
    let mut meter = LoudnessMeter::new();
    feed_mono(&mut meter, &sine(1_000.0, -6.0, rate, 1.0), rate);
    assert_close(meter.read().true_peak_left_dbtp, -6.0, 0.2, "true peak before silence");

    // 1.0 s of silence: still inside the 1.5 s hold.
    let quiet = vec![0.0; rate as usize];
    feed_mono(&mut meter, &quiet, rate);
    assert_close(meter.read().true_peak_left_dbtp, -6.0, 0.2, "true peak during hold");

    // 1.0 s more: 0.5 s past the hold, so about 10 dB of fall.
    feed_mono(&mut meter, &quiet, rate);
    let fallen = meter.read();
    assert_close(fallen.true_peak_left_dbtp, -16.0, 1.0, "true peak after 0.5 s of fall");
    // The max since reset never moves.
    assert_close(
        fallen.true_peak_max_dbtp,
        -6.0,
        0.2,
        "true peak max is unaffected by the fall",
    );
}

// MARK: Clipping

#[test]
fn clip_count_counts_runs_of_three_or_more() {
    let mut left = vec![0.0f32; 2000];
    // A run of 3, a run of 5, a run of 2 (too short), a negative run of 3.
    left[100..103].fill(1.0);
    left[300..305].fill(1.0);
    left[500..502].fill(1.0);
    left[700..703].fill(-1.0);
    let mut meter = LoudnessMeter::new();
    feed_blocks(&mut meter, &left, &vec![0.0; 2000], 48_000.0, 128);
    assert_eq!(meter.read().clip_count, 3);
}

#[test]
fn clip_count_on_hard_clipped_sine() {
    let rate = 48_000.0;
    let cycles = 100;
    let seconds = cycles as f64 / 1000.0;
    let clipped: Vec<f32> = test_signals::sine(1_000.0, 2.0, rate, seconds, 0.0)
        .iter()
        .map(|x| x.clamp(-1.0, 1.0))
        .collect();
    let count = measured(&clipped, rate).clip_count;
    // Two flat tops per cycle per channel, stereo: about 4 per cycle.
    assert!(
        (4 * cycles - 4..=4 * cycles + 4).contains(&count),
        "{count} clip events"
    );
    // The unclipped source of the same tone must report nothing.
    assert_eq!(measured(&sine(1_000.0, -1.0, rate, seconds), rate).clip_count, 0);
}

// MARK: Silence

#[test]
fn silence_gives_finite_floors() {
    let r = measured(&vec![0.0; 48_000 * 4], 48_000.0);
    let levels = [
        r.momentary_lufs,
        r.short_term_lufs,
        r.integrated_lufs,
        r.momentary_max_lufs,
        r.short_term_max_lufs,
        r.loudness_range_lu,
        r.true_peak_left_dbtp,
        r.true_peak_right_dbtp,
        r.true_peak_max_dbtp,
        r.rms_left_db,
        r.rms_right_db,
        r.plr_db,
        r.psr_db,
    ];
    assert!(levels.iter().all(|level| level.is_finite()), "{levels:?}");
    assert_eq!(
        (r.momentary_lufs, r.short_term_lufs, r.integrated_lufs),
        (SILENCE, SILENCE, SILENCE)
    );
    assert_eq!((r.true_peak_max_dbtp, r.rms_left_db), (-120.0, -120.0));
    assert_eq!(
        (r.loudness_range_lu, r.plr_db, r.psr_db, r.clip_count),
        (0.0, 0.0, 0.0, 0)
    );
    assert!((r.measured_seconds - 4.0).abs() <= 0.01);
}

// MARK: RMS, PLR, PSR

#[test]
fn rms_over_three_hundred_milliseconds() {
    let mut meter = LoudnessMeter::new();
    // −12 dBFS peak sine: RMS is 3.01 dB below the peak.
    feed(
        &mut meter,
        &sine(1_000.0, -12.0, 48_000.0, 2.0),
        &sine(1_000.0, -18.0, 48_000.0, 2.0),
        48_000.0,
    );
    let r = meter.read();
    assert_close(r.rms_left_db, -15.01, 0.1, "rms left");
    assert_close(r.rms_right_db, -21.01, 0.1, "rms right");
}

#[test]
fn plr_and_psr() {
    let r = measured(&sine(1_000.0, -23.0, 48_000.0, 8.0), 48_000.0);
    // True peak of a −23 dBFS sine is −23 dBTP; integrated reads −23 LUFS.
    assert_close(r.true_peak_max_dbtp, -23.0, 0.2, "true peak max");
    assert_close(
        r.plr_db,
        (r.true_peak_max_dbtp - r.integrated_lufs) as f64,
        0.001,
        "plr identity",
    );
    assert_close(r.plr_db, 0.0, 0.3, "plr of a steady sine");
    assert_close(r.psr_db, 0.0, 0.3, "psr of a steady sine");
}

// MARK: Reset and rate changes

#[test]
fn reset_clears_integrated_and_max_values() {
    let mut meter = LoudnessMeter::new();
    feed_mono(&mut meter, &sine(1_000.0, -10.0, 48_000.0, 4.0), 48_000.0);
    let before = meter.read();
    assert!(before.integrated_lufs > -20.0 && before.true_peak_max_dbtp > -20.0 && before.measured_seconds > 3.0);
    meter.reset();
    assert_eq!(
        meter.read(),
        LoudnessReading::default(),
        "a reset meter reads like a new one"
    );
}

/// A device switch changes the rate mid-measurement. Filters and windows
/// reconfigure, and the tone still measures the same.
#[test]
fn sample_rate_change_reconfigures() {
    let mut meter = LoudnessMeter::new();
    feed_mono(&mut meter, &sine(1_000.0, -23.0, 48_000.0, 6.0), 48_000.0);
    feed_mono(&mut meter, &sine(1_000.0, -23.0, 96_000.0, 6.0), 96_000.0);
    let r = meter.read();
    assert_close(r.momentary_lufs, -23.0, 0.1, "momentary after the rate change");
    assert_close(r.short_term_lufs, -23.0, 0.1, "short-term after the rate change");
    assert_close(r.integrated_lufs, -23.0, 0.1, "integrated across the rate change");
    assert!((r.measured_seconds - 12.0).abs() <= 0.01);
}

/// The meter must not care how the host chops the stream into blocks.
#[test]
fn result_is_independent_of_block_size() {
    let tone = sine(1_000.0, -23.0, 48_000.0, 4.0);
    let values = [37, 512, 800, 4801].map(|block| {
        let mut meter = LoudnessMeter::new();
        feed_blocks(&mut meter, &tone, &tone, 48_000.0, block);
        meter.read().integrated_lufs
    });
    for value in values {
        assert_close(value, values[0] as f64, 0.01, "integrated by block size");
    }
}

/// Contract: the integrated value is valid only after the first gated block.
#[test]
fn integrated_is_valid_only_after_the_first_gated_block() {
    let mut meter = LoudnessMeter::new();
    assert!(!meter.read().is_integrated_valid);
    let tone = sine(1_000.0, -20.0, 48_000.0, 1.0);
    // 300 ms: no whole 400 ms block yet.
    feed_mono(&mut meter, &tone[..14_400], 48_000.0);
    assert!(!meter.read().is_integrated_valid);
    assert_eq!(meter.read().integrated_lufs, SILENCE);
    feed_mono(&mut meter, &tone[14_400..], 48_000.0);
    assert!(meter.read().is_integrated_valid);
    assert!(meter.read().integrated_lufs > -30.0);
    meter.reset();
    assert!(!meter.read().is_integrated_valid);

    // Audio under the absolute gate never makes the integrated value valid.
    feed_mono(&mut meter, &sine(1_000.0, -90.0, 48_000.0, 2.0), 48_000.0);
    assert!(!meter.read().is_integrated_valid);
}

/// Bad input changes nothing.
#[test]
fn bad_input_is_ignored() {
    let mut meter = LoudnessMeter::new();
    meter.process(&[], &[], 48_000.0);
    for rate in [0.0, -48_000.0, f64::NAN, f64::INFINITY] {
        meter.process(&[0.5; 100], &[0.5; 100], rate);
    }
    assert_eq!(meter.read(), LoudnessReading::default());
}

// MARK: True peak balance
//
// The held true peak of two channels with the same peak must be the same
// number. A hold timer that restarts only when a peak *exceeds* the display
// saw-tooths on a repeating kick, on a different phase per channel: measured on
// the demo signal, 11 dB apart. `PeakHoldBallistics` holds the largest level of
// the last 1.5 s, which has no order-of-arrival state.

const RATE: f64 = 48_000.0;
const BLOCK: usize = 800;

/// Feeds `seconds` of the demo signal in blocks and reads every 0.5 s. `body`
/// gets the time, the reading, and the sample peak of each channel in dB over
/// the hold window.
fn sweep_demo(seconds: f64, mono: bool, mut body: impl FnMut(f64, &LoudnessReading, f64, f64)) {
    let mut meter = LoudnessMeter::new();
    let read_every = (0.5 * RATE).round() as usize / BLOCK * BLOCK;
    // The hold window `PeakHoldBallistics` covers, worst case.
    let hold_window = TRUE_PEAK_HOLD_SECONDS * peak_hold::SLOTS as f64 / (peak_hold::SLOTS - 1) as f64;
    let blocks_in_window = ((hold_window * RATE / BLOCK as f64).ceil() as usize).max(1);
    let (mut history_l, mut history_r) = (Vec::new(), Vec::new());
    let (mut position, mut since_read) = (0, 0);
    let peak_of = |x: &[f32]| x.iter().fold(0.0f32, |peak, v| peak.max(v.abs()));
    let window_db = |history: &[f32]| 20.0 * (peak_of(history).max(1e-9) as f64).log10();
    while position < (seconds * RATE) as usize {
        let (left, right) = test_signals::demo_block(position as i64, BLOCK, RATE);
        let right = if mono { left.clone() } else { right };
        history_l.push(peak_of(&left));
        history_r.push(peak_of(&right));
        if history_l.len() > blocks_in_window {
            history_l.remove(0);
            history_r.remove(0);
        }
        meter.process(&left, &right, RATE);
        position += BLOCK;
        since_read += BLOCK;
        if since_read >= read_every {
            since_read = 0;
            body(
                position as f64 / RATE,
                &meter.read(),
                window_db(&history_l),
                window_db(&history_r),
            );
        }
    }
}

/// A mono signal must read the same true peak on both channels, held and max.
#[test]
fn mono_signal_reads_the_same_true_peak_on_both_channels() {
    let mut reads = 0;
    sweep_demo(40.0, true, |time, r, _, _| {
        reads += 1;
        assert_eq!(
            r.true_peak_left_dbtp, r.true_peak_right_dbtp,
            "held true peak L/R at t={time:.2} s"
        );
        // The max since reset is one number for the pair, so it can never sit
        // under the held value of a channel.
        assert!(
            r.true_peak_max_dbtp >= r.true_peak_left_dbtp.max(r.true_peak_right_dbtp),
            "max true peak at t={time:.2} s"
        );
    });
    assert_eq!(reads, 80);
}

/// A mono sine, the simplest case: both channels agree after every block.
#[test]
fn mono_sine_reads_the_same_true_peak_on_both_channels() {
    let mut meter = LoudnessMeter::new();
    for block in sine(997.0, -6.0, RATE, 4.0).chunks(BLOCK) {
        meter.process(block, block, RATE);
        let r = meter.read();
        assert_eq!(r.true_peak_left_dbtp, r.true_peak_right_dbtp);
    }
}

/// On the demo signal the held true peaks can differ only by as much as the
/// sample peaks of the channels over the same window differ, plus 0.5 dB of
/// interpolator headroom.
#[test]
fn demo_signal_channels_track_their_own_sample_peaks() {
    let mut lowest = 0.0f32;
    sweep_demo(40.0, false, |time, r, sample_l, sample_r| {
        let difference = (r.true_peak_left_dbtp - r.true_peak_right_dbtp).abs() as f64;
        let allowed = (sample_l - sample_r).abs() + 0.5;
        assert!(
            difference < allowed,
            "t={time:.2} s: TP L {} R {} but sample peaks L {sample_l:.2} R {sample_r:.2}",
            r.true_peak_left_dbtp,
            r.true_peak_right_dbtp
        );
        lowest = lowest.min(r.true_peak_left_dbtp.min(r.true_peak_right_dbtp));
    });
    // The mono kick dominates: neither channel dips into the chord-only level.
    assert!(
        lowest > -9.0,
        "lowest held true peak over 40 s of the demo signal: {lowest}"
    );
}

/// The sliding-maximum hold must not swallow the fall: a peak that stops really
/// does decay, and a repeating peak really does hold.
#[test]
fn repeating_peak_holds_while_a_stopped_peak_falls() {
    let mut meter = LoudnessMeter::new();
    // A −6 dBFS click every 0.25 s for 6 s, then silence.
    let mut signal = vec![0.0f32; (6.0 * RATE) as usize];
    let click = amplitude(-6.0);
    for i in (0..signal.len()).step_by((0.25 * RATE) as usize) {
        signal[i] = click;
        signal[i + 1] = -click;
    }
    feed_mono(&mut meter, &signal, RATE);
    // Every click is identical, so the display must still stand on the peak.
    let repeating = meter.read();
    assert_eq!(
        repeating.true_peak_left_dbtp, repeating.true_peak_max_dbtp,
        "a repeating click holds the display at the peak"
    );
    assert_eq!(
        repeating.true_peak_right_dbtp, repeating.true_peak_max_dbtp,
        "on both channels"
    );
    assert!(
        repeating.true_peak_left_dbtp > -6.5,
        "the click itself is a -6 dBFS step pair"
    );

    feed_mono(&mut meter, &vec![0.0; (2.0 * RATE) as usize], RATE);
    let stopped = meter.read();
    assert!(
        stopped.true_peak_left_dbtp < -10.0,
        "true peak falls once the clicks stop"
    );
    assert_eq!(
        stopped.true_peak_left_dbtp, stopped.true_peak_right_dbtp,
        "fall is symmetric"
    );
    assert_eq!(
        stopped.true_peak_max_dbtp, repeating.true_peak_max_dbtp,
        "the max since reset never falls"
    );
}
