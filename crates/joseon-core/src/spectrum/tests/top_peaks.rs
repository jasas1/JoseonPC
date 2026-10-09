//! Port of `TopPeaksTests.swift` and `TopPeaksTonalTests.swift`: the strongest
//! tonal peaks of mid with their note names, and the lowest frequency that
//! carries sustained content.
//!
//! Not ported here: `testTheEngineCopiesThemIntoTheFrame` moves with the
//! analysis engine (port step 5). The allocation test is in
//! `tests/spectrum_performance.rs`.

use super::*;
use std::collections::BTreeSet;
use std::f64::consts::PI;

fn chord(frequencies: &[f64], amplitudes: &[f32], seconds: f64) -> Vec<f32> {
    let rate = 48_000.0;
    let mut out = vec![0.0f32; (rate * seconds) as usize];
    for (&hz, &amplitude) in frequencies.iter().zip(amplitudes) {
        for (sum, tone) in out
            .iter_mut()
            .zip(test_signals::sine(hz, amplitude, rate, seconds, 0.0))
        {
            *sum += tone;
        }
    }
    out
}

fn analyzed(signal: &[f32]) -> SpectrumAnalyzer {
    let mut analyzer = SpectrumAnalyzer::default();
    feed_mono(&mut analyzer, signal, 48_000.0);
    analyzer
}

// MARK: top_peaks

/// Five tones of decreasing level: five peaks, strongest first, each at its own
/// frequency and level, each with the right note name.
#[test]
fn five_tones_come_back_strongest_first() {
    // A2, E3, A3, C#4, E4: a spread-out A major, all more than 1/6 octave apart.
    let hz = [110.0, 164.81, 220.0, 277.18, 329.63];
    let levels = [-6.0f32, -9.0, -12.0, -15.0, -18.0];
    let mut analyzer = analyzed(&chord(&hz, &levels.map(|db| 10f32.powf(db / 20.0)), 3.0));
    let peaks = analyzer.top_peaks();
    assert_eq!(peaks.len(), 5);
    assert!(
        peaks.windows(2).all(|pair| pair[1].level_db <= pair[0].level_db),
        "peaks are not sorted by level"
    );
    for (i, name) in ["A2", "E3", "A3", "C#4", "E4"].into_iter().enumerate() {
        assert!(
            (peaks[i].frequency_hz as f64 - hz[i]).abs() <= hz[i] * 0.01,
            "peak {i} frequency"
        );
        assert_near(peaks[i].level_db, levels[i], 0.5, &format!("peak {i} level"));
        assert_eq!(peaks[i].note_name, name, "peak {i} note");
        assert_near(peaks[i].cents, 0.0, 5.0, &format!("peak {i} cents"));
    }
}

/// The contract: the first entry of the list is the same peak as `peak`.
#[test]
fn first_peak_is_the_peak_reading() {
    let mut analyzer = analyzed(&chord(&[440.0, 880.0, 1_318.5], &[0.5, 0.2, 0.1], 2.5));
    let peak = analyzer.read().peak;
    assert_eq!(analyzer.top_peaks().first(), Some(&peak));
}

/// At most five, however many tones there are.
#[test]
fn at_most_five() {
    let hz = [110.0, 147.0, 196.0, 262.0, 349.0, 466.0, 622.0, 831.0];
    let mut analyzer = analyzed(&chord(&hz, &[0.2; 8], 3.0));
    assert_eq!(analyzer.top_peaks().len(), MAX_TOP_PEAKS);
}

/// One tone must not fill the list with its own skirt: entries stay 1/6 octave apart.
#[test]
fn entries_are_at_least_one_sixth_of_an_octave_apart() {
    // 1000 and 1010 Hz are 0.014 octave apart: one entry, not two.
    let mut analyzer = analyzed(&chord(&[1_000.0, 1_010.0, 4_000.0], &[0.5, 0.45, 0.3], 3.0));
    let peaks = analyzer.top_peaks();
    assert!(!peaks.is_empty());
    for i in 0..peaks.len() {
        for j in i + 1..peaks.len() {
            let octaves = (peaks[i].frequency_hz / peaks[j].frequency_hz).log2().abs();
            assert!(
                octaves >= TOP_PEAK_SEPARATION_OCTAVES - 1e-4,
                "{} Hz and {} Hz are {octaves} octaves apart",
                peaks[i].frequency_hz,
                peaks[j].frequency_hz
            );
        }
    }
}

/// Noise has no tonal peak to name, so the list is empty.
#[test]
fn noise_and_silence_give_no_peaks() {
    assert!(
        analyzed(&white(0.3, 96_000)).top_peaks().is_empty(),
        "white noise is not tonal"
    );
    assert!(analyzed(&silence(48_000.0, 2.0)).top_peaks().is_empty());
}

/// A tone under the −80 dBFS gate is not reported.
#[test]
fn a_very_quiet_tone_is_below_the_gate() {
    assert!(analyzed(&sine(1_000.0, -90.0, 48_000.0, 2.0)).top_peaks().is_empty());
}

/// `reset` starts a new measurement: the list empties until new audio arrives.
#[test]
fn reset_clears_the_list() {
    let mut analyzer = analyzed(&chord(&[440.0, 880.0], &[0.5, 0.3], 2.0));
    assert!(!analyzer.top_peaks().is_empty());
    analyzer.reset();
    assert!(analyzer.top_peaks().is_empty());
}

// MARK: lowest_strong_hz

/// Zero before three seconds have been measured, then the low tone of the pair.
#[test]
fn lowest_strong_needs_three_seconds_and_then_finds_the_low_tone() {
    let mut analyzer = analyzed(&chord(&[80.0, 2_000.0], &[0.5, 0.5], 2.0));
    assert_eq!(analyzer.lowest_strong_hz(), 0.0, "nothing before 3 s of measurement");
    feed_mono(&mut analyzer, &chord(&[80.0, 2_000.0], &[0.5, 0.5], 4.0), 48_000.0);
    let hz = analyzer.lowest_strong_hz();
    assert!(hz > 0.0);
    assert_near(hz, 80.0, 80.0 * 0.15, "lowest strong content");
}

/// Content more than 12 dB under the loudest part of the smoothed average does not count.
#[test]
fn quiet_low_content_is_not_strong() {
    // 40 Hz is 50 dB under the 1 kHz tone: far below the 12 dB window.
    let mut analyzer = analyzed(&chord(&[40.0, 1_000.0], &[10f32.powf(-50.0 / 20.0) * 0.5, 0.5], 5.0));
    let hz = analyzer.lowest_strong_hz();
    assert!(hz > 200.0, "40 Hz is too quiet to count, got {hz} Hz");
    assert_near(hz, 1_000.0, 200.0, "lowest strong content");
}

/// `reset` starts the three-second clock again.
#[test]
fn reset_clears_lowest_strong() {
    let mut analyzer = analyzed(&chord(&[80.0, 2_000.0], &[0.5, 0.5], 5.0));
    assert!(analyzer.lowest_strong_hz() > 0.0);
    analyzer.reset();
    assert_eq!(analyzer.lowest_strong_hz(), 0.0);
}

// MARK: A note name belongs to a tone, and a marker belongs on the curve

/// Feeds like the engine does: 800 frames, then a read. `each` runs after every
/// block with the time in seconds.
fn run(
    analyzer: &mut SpectrumAnalyzer,
    seconds: f64,
    signal: impl Fn(usize, usize) -> (Vec<f32>, Vec<f32>),
    mut each: impl FnMut(f64, &mut SpectrumAnalyzer),
) -> SpectrumOutput {
    let rate = 48_000.0;
    let block = 800;
    let mut last = analyzer.read();
    for k in 0..(seconds * 60.0) as usize {
        let (left, right) = signal(k * block, block);
        analyzer.process(&left, &right, rate);
        last = analyzer.read();
        each(((k + 1) * block) as f64 / rate, analyzer);
    }
    last
}

fn mono(count: usize, sample: impl Fn(usize) -> f32) -> (Vec<f32>, Vec<f32>) {
    let x: Vec<f32> = (0..count).map(sample).collect();
    (x.clone(), x)
}

fn tone(hz: f64, amplitude: f64) -> impl Fn(usize, usize) -> (Vec<f32>, Vec<f32>) {
    move |start, count| {
        mono(count, |i| {
            (amplitude * (2.0 * PI * hz * (start + i) as f64 / 48_000.0).sin()) as f32
        })
    }
}

fn demo(start: usize, count: usize) -> (Vec<f32>, Vec<f32>) {
    test_signals::demo_block(start as i64, count, 48_000.0)
}

/// The kick drum of the demo signal over a little pink noise: one broad hump.
fn kick(start: usize, count: usize) -> (Vec<f32>, Vec<f32>) {
    let noise = test_signals::pink_noise(0.02, 1 << 16, 0x4B1C);
    mono(count, |i| {
        let beat = ((start + i) as f64 / 48_000.0) % 0.5;
        ((-beat * 18.0).exp() * (2.0 * PI * (48.0 + 60.0 * (-beat * 30.0).exp()) * beat).sin()) as f32 * 0.5
            + noise[(start + i) % noise.len()]
    })
}

/// The mid curve at the display bin nearest to `hz`, or one bin beside it.
fn curve_value(reading: &SpectrumReading, hz: f32) -> f32 {
    let i = nearest_bin(&reading.frequencies, hz);
    reading.mid[i.saturating_sub(1)..=(i + 1).min(reading.mid.len() - 1)]
        .iter()
        .copied()
        .fold(f32::MIN, f32::max)
}

/// The demo signal at 8 s: chord root 98 Hz. The list is partials of the chord,
/// by name, and nothing on the 35–110 Hz kick hump.
#[test]
fn demo_signal_names_the_chord_not_the_kick() {
    let mut analyzer = SpectrumAnalyzer::default();
    let out = run(&mut analyzer, 8.0, demo, |_, _| {});
    let peaks = analyzer.top_peaks();
    let chord = [
        ("G2", 98.0f32),
        ("D3", 147.0),
        ("G3", 196.0),
        ("B3", 246.96),
        ("D4", 294.0),
        ("G4", 392.0),
    ];
    assert!(peaks.len() >= 4, "the chord has six partials over the gate");
    for p in &peaks {
        let expected = chord.iter().find(|(name, _)| *name == p.note_name);
        let Some(&(_, expected)) = expected else {
            panic!("{} at {} Hz is not a partial of the chord", p.note_name, p.frequency_hz);
        };
        assert_near(p.frequency_hz, expected, expected * 0.005, &p.note_name);
        // On the curve: the display bin the marker lands in reads the same level.
        assert_near(
            p.level_db,
            curve_value(&out.spectrum, p.frequency_hz),
            0.05,
            &format!("{} floats off the mid curve", p.note_name),
        );
    }
    assert_eq!(
        peaks.first(),
        Some(&out.peak),
        "a list that is not empty starts with `peak`"
    );
    assert_eq!(out.peak.note_name, "G2");
    assert_near(out.peak.cents, 0.0, 3.0, "cents");
}

/// With a display tilt the marker follows the documented offset rule.
#[test]
fn levels_follow_the_tilt_offset_rule() {
    let mut analyzer = SpectrumAnalyzer::new(SpectrumSettings {
        tilt_db_per_octave: 4.5,
        ..Default::default()
    });
    let out = run(&mut analyzer, 8.0, demo, |_, _| {});
    let peaks = analyzer.top_peaks();
    assert!(peaks.len() >= 4);
    for p in &peaks {
        let on_curve = p.level_db + 4.5 * (p.frequency_hz / 1_000.0).log2();
        assert_near(on_curve, curve_value(&out.spectrum, p.frequency_hz), 0.1, &p.note_name);
    }
}

/// A kick drum and a little noise: one broad hump. `peak` says where it is,
/// names no note, and the list is empty.
#[test]
fn a_kick_hump_gets_no_note() {
    let mut analyzer = SpectrumAnalyzer::default();
    let mut named = 0;
    let out = run(&mut analyzer, 4.0, kick, |_, analyzer| {
        named += analyzer.top_peaks().iter().filter(|p| !p.note_name.is_empty()).count();
    });
    assert_eq!(named, 0, "a note was named on the kick hump in some frame");
    assert!(analyzer.top_peaks().is_empty());
    assert_eq!(out.peak.note_name, "");
    assert!(
        out.peak.frequency_hz > 30.0 && out.peak.frequency_hz < 120.0,
        "peak at {} Hz",
        out.peak.frequency_hz
    );
    assert_near(
        out.peak.level_db,
        curve_value(&out.spectrum, out.peak.frequency_hz),
        0.05,
        "the peak marker floats off the mid curve",
    );
}

/// Tones keep their names down to the bottom of the range, where the analyzer's
/// own lobe is wider than 1/12 octave. And a broad peak first in the list does
/// not push a tone out of it.
#[test]
fn low_tones_and_a_tonal_list_behind_a_broad_peak() {
    let mut low = SpectrumAnalyzer::default();
    run(&mut low, 3.0, tone(30.87, 0.25), |_, _| {});
    assert_eq!(
        low.top_peaks().first().map(|p| p.note_name.clone()),
        Some("B0".to_string())
    );

    // A loud kick and a 440 Hz tone 10 dB under its hump: `peak` is the hump, the list still has A4.
    let mut analyzer = SpectrumAnalyzer::default();
    let out = run(
        &mut analyzer,
        4.0,
        |start, count| {
            let (kick, _) = kick(start, count);
            let (a4, _) = tone(440.0, 0.02)(start, count);
            let mixed = add(&kick, &a4);
            (mixed.clone(), mixed)
        },
        |_, _| {},
    );
    let peaks = analyzer.top_peaks();
    assert_eq!(out.peak.note_name, "");
    assert_eq!(peaks.first(), Some(&out.peak));
    assert_eq!(
        peaks.iter().skip(1).map(|p| p.note_name.as_str()).collect::<Vec<_>>(),
        ["A4"]
    );
}

#[test]
fn demo_lowest_strong_is_the_kick() {
    let mut analyzer = SpectrumAnalyzer::default();
    let mut early = -1.0f32;
    let mut values = BTreeSet::new();
    run(&mut analyzer, 8.0, demo, |t, analyzer| {
        if t < 2.9 {
            early = early.max(analyzer.lowest_strong_hz());
        }
        if t > 5.0 {
            values.insert(analyzer.lowest_strong_hz() as i32);
        }
    });
    let hz = analyzer.lowest_strong_hz();
    assert_eq!(early, 0.0, "nothing before 3 s");
    assert!((30.0..=45.0).contains(&hz), "lowest strong content at {hz} Hz");
    assert_eq!(hz, hz.round(), "whole Hz");
    assert!(values.len() <= 3, "the number should hold still, got {values:?}");
}

#[test]
fn lowest_strong_of_one_tone_silence_and_infrasound() {
    let mut analyzer = SpectrumAnalyzer::default();
    run(&mut analyzer, 5.0, tone(1_000.0, 0.5), |_, _| {});
    assert_near(analyzer.lowest_strong_hz(), 1_000.0, 50.0, "one tone");

    let mut analyzer = SpectrumAnalyzer::default();
    run(
        &mut analyzer,
        5.0,
        |_, count| (vec![0.0; count], vec![0.0; count]),
        |_, _| {},
    );
    assert_eq!(analyzer.lowest_strong_hz(), 0.0);

    // 12 Hz is on the plot (it starts at 10 Hz) but it is not an answer: never under 20 Hz.
    let mut analyzer = SpectrumAnalyzer::default();
    run(&mut analyzer, 5.0, tone(12.0, 0.5), |_, _| {});
    let hz = analyzer.lowest_strong_hz();
    assert!(hz == 0.0 || hz >= 20.0, "got {hz} Hz");
}
