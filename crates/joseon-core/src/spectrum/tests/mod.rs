//! Tests of the spectrum analyzer, ported from `Tests/JoseonCoreTests` in the macOS repo.
//!
//! Every test drives the analyzer the way the engine does: 800-frame blocks
//! through `process`, then one `read`. Signals come from `test_signals`, never
//! from disk.

mod accuracy;
mod layers;
mod low_end;
mod seams;
mod third_octave;
mod tone_gate;
mod top_peaks;
mod walls;

use super::*;
use crate::test_signals;

pub(super) const FLOOR: f32 = SpectrumReading::FLOOR_DB;

/// Feeds a stereo signal in blocks.
pub(super) fn feed_blocks(analyzer: &mut SpectrumAnalyzer, left: &[f32], right: &[f32], rate: f64, block: usize) {
    assert_eq!(left.len(), right.len());
    for (l, r) in left.chunks(block).zip(right.chunks(block)) {
        analyzer.process(l, r, rate);
    }
}

/// Feeds a stereo signal in 800-frame blocks (the engine's block size at 60 Hz / 48 kHz).
pub(super) fn feed(analyzer: &mut SpectrumAnalyzer, left: &[f32], right: &[f32], rate: f64) {
    feed_blocks(analyzer, left, right, rate, 800);
}

/// Feeds the same signal to both channels.
pub(super) fn feed_mono(analyzer: &mut SpectrumAnalyzer, signal: &[f32], rate: f64) {
    feed(analyzer, signal, signal, rate);
}

pub(super) fn nearest_bin(frequencies: &[f32], hz: f32) -> usize {
    let mut best = (0, f32::MAX);
    for (i, f) in frequencies.iter().enumerate() {
        let distance = (f / hz).log2().abs();
        if distance < best.1 {
            best = (i, distance);
        }
    }
    best.0
}

/// Highest value of `curve` inside a window of ±`octaves` around `hz`.
pub(super) fn peak_near_in(curve: &[f32], frequencies: &[f32], hz: f32, octaves: f32) -> f32 {
    frequencies
        .iter()
        .zip(curve)
        .filter(|(f, _)| (**f / hz).log2().abs() <= octaves)
        .fold(FLOOR, |best, (_, &level)| best.max(level))
}

/// Highest value of `mid` near `hz`. Robust to the display grid landing between two bins.
pub(super) fn peak_near(reading: &SpectrumReading, hz: f32) -> f32 {
    peak_near_in(&reading.mid, &reading.frequencies, hz, 0.08)
}

/// Sine at a level in dBFS.
pub(super) fn sine(hz: f64, dbfs: f32, rate: f64, seconds: f64) -> Vec<f32> {
    test_signals::sine(hz, 10f32.powf(dbfs / 20.0), rate, seconds, 0.0)
}

pub(super) fn add(a: &[f32], b: &[f32]) -> Vec<f32> {
    a.iter().zip(b).map(|(x, y)| x + y).collect()
}

pub(super) fn silence(rate: f64, seconds: f64) -> Vec<f32> {
    vec![0.0; (rate * seconds) as usize]
}

pub(super) fn pink(amplitude: f32, count: usize) -> Vec<f32> {
    test_signals::pink_noise(amplitude, count, test_signals::DEFAULT_SEED)
}

pub(super) fn white(amplitude: f32, count: usize) -> Vec<f32> {
    test_signals::white_noise(amplitude, count, test_signals::DEFAULT_SEED)
}

pub(super) fn index_of_max(values: &[f32]) -> usize {
    let mut best = 0;
    for (i, &v) in values.iter().enumerate() {
        if v > values[best] {
            best = i;
        }
    }
    best
}

#[track_caller]
pub(super) fn assert_near(actual: f32, expected: f32, accuracy: f32, what: &str) {
    assert!(
        (actual - expected).abs() <= accuracy,
        "{what}: {actual}, expected {expected} ± {accuracy}"
    );
}
