//! Tests of the stereo analyzer, ported from `StereoFilterBankTests.swift`,
//! `StereoAnalyzerTests.swift`, `StereoAnalyzerSanitizeTests.swift`,
//! `StereoDemoBalanceTests.swift` and the stereo half of `ContractFieldTests.swift`.
//! The time budget is in `tests/performance.rs`.

use super::filter_bank::{band_sections, BandSplitFilterBank, BiquadSection};
use super::*;
use crate::test_signals;
use std::f64::consts::PI;

// Band indices of `BandEnergy::EDGES_HZ` = [20, 60, 250, 500, 2k, 4k, 6k, 12k, 24k].
const SUB_BASS: usize = 0;
const BASS: usize = 1;
const MID: usize = 3;
const UPPER_MID: usize = 4;
const PRESENCE: usize = 5;
const BRILLIANCE: usize = 6;
const AIR: usize = 7;

const EDGES: [f32; 9] = BandEnergy::EDGES_HZ;

fn sine(hz: f64, amplitude: f32, rate: f64, seconds: f64) -> Vec<f32> {
    test_signals::sine(hz, amplitude, rate, seconds, 0.0)
}

fn doubles(x: &[f32]) -> Vec<f64> {
    x.iter().map(|&v| v as f64).collect()
}

fn negated(x: &[f32]) -> Vec<f32> {
    x.iter().map(|v| -v).collect()
}

fn mix(a: &[f32], b: &[f32]) -> Vec<f32> {
    a.iter().zip(b).map(|(x, y)| x + y).collect()
}

/// Feeds a whole signal in blocks, the way the analysis engine does.
fn feed_blocks(analyzer: &mut StereoAnalyzer, left: &[f32], right: &[f32], rate: f64, block: usize) {
    assert_eq!(left.len(), right.len(), "test signal channels must match");
    for (l, r) in left.chunks(block).zip(right.chunks(block)) {
        analyzer.process(l, r, rate);
    }
}

fn feed(analyzer: &mut StereoAnalyzer, left: &[f32], right: &[f32], rate: f64) {
    feed_blocks(analyzer, left, right, rate, 800);
}

/// The reading after a signal went through a new analyzer.
fn measured(left: &[f32], right: &[f32], rate: f64) -> StereoReading {
    let mut analyzer = StereoAnalyzer::new();
    feed(&mut analyzer, left, right, rate);
    analyzer.read()
}

#[track_caller]
fn assert_no_nan(reading: &StereoReading) {
    assert!(!reading.correlation.is_nan() && !reading.balance.is_nan() && !reading.width.is_nan());
    assert!(reading
        .band_correlation
        .iter()
        .chain(&reading.band_balance)
        .all(|v| !v.is_nan()));
    assert!(
        reading.scope_points.iter().flatten().all(|v| !v.is_nan()),
        "scope point NaN"
    );
}

#[track_caller]
fn assert_near(actual: f32, expected: f32, accuracy: f32, what: &str) {
    assert!(
        (actual - expected).abs() <= accuracy,
        "{what}: {actual}, expected {expected} ± {accuracy}"
    );
}

// MARK: Filter bank

/// Direct form I, one sample at a time, written the long way. Nothing in common
/// with the bank's own arithmetic except the coefficients.
fn reference(sections: &[BiquadSection], input: &[f64]) -> Vec<f64> {
    let mut x = input.to_vec();
    for s in sections {
        let (mut x1, mut x2, mut y1, mut y2) = (0.0, 0.0, 0.0, 0.0);
        let mut out = vec![0.0; x.len()];
        for n in 0..x.len() {
            let value = s.b0 * x[n] + s.b1 * x1 + s.b2 * x2 - s.a1 * y1 - s.a2 * y2;
            x2 = x1;
            x1 = x[n];
            y2 = y1;
            y1 = value;
            out[n] = value;
        }
        x = out;
    }
    x
}

fn bank(rate: f64) -> BandSplitFilterBank {
    BandSplitFilterBank::new(&EDGES, rate).expect("a bank")
}

/// Every band, at every rate the analyzer supports, inside 1e-6 relative of the
/// scalar reference. The signal is broadband so no band is fed silence.
#[test]
fn bank_matches_a_scalar_reference_within_one_part_in_a_million() {
    for rate in [44_100.0, 48_000.0, 96_000.0] {
        let mut bank = bank(rate);
        assert_eq!(bank.band_count(), 8);
        let noise = doubles(&test_signals::pink_noise(0.5, 800, 0xB10C));
        let other = doubles(&test_signals::pink_noise(0.5, 800, 0xB10D));
        bank.run(&noise, &other);
        for band in 0..8 {
            let sections = band_sections(EDGES[band] as f64, EDGES[band + 1] as f64, rate);
            for (side, input, out) in [("left", &noise, bank.left(band)), ("right", &other, bank.right(band))] {
                let expected = reference(&sections, input);
                let scale = expected.iter().fold(0.0f64, |m, v| m.max(v.abs()));
                let worst = out.iter().zip(&expected).fold(0.0f64, |m, (a, b)| m.max((a - b).abs()));
                assert!(
                    worst < scale.max(1e-12) * 1e-6,
                    "{side} band {band} at {rate} Hz: worst {worst}, scale {scale}"
                );
            }
        }
    }
}

/// State carries across blocks: one block of 800 has to equal two of 400.
#[test]
fn bank_state_carries_across_blocks() {
    let (mut one, mut split) = (bank(48_000.0), bank(48_000.0));
    let noise = doubles(&test_signals::pink_noise(0.5, 800, 7));
    one.run(&noise, &noise);
    let whole: Vec<Vec<f64>> = (0..8).map(|band| one.left(band).to_vec()).collect();
    let mut halves: Vec<Vec<f64>> = vec![Vec::new(); 8];
    for half in noise.chunks(400) {
        split.run(half, half);
        for band in 0..8 {
            halves[band].extend_from_slice(split.right(band));
        }
    }
    for band in 0..8 {
        let worst = whole[band]
            .iter()
            .zip(&halves[band])
            .fold(0.0f64, |m, (a, b)| m.max((a - b).abs()));
        assert!(
            worst < 1e-12,
            "band {band}: one block and two blocks disagree by {worst}"
        );
    }
}

/// `reset_state` really clears the delay line.
#[test]
fn bank_reset_clears_the_delay_line() {
    let mut bank = bank(48_000.0);
    let noise = doubles(&test_signals::pink_noise(0.5, 256, 11));
    bank.run(&noise, &noise);
    let first = bank.left(3).to_vec();
    bank.reset_state();
    bank.run(&noise, &noise);
    assert_eq!(first, bank.left(3));
}

/// Left and right keep their own state: the same band fed different signals does not mix.
#[test]
fn bank_left_and_right_are_independent() {
    let mut bank = bank(48_000.0);
    let left = doubles(&sine(1_000.0, 0.5, 48_000.0, 512.0 / 48_000.0));
    bank.run(&left, &vec![0.0; left.len()]);
    for band in 0..8 {
        assert!(
            bank.right(band).iter().all(|&v| v == 0.0),
            "a silent right channel stays silent"
        );
    }
    assert!(bank.left(MID).iter().map(|v| v * v).sum::<f64>() > 0.0);
}

/// A 1 kHz tone belongs to the mid band and to no other: that is what the bank is for.
#[test]
fn bank_a_tone_picks_one_band() {
    let rate = 48_000.0;
    let mut bank = bank(rate);
    // Long enough that the band-pass start-up transient has rung out. Energy is
    // measured over the last quarter only.
    let n = 48_000;
    let tone = doubles(&sine(1_000.0, 0.5, rate, n as f64 / rate));
    bank.run(&tone, &tone);
    let energies: Vec<f64> = (0..8)
        .map(|band| bank.left(band)[n * 3 / 4..].iter().map(|v| v * v).sum())
        .collect();
    let loudest = (0..8).max_by(|&a, &b| energies[a].total_cmp(&energies[b])).unwrap();
    assert_eq!(loudest, MID, "1 kHz landed in band {loudest}");
    for band in (0..8).filter(|&band| band != loudest) {
        assert!(energies[band] < energies[loudest] * 1e-4, "band {band} leaks");
    }
}

#[test]
fn bank_refuses_bad_input_and_grows_for_a_long_block() {
    assert!(BandSplitFilterBank::new(&[20.0], 48_000.0).is_none());
    assert!(BandSplitFilterBank::new(&EDGES, 0.0).is_none());
    assert!(BandSplitFilterBank::new(&EDGES, f64::NAN).is_none());
    let mut bank = bank(48_000.0);
    let long = doubles(&test_signals::pink_noise(0.5, 5_000, 3));
    bank.run(&long, &long);
    assert_eq!(bank.left(0).len(), 5_000);
    assert_eq!(bank.left(MID), bank.right(MID));
}

// MARK: Broadband numbers

#[test]
fn mono_sine_is_fully_correlated_narrow_and_centred() {
    let s = sine(1_000.0, 0.5, 48_000.0, 2.0);
    let r = measured(&s, &s, 48_000.0);
    assert!(r.correlation > 0.99);
    assert!(r.width < 0.01);
    assert_near(r.balance, 0.0, 0.01, "balance");
    assert_no_nan(&r);
}

#[test]
fn inverted_channel_is_anti_correlated() {
    let s = sine(1_000.0, 0.5, 48_000.0, 2.0);
    let r = measured(&s, &negated(&s), 48_000.0);
    assert!(r.correlation < -0.99);
    // No mid energy at all: width pins at the clamp.
    assert_near(r.width, MAX_WIDTH, 1e-5, "width");
    assert_no_nan(&r);
}

#[test]
fn independent_noise_is_uncorrelated() {
    let r = measured(
        &test_signals::white_noise(0.5, 96_000, 1),
        &test_signals::white_noise(0.5, 96_000, 99),
        48_000.0,
    );
    assert!(r.correlation.abs() < 0.1);
    assert_no_nan(&r);
}

#[test]
fn left_only_is_hard_left() {
    let s = sine(700.0, 0.5, 48_000.0, 2.0);
    let r = measured(&s, &vec![0.0; s.len()], 48_000.0);
    assert!(r.balance < -0.95);
    // A hard-panned channel splits evenly into mid and side, so width reads 1.
    assert_near(r.width, 1.0, 0.02, "width");
    assert_no_nan(&r);
}

// MARK: Per-band numbers

#[test]
fn band_correlation_splits_mono_bass_from_inverted_presence() {
    let low = sine(100.0, 0.4, 48_000.0, 2.0);
    let high = sine(5_000.0, 0.4, 48_000.0, 2.0);
    let r = measured(&mix(&low, &high), &mix(&low, &negated(&high)), 48_000.0);
    assert!(r.band_correlation[BASS] > 0.99);
    assert!(r.band_correlation[PRESENCE] < -0.99);
    assert_no_nan(&r);
}

#[test]
fn band_balance_puts_right_only_tone_in_its_band_and_leaves_empty_bands_at_zero() {
    // 4 seconds: the band-split start-up transient has to leave the 300 ms
    // window before the far bands settle under the empty-band gate.
    let s = sine(1_000.0, 0.5, 48_000.0, 4.0);
    let r = measured(&vec![0.0; s.len()], &s, 48_000.0);
    assert!(r.band_balance[MID] > 0.9);
    // Bands far from 1 kHz hold no energy above the gate and stay neutral.
    for band in [SUB_BASS, BRILLIANCE, AIR] {
        assert_eq!(r.band_balance[band], 0.0);
        assert_eq!(r.band_correlation[band], 0.0);
    }
    assert_no_nan(&r);
}

/// Contract: `band_active` marks only the bands with energy.
#[test]
fn band_active_marks_only_the_bands_with_energy() {
    let mut analyzer = StereoAnalyzer::new();
    assert_eq!(
        analyzer.read().band_active,
        [false; 8],
        "nothing is active before audio"
    );
    let s = sine(1_000.0, 0.5, 48_000.0, 4.0);
    feed(&mut analyzer, &s, &s, 48_000.0);
    let r = analyzer.read();
    assert!(r.band_active[MID], "1 kHz is in the mid band");
    assert!(!r.band_active[SUB_BASS], "sub bass is a gated empty band");
    assert!(!r.band_active[AIR], "air is a gated empty band");
    for band in (0..8).filter(|&band| !r.band_active[band]) {
        assert_eq!((r.band_correlation[band], r.band_balance[band]), (0.0, 0.0));
    }
}

// MARK: Vectorscope

#[test]
fn scope_point_count_is_capped_by_the_request_and_by_fifty_milliseconds() {
    let mut analyzer = StereoAnalyzer::new();
    let s = sine(440.0, 0.3, 48_000.0, 0.5);
    feed(&mut analyzer, &s, &s, 48_000.0);
    // 50 ms at 48 kHz = 2400 frames, more than the 2048 default: the request caps it.
    assert_eq!(analyzer.read().scope_points.len(), 2048);
    // A request for more than the window holds gives the whole 50 ms and no more.
    analyzer.set_scope_point_count(8_192);
    assert_eq!(analyzer.read().scope_points.len(), 2_400);
    analyzer.set_scope_point_count(256);
    assert_eq!(analyzer.read().scope_points.len(), 256);
    analyzer.set_scope_point_count(1);
    assert_eq!(analyzer.read().scope_points.len(), 1);
    analyzer.set_scope_point_count(0);
    assert!(analyzer.read().scope_points.is_empty());
}

#[test]
fn scope_is_newest_last_and_full_scale_mono_reaches_the_diamond_tip() {
    let mut analyzer = StereoAnalyzer::new();
    let zeros = vec![0.0; 9_600]; // 200 ms, fills the 50 ms ring
    feed(&mut analyzer, &zeros, &zeros, 48_000.0);
    // One loud in-phase frame last.
    analyzer.process(&[1.0], &[1.0], 48_000.0);
    let points = analyzer.read().scope_points;
    assert_eq!(points.len(), 2_048);
    // L = R = 1 is the top tip of the full-scale diamond: y = 1 by the mapping, not by the clamp.
    assert_eq!(points[points.len() - 1], [0.0, 1.0]);
    assert_eq!(points[points.len() - 2][1], 0.0);
    assert_eq!(points[0][1], 0.0);
}

#[test]
fn scope_geometry_mono_left_only_and_inverted() {
    let s = sine(200.0, 0.5, 48_000.0, 0.3);
    let zeros = vec![0.0; s.len()];
    let largest = |points: &[[f32; 2]], axis: usize| points.iter().fold(0.0f32, |m, p| m.max(p[axis].abs()));

    let mono = measured(&s, &s, 48_000.0).scope_points;
    assert!(!mono.is_empty());
    assert!(mono.iter().all(|p| p[0].abs() <= 1e-6), "mono is a vertical line");
    // Mono at 0.5: y = (L + R)·0.5 = L, so the line reaches 0.5, half way to the tip.
    assert_near(largest(&mono, 1), 0.5, 0.01, "mono height");

    // x = −0.5·L, y = +0.5·L: the cloud lies on the up-left / down-right diagonal.
    let left_only = measured(&s, &zeros, 48_000.0).scope_points;
    assert!(left_only
        .iter()
        .all(|p| p[0] * p[1] <= 1e-9 && (p[0] + p[1]).abs() <= 1e-6));
    assert!(left_only.iter().any(|p| p[1] > 0.2 && p[0] < -0.2), "no up-left points");

    let inverted = measured(&s, &negated(&s), 48_000.0).scope_points;
    assert!(
        inverted.iter().all(|p| p[1].abs() <= 1e-6),
        "anti-phase is a horizontal line"
    );
    // Anti-phase at 0.5: x = (R − L)·0.5 = −L, so the line reaches 0.5 either side.
    assert_near(largest(&inverted, 0), 0.5, 0.01, "anti-phase width");
}

/// A hard-clipped signal follows the 45° edges of the full-scale diamond. At a
/// 0.7071 scale a clipped mono sample would map to y = ±1.414, and the clamp
/// would draw a flat top inside the diamond.
#[test]
fn hard_clipped_signal_follows_the_diamond_edges() {
    let n = 4_800; // 50 ms at 96 kHz: exactly the ring
    let left = vec![1.0f32; n]; // pinned at full scale
    let right: Vec<f32> = (0..n).map(|i| i as f32 / (n - 1) as f32 * 2.0 - 1.0).collect(); // sweeps −1 ... 1
    let mut analyzer = StereoAnalyzer::new();
    analyzer.set_scope_point_count(4_800);
    feed(&mut analyzer, &left, &right, 96_000.0);
    let points = analyzer.read().scope_points;
    assert_eq!(points.len(), n);
    // Every point sits on an edge of the full-scale diamond: |x| + |y| = max(|L|, |R|) = 1.
    for p in &points {
        assert!((p[0].abs() + p[1].abs() - 1.0).abs() <= 1e-6 && p[0].abs() <= 1.0 && p[1].abs() <= 1.0);
    }
    // The sweep is even, so the edge is drawn evenly: only the R = 1 end reaches the tip.
    let at_the_top = points.iter().filter(|p| p[1] >= 0.999).count();
    assert!(
        (at_the_top as f64 / n as f64) < 0.02,
        "flat top: {at_the_top} of {n} points"
    );
    let ys = points.iter().map(|p| p[1]);
    assert_near(ys.clone().fold(f32::MIN, f32::max), 1.0, 1e-6, "top");
    assert_near(ys.fold(f32::MAX, f32::min), 0.0, 1e-6, "bottom");
    assert_near(points.iter().map(|p| p[0]).fold(f32::MAX, f32::min), -1.0, 1e-6, "left");
}

/// The −1...1 box of the contract holds for every in-range sample without the clamp doing work.
#[test]
fn scope_points_stay_in_the_unit_diamond_for_full_scale_input() {
    let n = 2_400;
    let (left, right) = (
        test_signals::white_noise(1.0, n, 7),
        test_signals::white_noise(1.0, n, 8),
    );
    let mut analyzer = StereoAnalyzer::new();
    analyzer.set_scope_point_count(2_400);
    feed(&mut analyzer, &left, &right, 48_000.0);
    let points = analyzer.read().scope_points;
    assert_eq!(points.len(), n);
    for (i, p) in points.iter().enumerate() {
        let size = p[0].abs() + p[1].abs();
        assert!(size <= 1.0 + 1e-6, "point {i} left the diamond");
        assert!(
            (size - left[i].abs().max(right[i].abs())).abs() <= 1e-6,
            "|x| + |y| must be max(|L|, |R|)"
        );
    }
}

// MARK: Silence, reset, sample rate

#[test]
fn silence_gives_zeros_and_no_nan() {
    let zeros = vec![0.0; 48_000];
    let r = measured(&zeros, &zeros, 48_000.0);
    assert_eq!((r.correlation, r.balance, r.width), (0.0, 0.0, 0.0));
    assert_eq!(
        (r.band_correlation, r.band_balance, r.band_active),
        ([0.0; 8], [0.0; 8], [false; 8])
    );
    assert_eq!(r.scope_points.len(), 2_048);
    assert!(r.scope_points.iter().all(|p| *p == [0.0, 0.0]));
}

#[test]
fn reset_clears_everything() {
    let mut analyzer = StereoAnalyzer::new();
    let s = sine(1_000.0, 0.5, 48_000.0, 1.0);
    feed(&mut analyzer, &s, &vec![0.0; s.len()], 48_000.0);
    assert!(analyzer.read().balance < -0.9);
    analyzer.reset();
    assert_eq!(analyzer.read(), StereoReading::default());
}

#[test]
fn sample_rate_change_reconfigures_cleanly() {
    let mut analyzer = StereoAnalyzer::new();
    analyzer.set_scope_point_count(8_192);
    let mono = sine(1_000.0, 0.5, 48_000.0, 1.0);
    feed(&mut analyzer, &mono, &mono, 48_000.0);
    assert!(analyzer.read().correlation > 0.99);
    assert_eq!(analyzer.read().scope_points.len(), 2_400); // 50 ms at 48 kHz

    let high = sine(1_000.0, 0.5, 96_000.0, 2.0);
    feed(&mut analyzer, &high, &negated(&high), 96_000.0);
    let r = analyzer.read();
    assert!(r.correlation < -0.99);
    assert_eq!(r.scope_points.len(), 4_800); // 50 ms at 96 kHz
    assert_no_nan(&r);
}

#[test]
fn same_results_at_44100_and_96000() {
    let measure_at = |rate: f64| {
        let low = sine(200.0, 0.4, rate, 2.0);
        let high_l = sine(3_000.0, 0.3, rate, 2.0);
        let high_r = test_signals::sine(3_000.0, 0.3, rate, 2.0, PI / 3.0);
        measured(&mix(&low, &high_l), &mix(&low, &high_r), rate)
    };
    let (a, b) = (measure_at(44_100.0), measure_at(96_000.0));
    assert_near(a.correlation, b.correlation, 0.02, "correlation");
    assert_near(a.balance, b.balance, 0.02, "balance");
    assert_near(a.width, b.width, 0.02, "width");
    for band in [BASS, UPPER_MID] {
        assert_near(
            a.band_correlation[band],
            b.band_correlation[band],
            0.02,
            "band correlation",
        );
        assert_near(a.band_balance[band], b.band_balance[band], 0.02, "band balance");
    }
    assert_no_nan(&a);
    assert_no_nan(&b);
}

/// The engine hands over up to 32768 frames at once. One big block must give
/// the same answer as many small ones and must not overrun the 50 ms scope ring.
#[test]
fn one_large_block_matches_many_small_blocks() {
    let base = test_signals::pink_noise(0.5, 48_000, 3);
    let other = test_signals::pink_noise(0.5, 48_000, 5);
    let right: Vec<f32> = base.iter().zip(&other).map(|(a, b)| 0.7 * a + 0.3 * b).collect();
    let read = |block: usize| {
        let mut analyzer = StereoAnalyzer::new();
        feed_blocks(&mut analyzer, &base, &right, 48_000.0, block);
        analyzer.read()
    };
    let (a, b) = (read(512), read(32_768));
    assert_near(a.correlation, b.correlation, 0.02, "correlation");
    assert_near(a.balance, b.balance, 0.02, "balance");
    assert_near(a.width, b.width, 0.02, "width");
    for band in 0..8 {
        assert_near(
            a.band_correlation[band],
            b.band_correlation[band],
            0.02,
            &format!("band {band} correlation"),
        );
        assert_near(
            a.band_balance[band],
            b.band_balance[band],
            0.02,
            &format!("band {band} balance"),
        );
    }
    assert_eq!(b.scope_points.len(), 2_048);
    assert_no_nan(&b);
}

// MARK: Bad samples

#[test]
fn non_finite_sample_does_not_poison_the_accumulators() {
    let mut left = sine(300.0, 0.5, 48_000.0, 0.5);
    let mut right = left.clone();
    let mut reference = StereoAnalyzer::new();
    reference.process(&left, &right, 48_000.0);
    let expected = reference.read();

    left[10] = f32::NAN;
    right[5000] = f32::INFINITY;
    left[12_000] = f32::NEG_INFINITY;
    right[12_000] = f32::NAN;
    let mut analyzer = StereoAnalyzer::new();
    analyzer.process(&left, &right, 48_000.0);
    assert!(analyzer.sanitized_chunk_count() >= 1);
    let reading = analyzer.read();
    assert_near(reading.correlation, expected.correlation, 0.02, "correlation");
    assert_near(reading.balance, expected.balance, 0.02, "balance");
    assert!(reading.scope_points.iter().flatten().all(|v| v.is_finite()));
    assert!(reading.band_correlation.iter().all(|v| v.is_finite()));

    // No reset: a later anti-phase signal reads −1, so nothing is stuck.
    let tone = sine(300.0, 0.5, 48_000.0, 2.0);
    analyzer.process(&tone, &negated(&tone), 48_000.0);
    assert_near(
        analyzer.read().correlation,
        -1.0,
        0.02,
        "correlation after the bad block",
    );
    assert!(analyzer.read().width > 1.0);
}

#[test]
fn clean_input_is_never_sanitized() {
    let mut analyzer = StereoAnalyzer::new();
    let tone = sine(1_000.0, 1.0, 96_000.0, 1.0);
    analyzer.process(&tone, &tone, 96_000.0);
    assert_eq!(analyzer.sanitized_chunk_count(), 0);
    analyzer.process(&[], &[], 96_000.0);
    analyzer.process(&tone, &tone, f64::NAN);
    assert!(analyzer.read().correlation > 0.99, "bad calls change nothing");
}

// MARK: The demo signal

/// The only content of the demo signal below 60 Hz is the kick, which is mono,
/// plus the pink noise bed, which has no systematic side. So the sub-bass
/// balance the analyzer reports is centred. A view that wanders there has a
/// render-side problem, not a measurement.
#[test]
fn sub_bass_band_balance_on_the_demo_signal_is_centred() {
    let (rate, block) = (48_000.0, 800);
    let mut analyzer = StereoAnalyzer::new();
    let (mut position, mut reads) = (0usize, 0);
    let (mut worst, mut sum) = (0.0f32, 0.0f64);
    while position < (40.0 * rate) as usize {
        let (left, right) = test_signals::demo_block(position as i64, block, rate);
        analyzer.process(&left, &right, rate);
        position += block;
        // Skip the first second: the band filters and the 300 ms window are still settling.
        if (position as f64) < rate {
            continue;
        }
        let reading = analyzer.read();
        assert!(
            reading.band_active[SUB_BASS],
            "sub-bass band went inactive at sample {position}"
        );
        let balance = reading.band_balance[SUB_BASS];
        if balance.abs() > worst.abs() {
            worst = balance;
        }
        sum += balance as f64;
        reads += 1;
    }
    assert!(reads > 2_000);
    assert!(worst.abs() < 0.05, "worst sub-bass balance {worst}");
    assert!(
        (sum / reads as f64).abs() < 0.01,
        "mean sub-bass balance {}",
        sum / reads as f64
    );
}
