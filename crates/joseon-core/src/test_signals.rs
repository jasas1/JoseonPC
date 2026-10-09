//! Synthetic signals for tests, the probe and demo mode.
//!
//! Port source: `Sources/JoseonCore/TestSignals.swift`. The arithmetic is the
//! same operation by operation, in the same number types, so both apps make
//! the same samples. `sin` and `exp` come from the system math library, so the
//! last bit can differ between platforms.

use crate::contracts::{AudioSource, SourceError, StreamInfo, StreamInfoCallback};
use crate::ring_buffer::{stereo_ring_buffer, RingReader, RingWriter, DEFAULT_CAPACITY};
use std::f64::consts::PI;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

/// The seed of the deterministic noise: the bytes of "Joseon".
pub const DEFAULT_SEED: u64 = 0x4A6F_7365_6F6E;

/// Sine at `hz`, peak amplitude `amplitude` (1.0 = 0 dBFS), `phase` in radians.
pub fn sine(hz: f64, amplitude: f32, sample_rate: f64, seconds: f64, phase: f64) -> Vec<f32> {
    let count = (sample_rate * seconds) as usize;
    (0..count)
        .map(|i| amplitude * (2.0 * PI * hz * i as f64 / sample_rate + phase).sin() as f32)
        .collect()
}

/// Deterministic white noise in -amplitude...amplitude.
pub fn white_noise(amplitude: f32, count: usize, seed: u64) -> Vec<f32> {
    let mut state = seed;
    (0..count)
        .map(|_| {
            state = state
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            let unit = (state >> 40) as f32 / (1u32 << 24) as f32;
            (unit * 2.0 - 1.0) * amplitude
        })
        .collect()
}

/// Pink-ish noise (Paul Kellet filter) from the deterministic white noise.
pub fn pink_noise(amplitude: f32, count: usize, seed: u64) -> Vec<f32> {
    let white = white_noise(1.0, count, seed);
    let (mut b0, mut b1, mut b2) = (0.0f32, 0.0f32, 0.0f32);
    let mut peak = 1e-9f32;
    let mut out: Vec<f32> = white
        .iter()
        .map(|&w| {
            b0 = 0.99765 * b0 + w * 0.0990460;
            b1 = 0.96300 * b1 + w * 0.2965164;
            b2 = 0.57000 * b2 + w * 1.0526913;
            let sample = b0 + b1 + b2 + w * 0.1848;
            peak = peak.max(sample.abs());
            sample
        })
        .collect();
    let gain = amplitude / peak;
    for sample in &mut out {
        *sample *= gain;
    }
    out
}

/// Pink noise table of the demo signal. Built once.
fn demo_noise() -> &'static [f32] {
    static TABLE: OnceLock<Vec<f32>> = OnceLock::new();
    TABLE.get_or_init(|| pink_noise(0.08, 1 << 16, DEFAULT_SEED))
}

/// The music-like demo signal: a kick every half second, a four-chord loop (2 s
/// per chord) that pans slowly, two quiet one-sided tones, and a pink noise
/// bed. It is a pure function of the sample position, so an offline render and
/// `DemoAudioSource` give the same samples.
///
/// The one-sided tones are real stereo content: a 6.2 kHz shimmer in the
/// **right** channel only and a 3.1 kHz tone in the **left** channel only, both
/// with a slow amplitude modulation. A tone in one channel puts energy in mid,
/// in side and in that channel, which is what a stereo view is for.
///
/// `start_sample` is the index of the first sample since the start of the
/// signal. Returns `(left, right)`.
pub fn demo_block(start_sample: i64, count: usize, sample_rate: f64) -> (Vec<f32>, Vec<f32>) {
    const ROOTS: [f64; 4] = [110.0, 130.81, 146.83, 98.0];
    const PARTIALS: [f64; 6] = [1.0, 1.5, 2.0, 2.52, 3.0, 4.0];
    let noise = demo_noise();
    let noise_len = noise.len() as i64;
    let mut left = vec![0.0f32; count];
    let mut right = vec![0.0f32; count];
    for i in 0..count {
        let sample = start_sample + i as i64;
        let time = sample as f64 / sample_rate;
        let beat = time % 0.5;
        let kick = ((-beat * 18.0).exp() * (2.0 * PI * (48.0 + 60.0 * (-beat * 30.0).exp()) * beat).sin()) as f32 * 0.5;
        let root = ROOTS[((time / 2.0) as i64).rem_euclid(4) as usize];
        let mut chord = 0.0f32;
        for (k, partial) in PARTIALS.iter().enumerate() {
            chord += (2.0 * PI * root * partial * time).sin() as f32 * 0.07 / (k + 1) as f32;
        }
        let pan = (time * 0.7).sin() as f32;
        // Right only. The modulation runs between half and full depth, so the tone breathes.
        let shimmer =
            ((2.0 * PI * 6_200.0 * time).sin() * (0.75 + 0.25 * (2.0 * PI * 3.0 * time).sin())) as f32 * 0.0047;
        // Left only, quieter, at a different rate so the two never line up.
        let sparkle =
            ((2.0 * PI * 3_100.0 * time).sin() * (0.75 + 0.25 * (2.0 * PI * 1.7 * time).sin())) as f32 * 0.0030;
        let index = sample.rem_euclid(noise_len);
        left[i] = kick + chord * (1.0 - 0.4 * pan) + sparkle + noise[index as usize] * 0.5;
        right[i] = kick + chord * (1.0 + 0.4 * pan) + shimmer + noise[((index + 4002) % noise_len) as usize] * 0.5;
    }
    (left, right)
}

/// The same, addressed by time in seconds since the start of the signal.
pub fn demo_block_at_time(start_time: f64, count: usize, sample_rate: f64) -> (Vec<f32>, Vec<f32>) {
    demo_block((start_time * sample_rate).round() as i64, count, sample_rate)
}

/// A looping synthetic `AudioSource`: the demo signal in real time, in 10 ms
/// blocks. Used when capture is not possible.
pub struct DemoAudioSource {
    stream_info: Option<StreamInfo>,
    reader: Option<RingReader>,
    /// `Some` while stopped. The demo thread owns the writer while it runs and
    /// gives it back when it ends.
    writer: Option<RingWriter>,
    on_stream_info_change: Option<StreamInfoCallback>,
    stop_flag: Arc<AtomicBool>,
    thread: Option<JoinHandle<RingWriter>>,
}

impl DemoAudioSource {
    pub const SAMPLE_RATE: f64 = 48_000.0;

    pub fn new() -> Self {
        let (writer, reader) = stereo_ring_buffer(DEFAULT_CAPACITY);
        Self {
            stream_info: None,
            reader: Some(reader),
            writer: Some(writer),
            on_stream_info_change: None,
            stop_flag: Arc::new(AtomicBool::new(false)),
            thread: None,
        }
    }
}

impl Default for DemoAudioSource {
    fn default() -> Self {
        Self::new()
    }
}

impl AudioSource for DemoAudioSource {
    fn stream_info(&self) -> Option<StreamInfo> {
        self.stream_info.clone()
    }

    fn take_reader(&mut self) -> Option<RingReader> {
        self.reader.take()
    }

    fn set_on_stream_info_change(&mut self, callback: Option<StreamInfoCallback>) {
        self.on_stream_info_change = callback;
    }

    fn start(&mut self) -> Result<(), SourceError> {
        let Some(mut writer) = self.writer.take() else {
            return Ok(()); // already running
        };
        let info = StreamInfo {
            bit_depth: Some(32),
            active_sources: vec!["Joseon demo".to_string()],
            ..StreamInfo::new(Self::SAMPLE_RATE, 2, "Demo signal")
        };
        self.stream_info = Some(info.clone());
        if let Some(callback) = &self.on_stream_info_change {
            callback(info);
        }

        self.stop_flag.store(false, Ordering::Relaxed);
        let stop = Arc::clone(&self.stop_flag);
        let spawned = std::thread::Builder::new()
            .name("joseon.demo".to_string())
            .spawn(move || {
                let rate = DemoAudioSource::SAMPLE_RATE;
                let block = (rate / 100.0) as usize;
                let period = Duration::from_millis(10);
                let mut position = 0i64;
                let mut next = Instant::now();
                while !stop.load(Ordering::Relaxed) {
                    let (left, right) = demo_block(position, block, rate);
                    position += block as i64;
                    writer.write(&left, &right, rate);
                    next += period;
                    let now = Instant::now();
                    match next.checked_duration_since(now) {
                        Some(wait) => std::thread::sleep(wait),
                        // More than one period late (the machine slept): start a new schedule.
                        None => next = now,
                    }
                }
                writer
            });
        match spawned {
            Ok(thread) => {
                self.thread = Some(thread);
                Ok(())
            }
            Err(error) => Err(SourceError::Platform(error.to_string())),
        }
    }

    fn stop(&mut self) {
        self.stop_flag.store(true, Ordering::Relaxed);
        if let Some(thread) = self.thread.take() {
            // A join error means the demo thread panicked. The writer is then
            // gone and the source stays stopped.
            self.writer = thread.join().ok();
        }
    }

    fn is_running(&self) -> bool {
        self.thread.is_some()
    }
}

impl Drop for DemoAudioSource {
    fn drop(&mut self) {
        self.stop();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    /// Port of `TestSignalsTests.testDemoBlockIsAPureFunctionOfTheSamplePosition`.
    #[test]
    fn demo_block_is_a_pure_function_of_the_sample_position() {
        let whole = demo_block(0, 2_000, 48_000.0);
        let first = demo_block(0, 800, 48_000.0);
        let second = demo_block_at_time(800.0 / 48_000.0, 1_200, 48_000.0);
        assert_eq!(whole.0, [first.0, second.0].concat());
        assert_eq!(whole.1, [first.1, second.1].concat());
        assert_ne!(whole.0, whole.1);
        let peak = whole.0.iter().chain(&whole.1).fold(0.0f32, |peak, x| peak.max(x.abs()));
        assert!(peak > 0.05 && peak < 1.0, "peak {peak}");
        // The table wraps without a fault, far into the signal.
        assert_eq!(demo_block(48_000 * 3_600, 480, 48_000.0).0.len(), 480);
        // A negative position is valid too.
        let before = demo_block(-480, 480, 48_000.0);
        assert!(before.0.iter().chain(&before.1).all(|x| x.is_finite()));
    }

    // MARK: Parity with the macOS app
    //
    // The reference numbers come from the Swift `TestSignals` in the macOS repo,
    // release build, Apple silicon, 2026-10-07: sample count, FNV-1a hash of the
    // sample bit patterns, and the bit patterns of the first four samples.

    fn fnv1a(samples: &[f32]) -> u64 {
        samples.iter().fold(0xcbf2_9ce4_8422_2325, |hash, x| {
            (hash ^ x.to_bits() as u64).wrapping_mul(0x0100_0000_01b3)
        })
    }

    /// The noise uses only integer steps and f32 add and multiply. IEEE 754
    /// fixes those results, so the match is bit for bit on every platform.
    #[test]
    fn noise_matches_the_macos_app_bit_for_bit() {
        let white = white_noise(0.25, 50_000, DEFAULT_SEED);
        assert_eq!(
            white[..4].iter().map(|x| x.to_bits()).collect::<Vec<_>>(),
            [0x3dabde40, 0x3d53d568, 0xbe7183ac, 0x3e7f6376]
        );
        assert_eq!(fnv1a(&white), 0xeed2e725f9d5db23);
        let pink = pink_noise(0.5, 1 << 16, DEFAULT_SEED);
        assert_eq!(
            pink[..4].iter().map(|x| x.to_bits()).collect::<Vec<_>>(),
            [0x3d28f024, 0x3d4df217, 0xbda9014c, 0x3d9f9c7a]
        );
        assert_eq!(fnv1a(&pink), 0x6097dc246eda60a1);
    }

    /// `sin` and `exp` come from the system math library. On macOS that is the
    /// library the Swift app uses, so the match is bit for bit. Other systems
    /// can differ in the last bit, so there the first samples are compared with
    /// a tolerance of 1e-6 of full scale (−120 dB).
    #[test]
    fn tones_and_demo_signal_match_the_macos_app() {
        let tone = sine(997.0, 0.5, 44_100.0, 1.0, 0.3);
        let (demo_left, demo_right) = demo_block(0, 96_000, 48_000.0);
        let (late_left, late_right) = demo_block(48_000 * 3_600, 4_800, 96_000.0);
        type Case<'a> = (&'a str, &'a [f32], usize, u64, [u32; 4]);
        let cases: [Case; 5] = [
            (
                "sine",
                &tone,
                44_100,
                0xb4e69a0c3236722d,
                [0x3e174e6d, 0x3e5b078a, 0x3e8d2b94, 0x3ea9fb65],
            ),
            (
                "demo left",
                &demo_left,
                96_000,
                0xe5f03ca16475f46f,
                [0x3b583d9e, 0x3c85b286, 0x3c921736, 0x3d2e2796],
            ),
            (
                "demo right",
                &demo_right,
                96_000,
                0xd74fc3069d19468e,
                [0x3c4d17e1, 0x3cea1f40, 0x3cf4d496, 0x3d3759ff],
            ),
            (
                "demo left, 1 h in, 96 kHz",
                &late_left,
                4_800,
                0x5c175721deb98418,
                [0xbc932632, 0xbc7ff83c, 0xbb7b1418, 0xbb8553e8],
            ),
            (
                "demo right, 1 h in, 96 kHz",
                &late_right,
                4_800,
                0x5740f07c4920af2d,
                [0x3b834152, 0x3c9d60a6, 0x3ca7da75, 0x3cded5f6],
            ),
        ];
        for (name, samples, count, hash, first) in cases {
            assert_eq!(samples.len(), count, "{name}");
            for (i, bits) in first.into_iter().enumerate() {
                let expected = f32::from_bits(bits);
                assert!(
                    (samples[i] - expected).abs() < 1e-6,
                    "{name}[{i}]: {} vs {expected}",
                    samples[i]
                );
            }
            if cfg!(target_os = "macos") {
                assert_eq!(fnv1a(samples), hash, "{name}: not bit for bit");
            }
        }
    }

    #[test]
    fn sine_has_the_requested_length_level_and_phase() {
        let tone = sine(1_000.0, 0.5, 48_000.0, 0.5, 0.0);
        assert_eq!(tone.len(), 24_000);
        assert_eq!(tone[0], 0.0);
        // 1 kHz at 48 kHz: sample 12 is a quarter period, the positive peak.
        assert!((tone[12] - 0.5).abs() < 1e-6);
        let peak = tone.iter().fold(0.0f32, |peak, x| peak.max(x.abs()));
        assert!((peak - 0.5).abs() < 1e-6);
        let cosine = sine(1_000.0, 0.5, 48_000.0, 0.5, PI / 2.0);
        assert!((cosine[0] - 0.5).abs() < 1e-6);
    }

    #[test]
    fn white_noise_is_deterministic_and_inside_the_amplitude() {
        let a = white_noise(0.25, 50_000, DEFAULT_SEED);
        assert_eq!(a, white_noise(0.25, 50_000, DEFAULT_SEED));
        assert_ne!(a, white_noise(0.25, 50_000, 1));
        assert!(a.iter().all(|x| x.abs() <= 0.25));
        let mean = a.iter().sum::<f32>() / a.len() as f32;
        assert!(mean.abs() < 0.005, "mean {mean}");
        // Uniform noise in ±A has RMS A/√3.
        let rms = (a.iter().map(|x| x * x).sum::<f32>() / a.len() as f32).sqrt();
        assert!((rms - 0.25 / 3f32.sqrt()).abs() < 0.003, "rms {rms}");
        // The first values, fixed by hand from the generator: state = seed·a + c,
        // unit = (state >> 40) / 2^24.
        let state = DEFAULT_SEED
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        let expected = ((state >> 40) as f32 / 16_777_216.0 * 2.0 - 1.0) * 0.25;
        assert_eq!(a[0], expected);
    }

    #[test]
    fn pink_noise_peaks_at_the_amplitude_and_falls_with_frequency() {
        let pink = pink_noise(0.5, 1 << 16, DEFAULT_SEED);
        let peak = pink.iter().fold(0.0f32, |peak, x| peak.max(x.abs()));
        assert!((peak - 0.5).abs() < 1e-6, "peak {peak}");
        // The mean square of the first difference weights high frequencies.
        // Relative to the signal power it is 2 for white noise and far less for pink.
        let tilt = |x: &[f32]| {
            let power: f32 = x.iter().map(|v| v * v).sum();
            let difference: f32 = x.windows(2).map(|w| (w[1] - w[0]).powi(2)).sum();
            difference / power
        };
        let white = white_noise(0.5, 1 << 16, DEFAULT_SEED);
        assert!((tilt(&white) - 2.0).abs() < 0.05, "white {}", tilt(&white));
        assert!(tilt(&pink) < 0.5, "pink {}", tilt(&pink));
    }

    #[test]
    fn demo_source_plays_the_demo_signal_in_real_time() {
        let mut source = DemoAudioSource::new();
        assert!(!source.is_running());
        assert!(source.stream_info().is_none());
        let seen = Arc::new(Mutex::new(Vec::new()));
        let sink = Arc::clone(&seen);
        source.set_on_stream_info_change(Some(Box::new(move |info| sink.lock().unwrap().push(info))));
        let mut reader = source.take_reader().expect("the first call gives the reader");
        assert!(source.take_reader().is_none(), "the reader is given once");

        source.start().unwrap();
        source.start().unwrap(); // a second start does nothing
        assert!(source.is_running());
        let info = source.stream_info().unwrap();
        assert_eq!(
            (info.sample_rate, info.channel_count, info.device_name.as_str()),
            (48_000.0, 2, "Demo signal")
        );
        assert_eq!(seen.lock().unwrap().as_slice(), &[info]);

        std::thread::sleep(Duration::from_millis(300));
        source.stop();
        assert!(!source.is_running());

        let (mut left, mut right) = (vec![0.0; 48_000], vec![0.0; 48_000]);
        let n = reader.read(&mut left, &mut right);
        // 300 ms is 14 400 frames. Allow a slow, loaded machine.
        assert!((4_800..=24_000).contains(&n), "{n} frames in 300 ms");
        assert_eq!(n % 480, 0, "whole 10 ms blocks");
        let (expected_left, expected_right) = demo_block(0, n, 48_000.0);
        assert_eq!(&left[..n], expected_left.as_slice());
        assert_eq!(&right[..n], expected_right.as_slice());

        // A stopped source writes nothing more, and it can start again from the top.
        std::thread::sleep(Duration::from_millis(50));
        assert_eq!(reader.available_frames(), 0);
        source.start().unwrap();
        std::thread::sleep(Duration::from_millis(100));
        source.stop();
        let n = reader.read(&mut left, &mut right);
        assert!(n >= 480);
        assert_eq!(&left[..480], &demo_block(0, 480, 48_000.0).0[..]);
    }
}
