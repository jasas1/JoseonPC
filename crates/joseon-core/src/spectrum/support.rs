//! Small numeric helpers and note names.
//!
//! Port source: `Sources/JoseonCore/Spectrum/SpectrumSupport.swift`. The Swift
//! file also has fixed-size scratch buffers; in Rust those are plain `Vec`s
//! that are allocated at configure time and never grow.

/// Smallest power of two that is not below `v`.
pub(crate) fn next_pow2(v: usize) -> usize {
    let mut n = 1;
    while n < v {
        n <<= 1;
    }
    n
}

/// Power of two closest to `v` on a log scale, inside `lo...hi`. Keeps window
/// *durations* similar when the sample rate changes: 32768 at 48 kHz stays
/// 32768 at 44.1 kHz and becomes 65536 at 96 kHz.
pub(crate) fn nearest_pow2(v: f64, lo: usize, hi: usize) -> usize {
    if v <= 1.0 {
        return lo;
    }
    let exponent = v.log2().round().max(0.0) as u32;
    (1usize << exponent.min(62)).clamp(lo, hi)
}

const NOTE_NAMES: [&str; 12] = ["C", "C#", "D", "D#", "E", "F", "F#", "G", "G#", "A", "A#", "B"];

/// Equal temperament with A4 = 440 Hz. Returns the note name and the offset in
/// cents (-50...+50). The name is empty when the frequency is not a usable note.
pub fn note_for_hz(hz: f32) -> (String, f32) {
    if !(hz > 0.0 && hz.is_finite()) {
        return (String::new(), 0.0);
    }
    // MIDI number as a real: 69 = A4 = 440 Hz.
    let midi = 12.0 * (hz as f64 / 440.0).log2() + 69.0;
    if !(midi.is_finite() && midi > -0.5 && midi < 127.5) {
        return (String::new(), 0.0);
    }
    let nearest = midi.round() as i64;
    let cents = ((midi - nearest as f64) * 100.0) as f32;
    let octave = nearest / 12 - 1;
    (format!("{}{}", NOTE_NAMES[(nearest % 12) as usize], octave), cents)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn powers_of_two() {
        assert_eq!([0, 1, 2, 3, 1024, 1025].map(next_pow2), [1, 1, 2, 4, 1024, 2048]);
        assert_eq!(nearest_pow2(32_768.0, 4_096, 1 << 17), 32_768);
        assert_eq!(nearest_pow2(32_768.0 * 44_100.0 / 48_000.0, 4_096, 1 << 17), 32_768);
        assert_eq!(nearest_pow2(65_536.0, 4_096, 1 << 17), 65_536);
        assert_eq!(nearest_pow2(32_768.0 * 4.0, 4_096, 1 << 17), 1 << 17);
        assert_eq!(nearest_pow2(8.0, 256, 1 << 13), 256);
        assert_eq!(nearest_pow2(0.5, 256, 1 << 13), 256);
    }

    #[test]
    fn note_names() {
        assert_eq!(note_for_hz(440.0), ("A4".to_string(), 0.0));
        let (name, cents) = note_for_hz(261.63);
        assert_eq!(name, "C4");
        assert!(cents.abs() < 0.1);
        let (name, cents) = note_for_hz(415.30);
        assert_eq!(name, "G#4");
        assert!(cents.abs() < 0.1);
        // A quarter tone over A4 is on the edge: 49 cents sharp stays A4.
        let (name, cents) = note_for_hz(440.0 * 2f32.powf(0.49 / 12.0));
        assert_eq!(name, "A4");
        assert!((cents - 49.0).abs() < 0.1);
        assert_eq!(note_for_hz(27.5).0, "A0");
        assert_eq!(note_for_hz(8.18).0, "C-1");
        assert_eq!(note_for_hz(0.0).0, "");
        assert_eq!(note_for_hz(-5.0).0, "");
        assert_eq!(note_for_hz(f32::NAN).0, "");
        assert_eq!(note_for_hz(f32::INFINITY).0, "");
        assert_eq!(note_for_hz(20_000.0).0, "", "over MIDI 127");
    }
}
