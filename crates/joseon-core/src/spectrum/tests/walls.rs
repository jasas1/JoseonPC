//! Port of `SpectrumWallsTests.swift`: the flanks of tonal peaks. Nothing is
//! excluded between 100 Hz and 5 kHz.
//!
//! The bar, for every pair of adjacent display bins:
//!
//! - outside the main lobe of a tone: `|delta| <= 3 dB`;
//! - inside the main lobe: the curve rises monotonically to the peak and falls
//!   monotonically from it, and it follows `noise + Hann main lobe` of the
//!   analyzer's resolution bandwidth at that frequency;
//! - no prominent local maximum that is not a partial of the signal;
//! - away from the lobes the curve with tones equals the curve of the same
//!   noise without tones, so there is no shelf between neighbouring tones.

use super::low_end::grid;
use super::*;
use std::f64::consts::PI;

/// Feeds `seconds` of `signal(start_sample, count)` in 60 blocks per second,
/// with a `read` after each block. Returns the readings at the times in
/// `read_at`, then the last reading.
fn run(seconds: f64, read_at: &[f64], signal: impl Fn(usize, usize) -> (Vec<f32>, Vec<f32>)) -> Vec<SpectrumReading> {
    let rate = 48_000.0;
    let mut analyzer = SpectrumAnalyzer::default();
    let block = (rate / 60.0) as usize;
    let mut out = Vec::new();
    let mut pending = read_at.iter().copied().peekable();
    let mut reading = analyzer.read().spectrum;
    for k in 0..(seconds * 60.0) as usize {
        let (left, right) = signal(k * block, block);
        analyzer.process(&left, &right, rate);
        reading = analyzer.read().spectrum;
        let t = ((k + 1) * block) as f64 / rate;
        while pending.peek().is_some_and(|&next| t >= next) {
            out.push(reading.clone());
            pending.next();
        }
    }
    out.push(reading);
    out
}

fn smoothstep(f: f32, centre: f32, half_width: f32) -> f32 {
    let t = ((f / centre).log2() + half_width) / (2.0 * half_width);
    let c = t.max(0.0).min(1.0);
    c * c * (3.0 - 2.0 * c)
}

/// Bin width of the lobe a tone is drawn with at 48 kHz: the FFT's own (1.46,
/// 5.86, 23.4 Hz), moving from one to the next across the half-octave
/// crossfades at 200 Hz and 2 kHz. An independent copy on purpose: the test
/// must not take the width from the code under test.
fn tone_bin_hz(f: f32) -> f32 {
    let [low, mid, high] = [32_768.0f32, 8_192.0, 2_048.0].map(|size| (48_000.0 / size).log2());
    let s_low = smoothstep(f, 200.0, 0.5);
    let s_high = smoothstep(f, 2_000.0, 0.5);
    ((1.0 - s_low) * low + s_low * (1.0 - s_high) * mid + s_high * high).exp2()
}

/// Hann main lobe in power, `u` in bins off centre. Zero outside the main lobe.
fn hann_lobe_power(u: f32) -> f32 {
    let a = u.abs();
    if a >= 2.0 {
        return 0.0;
    }
    if a < 1e-4 {
        return 1.0;
    }
    if (a - 1.0).abs() < 1e-4 {
        return 0.25;
    }
    let h = (std::f32::consts::PI * a).sin() / (std::f32::consts::PI * a) / (1.0 - a * a);
    h * h
}

#[derive(Clone, Copy)]
struct Tone {
    hz: f32,
    level_db: f32,
}

/// `noise + sum of lobes`, in dB, with the lobe taken at the point of the
/// display bin's span nearest to the tone (the display keeps the highest point
/// inside a bin).
fn expected(frequencies: &[f32], noise_db: &[f32], tones: &[Tone]) -> Vec<f32> {
    let half_step = (frequencies[1] / frequencies[0]).sqrt();
    frequencies
        .iter()
        .zip(noise_db)
        .map(|(&f, &noise)| {
            let mut power = 10f32.powf(noise / 10.0);
            for tone in tones {
                let (lo, hi) = (f / half_step, f * half_step);
                let distance = if tone.hz < lo {
                    lo - tone.hz
                } else if tone.hz > hi {
                    tone.hz - hi
                } else {
                    0.0
                };
                power += 10f32.powf(tone.level_db / 10.0) * hann_lobe_power(distance / tone_bin_hz(tone.hz));
            }
            10.0 * power.max(1e-30).log10()
        })
        .collect()
}

/// The tone whose main lobe holds `f`, if any.
fn in_lobe(f: f32, tones: &[f32], bin_ratio: f32) -> Option<f32> {
    tones.iter().copied().find(|&t| {
        let reach = 2.0 * tone_bin_hz(t);
        f >= (t - reach) / bin_ratio && f <= (t + reach) * bin_ratio
    })
}

/// The flank rules, from 100 Hz to 5 kHz. Returns failures a person can read.
fn flank_failures(reading: &SpectrumReading, curve: &[f32], tones: &[f32]) -> Vec<String> {
    let mut failures = Vec::new();
    let f = &reading.frequencies;
    let ratio = f[1] / f[0];
    for i in 1..curve.len() {
        if !(f[i - 1] >= 100.0 && f[i] <= 5_000.0) {
            continue;
        }
        let d = curve[i] - curve[i - 1];
        let (a, b) = (in_lobe(f[i - 1], tones, ratio), in_lobe(f[i], tones, ratio));
        match a.or(b) {
            Some(t) if a.is_none() || b.is_none() || a == b => {
                // On one main lobe, its foot included: monotone towards the peak
                // on the left, away from it on the right.
                let mid = (f[i - 1] * f[i]).sqrt();
                // The top of the lobe is one or two display bins wide: no direction there.
                if (mid / t).log2().abs() < ratio.log2() {
                    continue;
                }
                if mid < t && d < -0.5 {
                    failures.push(format!("left flank of {t:.1} Hz falls {d:.2} dB at {:.1} Hz", f[i]));
                }
                if mid > t && d > 0.5 {
                    failures.push(format!("right flank of {t:.1} Hz rises {d:.2} dB at {:.1} Hz", f[i]));
                }
            }
            _ if d.abs() > 3.0 => failures.push(format!("{d:.2} dB step outside any lobe at {:.1} Hz", f[i])),
            _ => {}
        }
    }
    failures
}

/// Local maxima from 100 Hz to 5 kHz that stand 5 dB over both sides within 8 bins.
fn prominent_maxima(reading: &SpectrumReading, curve: &[f32]) -> Vec<f32> {
    let (prominence, reach) = (5.0, 8);
    let f = &reading.frequencies;
    let lowest = |values: &[f32]| values.iter().copied().fold(f32::MAX, f32::min);
    (reach..curve.len() - reach)
        .filter(|&i| f[i] >= 100.0 && f[i] <= 5_000.0)
        .filter(|&i| curve[i] >= curve[i - 1] && curve[i] > curve[i + 1])
        .filter(|&i| curve[i] - lowest(&curve[i - reach..i]).max(lowest(&curve[i + 1..=i + reach])) >= prominence)
        .map(|i| f[i])
        .collect()
}

/// Pink noise with and without `tones`, after `seconds`.
fn noise_plus_tones(tones: &[Tone], seconds: f64) -> (SpectrumReading, SpectrumReading) {
    let rate = 48_000.0;
    let count = (rate * seconds) as usize;
    let noise_l = test_signals::pink_noise(0.04, count, 0x5EED);
    let noise_r = test_signals::pink_noise(0.04, count, 0xC0FFEE);
    let (mut tone_l, mut tone_r) = (noise_l.clone(), noise_r.clone());
    for tone in tones {
        let s = test_signals::sine(tone.hz as f64, 10f32.powf(tone.level_db / 20.0), rate, seconds, 0.0);
        for i in 0..count {
            tone_l[i] += s[i];
            tone_r[i] += s[i];
        }
    }
    let last = |l: &[f32], r: &[f32]| {
        run(seconds, &[], |start, n| {
            (l[start..start + n].to_vec(), r[start..start + n].to_vec())
        })
        .pop()
        .unwrap()
    };
    (last(&tone_l, &tone_r), last(&noise_l, &noise_r))
}

fn check_against_model(name: &str, tones: &[Tone]) {
    let (with, without) = noise_plus_tones(tones, 4.0);
    let model = expected(&with.frequencies, &without.mid, tones);
    let (mut worst, mut worst_hz) = (0.0f32, 0.0f32);
    for (i, &f) in with.frequencies.iter().enumerate() {
        if f >= 100.0 && f <= 5_000.0 {
            let error = (with.mid[i] - model[i]).abs();
            if error > worst {
                (worst, worst_hz) = (error, f);
            }
        }
    }
    assert!(
        worst <= 3.0,
        "{name}: mid is {worst} dB away from noise + Hann lobe at {worst_hz} Hz"
    );
    let hz: Vec<f32> = tones.iter().map(|tone| tone.hz).collect();
    let failures = flank_failures(&with, &with.mid, &hz);
    assert!(failures.is_empty(), "{name}: {}", failures.join("; "));
    let maxima = prominent_maxima(&with, &with.mid);
    for &m in &maxima {
        assert!(
            hz.iter().any(|t| (t / m - 1.0).abs() < 0.015),
            "{name}: false peak at {m} Hz"
        );
    }
    for &t in &hz {
        assert!(
            maxima.iter().any(|m| (t / m - 1.0).abs() < 0.015),
            "{name}: no peak at {t} Hz"
        );
    }
}

/// Pink noise with three tones 25 dB over it, a fifth and an octave apart like
/// the upper partials of the demo chord.
#[test]
fn three_tones_over_pink_noise() {
    check_against_model(
        "three tones",
        &[196.0, 294.0, 392.0].map(|hz| Tone { hz, level_db: -36.0 }),
    );
}

/// One tone, so nothing but its own lobe can shape the flank.
#[test]
fn one_tone_over_pink_noise() {
    check_against_model(
        "one tone",
        &[Tone {
            hz: 392.0,
            level_db: -36.0,
        }],
    );
}

/// A loud tone has sidelobes well over the noise. They must not be taken for tones.
#[test]
fn loud_tone_has_no_false_neighbours() {
    let (with, _) = noise_plus_tones(
        &[Tone {
            hz: 392.0,
            level_db: -6.0,
        }],
        3.0,
    );
    let maxima = prominent_maxima(&with, &with.mid);
    assert_eq!(maxima.len(), 1, "maxima at {maxima:?}");
    let failures: Vec<String> = flank_failures(&with, &with.mid, &[392.0])
        .into_iter()
        .filter(|failure| !failure.contains("outside"))
        .collect();
    assert!(failures.is_empty(), "{}", failures.join("; "));
}

const CHORD_PARTIALS: [f32; 6] = [1.0, 1.5, 2.0, 2.52, 3.0, 4.0];

fn demo(start: usize, count: usize) -> (Vec<f32>, Vec<f32>) {
    test_signals::demo_block(start as i64, count, 48_000.0)
}

/// The demo signal, read at 8 s (chord root 98 Hz) and in the middle of the
/// chord before it (root 146.83 Hz).
#[test]
fn demo_signal_flanks() {
    let readings = run(8.0, &[5.5], demo);
    for (reading, root, name) in [
        (&readings[0], 146.83f32, "demo 5.5 s"),
        (&readings[1], 98.0, "demo 8 s"),
    ] {
        let mut partials: Vec<f32> = CHORD_PARTIALS.iter().map(|m| m * root).collect();
        partials.push(3_100.0);
        for (curve_name, curve) in [
            ("mid", &reading.mid),
            ("left", &reading.left),
            ("right", &reading.right),
        ] {
            let failures = flank_failures(reading, curve, &partials);
            assert!(failures.is_empty(), "{name} {curve_name}: {}", failures.join("; "));
            for m in prominent_maxima(reading, curve) {
                assert!(
                    partials.iter().any(|p| (p / m - 1.0).abs() < 0.015),
                    "{name} {curve_name}: false peak at {m} Hz"
                );
            }
        }
        // No shelf: the floor between the last two partials is the floor after the last one.
        let median = |lo: f32, hi: f32| {
            let mut values: Vec<f32> = reading
                .frequencies
                .iter()
                .zip(&reading.mid)
                .filter(|(f, _)| **f >= lo && **f <= hi)
                .map(|(_, &v)| v)
                .collect();
            values.sort_by(f32::total_cmp);
            values[values.len() / 2]
        };
        let (top, below) = (4.0 * root, 3.0 * root);
        let (between, after) = (median(below * 1.06, top / 1.06), median(top * 1.06, top * 1.25));
        assert_near(
            between,
            after,
            3.0,
            &format!("{name}: floor between the partials and after them"),
        );
    }
}

/// The chord of `test_signals::demo_block`, so it can be taken out again.
fn demo_chord(start: usize, count: usize) -> (Vec<f32>, Vec<f32>) {
    let (mut left, mut right) = (vec![0.0f32; count], vec![0.0f32; count]);
    for i in 0..count {
        let time = (start + i) as f64 / 48_000.0;
        let root = [110.0, 130.81, 146.83, 98.0][(time / 2.0) as usize % 4];
        let mut chord = 0.0f32;
        for (k, m) in [1.0, 1.5, 2.0, 2.52, 3.0, 4.0].into_iter().enumerate() {
            chord += (2.0 * PI * root * m * time).sin() as f32 * 0.07 / (k + 1) as f32;
        }
        let pan = (time * 0.7).sin() as f32;
        left[i] = chord * (1.0 - 0.4 * pan);
        right[i] = chord * (1.0 + 0.4 * pan);
    }
    (left, right)
}

/// The demo signal with and without its chord: away from the lobes of the
/// partials the two curves are the same curve. A floor that is lifted between
/// close partials, or that collapses after the last one, shows here.
#[test]
fn demo_floor_is_the_same_with_and_without_the_chord() {
    let with = run(8.0, &[], demo).pop().unwrap();
    let without = run(8.0, &[], |start, count| {
        let (left, right) = demo(start, count);
        let (chord_left, chord_right) = demo_chord(start, count);
        let minus = |a: &[f32], b: &[f32]| a.iter().zip(b).map(|(x, y)| x - y).collect();
        (minus(&left, &chord_left), minus(&right, &chord_right))
    })
    .pop()
    .unwrap();
    let partials = CHORD_PARTIALS.map(|m| m * 98.0);
    let ratio = with.frequencies[1] / with.frequencies[0];
    let (mut worst, mut worst_hz) = (0.0f32, 0.0f32);
    for (i, &f) in with.frequencies.iter().enumerate() {
        if !(f >= 100.0 && f <= 5_000.0) || in_lobe(f, &partials, ratio * ratio).is_some() {
            continue;
        }
        let error = (with.mid[i] - without.mid[i]).abs();
        if error > worst {
            (worst, worst_hz) = (error, f);
        }
    }
    assert!(worst <= 3.0, "floor differs by {worst} dB at {worst_hz} Hz");
}

/// In one 8192-point transform of the demo signal, every partial of the chord
/// is found as a tone (a neighbour partial inside the noise context must not
/// hide it), and nothing else near a partial is (a skirt bin three bins from a
/// real tone must not pass as one).
#[test]
fn every_chord_partial_is_a_tone() {
    let (frequencies, step) = grid(1024);
    let weight = vec![1.0f32; frequencies.len()];
    let size = 8192;
    let df = 48_000.0f32 / size as f32;
    let mut resolution = SpectrumResolution::new(size, 1024, 48_000.0).unwrap();
    resolution.update_map(&frequencies, step, &weight, &|_| 3.0 * df, &|_| df);
    let mut checked = 0;
    let mut t = 7.0f64;
    while t <= 8.0 {
        // A window that holds a kick onset (every 0.5 s) really is broadband at
        // 150-250 Hz: the partials are 6 dB over it there, not 14, and are
        // rightly not called tones.
        if t % 0.5 >= size as f64 / 48_000.0 {
            let end = (t * 48_000.0) as usize;
            let (left, right) = demo(end - size, size);
            resolution.transform(&left, &right, 0);
            let bins: Vec<f32> = resolution.detected_tones().iter().map(|tone| tone.bin as f32).collect();
            for m in [1.5f32, 2.0, 2.52, 3.0, 4.0] {
                let x = 98.0 * m / df;
                assert!(
                    bins.iter().any(|b| (b - x).abs() <= 1.0),
                    "t = {t}: no tone at {} Hz, found bins {bins:?}",
                    98.0 * m
                );
                assert!(
                    !bins.iter().any(|b| (b - x).abs() > 1.5 && (b - x).abs() < 6.0),
                    "t = {t}: false tone beside {} Hz, found bins {bins:?}",
                    98.0 * m
                );
            }
            checked += 1;
        }
        t += 0.04;
    }
    assert!(checked >= 10, "only {checked} windows were checked");
}
