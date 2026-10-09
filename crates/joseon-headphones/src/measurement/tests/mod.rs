//! The whole measurement chain, simulated in code. Nothing here opens a device,
//! plays a sound or records anything: a "microphone" is a transfer function and
//! a "recording" is a vector.
//!
//! ```text
//! sweep → distortion → headphone → interface gain → clock drift → delay → microphone → noise
//! ```
//!
//! Every stage has a closed form, so every test compares against a truth it
//! computed, not against another run of the same code.
//!
//! Ported from `Tests/JoseonHeadphonesTests/MeasurementTestSupport.swift`.

mod analysis;
mod calibration;
mod honesty;
mod pipeline;
mod result_page;
mod sweep;

use super::fft::{next_power_of_two, rms, HalfSpectrum, RealFft, SplitMix64};
use super::sweep_signal::{SweepOptions, SweepSignal};
use crate::curve::{CurveInterpolator, HeadphoneCurve};
use std::f64::consts::PI;

// MARK: Biquads

/// One RBJ-cookbook biquad. `process` is the real filter; `response_db` is its exact magnitude.
#[derive(Clone, Copy)]
pub(crate) struct Biquad {
    b0: f64,
    b1: f64,
    b2: f64,
    a1: f64,
    a2: f64,
}

impl Biquad {
    pub fn high_pass(hz: f64, q: f64, sample_rate: f64) -> Self {
        let w = 2.0 * PI * hz / sample_rate;
        let alpha = w.sin() / (2.0 * q);
        let cosw = w.cos();
        let a0 = 1.0 + alpha;
        Self {
            b0: (1.0 + cosw) / 2.0 / a0,
            b1: -(1.0 + cosw) / a0,
            b2: (1.0 + cosw) / 2.0 / a0,
            a1: -2.0 * cosw / a0,
            a2: (1.0 - alpha) / a0,
        }
    }

    /// A **first-order** high-pass, by bilinear transform of `s / (s + ω₀)`.
    /// This is the shape of a seal leak: 6 dB per octave, no resonance.
    pub fn first_order_high_pass(hz: f64, sample_rate: f64) -> Self {
        let k = (PI * hz / sample_rate).tan();
        Self {
            b0: 1.0 / (1.0 + k),
            b1: -1.0 / (1.0 + k),
            b2: 0.0,
            a1: (k - 1.0) / (1.0 + k),
            a2: 0.0,
        }
    }

    pub fn peaking(hz: f64, gain_db: f64, q: f64, sample_rate: f64) -> Self {
        let a = 10.0f64.powf(gain_db / 40.0);
        let w = 2.0 * PI * hz / sample_rate;
        let alpha = w.sin() / (2.0 * q);
        let cosw = w.cos();
        let a0 = 1.0 + alpha / a;
        Self {
            b0: (1.0 + alpha * a) / a0,
            b1: -2.0 * cosw / a0,
            b2: (1.0 - alpha * a) / a0,
            a1: -2.0 * cosw / a0,
            a2: (1.0 - alpha / a) / a0,
        }
    }

    /// Exact magnitude in dB at one frequency.
    pub fn response_db(&self, hz: f64, sample_rate: f64) -> f64 {
        let w = 2.0 * PI * hz / sample_rate;
        let (sw, cw) = w.sin_cos();
        let (s2w, c2w) = (2.0 * w).sin_cos();
        let numerator_re = self.b0 + self.b1 * cw + self.b2 * c2w;
        let numerator_im = -(self.b1 * sw + self.b2 * s2w);
        let denominator_re = 1.0 + self.a1 * cw + self.a2 * c2w;
        let denominator_im = -(self.a1 * sw + self.a2 * s2w);
        let numerator = (numerator_re * numerator_re + numerator_im * numerator_im).sqrt();
        let denominator = (denominator_re * denominator_re + denominator_im * denominator_im).sqrt();
        if !(denominator > 0.0) {
            return 0.0;
        }
        20.0 * (numerator / denominator).log10()
    }
}

#[derive(Clone)]
pub(crate) struct FilterCascade {
    pub stages: Vec<Biquad>,
    pub sample_rate: f64,
}

impl FilterCascade {
    /// The headphone under test: bass roll-off, +6 dB at 3 kHz, −8 dB at 8 kHz.
    pub fn headphone(sample_rate: f64) -> Self {
        Self {
            stages: vec![
                Biquad::high_pass(35.0, 0.707, sample_rate),
                Biquad::peaking(3_000.0, 6.0, 2.0, sample_rate),
                Biquad::peaking(8_000.0, -8.0, 3.0, sample_rate),
            ],
            sample_rate,
        }
    }

    pub fn process(&self, x: &[f64]) -> Vec<f64> {
        let mut y = x.to_vec();
        for stage in &self.stages {
            let (mut x1, mut x2, mut y1, mut y2) = (0.0, 0.0, 0.0, 0.0);
            for v in &mut y {
                let input = *v;
                let output = stage.b0 * input + stage.b1 * x1 + stage.b2 * x2 - stage.a1 * y1 - stage.a2 * y2;
                x2 = x1;
                x1 = input;
                y2 = y1;
                y1 = output;
                *v = output;
            }
        }
        y
    }

    pub fn response_db(&self, hz: f64) -> f64 {
        self.stages.iter().map(|s| s.response_db(hz, self.sample_rate)).sum()
    }

    pub fn response_on(&self, grid: &[f64]) -> Vec<f64> {
        grid.iter().map(|&hz| self.response_db(hz)).collect()
    }
}

// MARK: The simulated chain

#[derive(Clone)]
pub(crate) struct SimulatedChain {
    pub sample_rate: f64,
    pub headphone: FilterCascade,
    /// Points of the response of the microphone, as `(hz, dB)`. Applied with
    /// zero phase, as a magnitude calibration file describes it.
    pub mic_curve: Vec<(f64, f64)>,
    /// Delay between play and record, in samples.
    pub delay_samples: usize,
    pub tail_samples: usize,
    /// Broadband signal-to-noise ratio of the recording, in dB. `None` for a silent room.
    pub snr_db: Option<f64>,
    /// Injected 2nd and 3rd harmonic, as a ratio of the fundamental (0.01 = 1 %).
    pub second_harmonic: f64,
    pub third_harmonic: f64,
    /// Clock difference between the two devices, in parts per million.
    pub drift_ppm: f64,
    /// Everything linear between the headphone terminals and the samples.
    pub gain: f64,
    pub seed: u64,
}

impl SimulatedChain {
    pub fn new(sample_rate: f64, headphone: FilterCascade) -> Self {
        Self {
            sample_rate,
            headphone,
            mic_curve: Vec::new(),
            delay_samples: 3_733,
            tail_samples: 6_000,
            snr_db: None,
            second_harmonic: 0.0,
            third_harmonic: 0.0,
            drift_ppm: 0.0,
            gain: 1.0,
            seed: 0xC0FFEE,
        }
    }

    /// The reference chain at 48 kHz with `snr_db` of noise.
    pub fn standard(snr_db: f64) -> Self {
        let mut chain = Self::new(SAMPLE_RATE, FilterCascade::headphone(SAMPLE_RATE));
        chain.snr_db = Some(snr_db);
        chain
    }

    /// The same chain with another noise seed.
    pub fn seeded(&self, seed: u64) -> Self {
        let mut chain = self.clone();
        chain.seed = seed;
        chain
    }

    /// The microphone response as the interpolator sees it.
    pub fn mic_calibration_curve(&self) -> HeadphoneCurve {
        HeadphoneCurve {
            name: "Test microphone".to_string(),
            source: "simulated".to_string(),
            frequencies_hz: self.mic_curve.iter().map(|p| p.0 as f32).collect(),
            levels_db: self.mic_curve.iter().map(|p| p.1 as f32).collect(),
        }
    }

    /// The exact magnitude of everything the analysis must recover, in dB, with
    /// the interface gain and (optionally) the microphone.
    pub fn true_response_db(&self, grid: &[f64], include_mic: bool) -> Vec<f64> {
        let gain_db = 20.0 * self.gain.log10();
        let mut out: Vec<f64> = grid
            .iter()
            .map(|&hz| self.headphone.response_db(hz) + gain_db)
            .collect();
        if include_mic && !self.mic_curve.is_empty() {
            let mic = CurveInterpolator::new(self.mic_calibration_curve());
            for (v, &hz) in out.iter_mut().zip(grid) {
                *v += mic.level(hz as f32) as f64;
            }
        }
        out
    }

    /// Records one run of `sweep`.
    pub fn record(&self, sweep: &SweepSignal) -> Vec<f64> {
        let mut x = sweep.samples.clone();
        // Memoryless distortion on the drive signal (a Hammerstein driver).
        if self.second_harmonic != 0.0 || self.third_harmonic != 0.0 {
            x = distort(
                &x,
                10.0f64.powf(sweep.level_dbfs / 20.0),
                self.second_harmonic,
                self.third_harmonic,
            );
        }
        x = self.headphone.process(&x);
        if self.gain != 1.0 {
            for v in &mut x {
                *v *= self.gain;
            }
        }
        if self.drift_ppm != 0.0 {
            x = resample(&x, 1.0 + self.drift_ppm * 1e-6);
        }
        let mut recording = vec![0.0; self.delay_samples + x.len() + self.tail_samples];
        recording[self.delay_samples..self.delay_samples + x.len()].copy_from_slice(&x);
        if !self.mic_curve.is_empty() {
            recording = apply_zero_phase_curve(&recording, self.mic_calibration_curve(), self.sample_rate);
        }
        if let Some(snr_db) = self.snr_db {
            let noise = pink_noise(
                recording.len(),
                rms(&x) * 10.0f64.powf(-snr_db / 20.0),
                self.seed.wrapping_add(7),
            );
            for (v, n) in recording.iter_mut().zip(&noise) {
                *v += n;
            }
        }
        recording
    }

    /// A recording of the same room with nothing playing, as long as `record` makes.
    pub fn silence(&self, sweep: &SweepSignal) -> Vec<f64> {
        let count = self.delay_samples + sweep.samples.len() + self.tail_samples;
        let Some(snr_db) = self.snr_db else {
            return vec![0.0; count];
        };
        let mut x = self.headphone.process(&sweep.samples);
        if self.gain != 1.0 {
            for v in &mut x {
                *v *= self.gain;
            }
        }
        pink_noise(
            count,
            rms(&x) * 10.0f64.powf(-snr_db / 20.0),
            self.seed.wrapping_add(99),
        )
    }
}

pub(crate) const SAMPLE_RATE: f64 = 48_000.0;

/// A 20 Hz – 20 kHz sweep at 48 kHz.
pub(crate) fn sweep(seconds: f64, level_dbfs: f64) -> SweepSignal {
    SweepSignal::exponential_sweep(
        SAMPLE_RATE,
        SweepOptions {
            seconds,
            level_dbfs,
            ..Default::default()
        },
    )
}

// MARK: Signal helpers

/// Adds a known amount of 2nd and 3rd harmonic, band limited as a real chain is.
///
/// 1. **No aliasing.** The polynomial runs at four times the rate, and the
///    result is brick-wall filtered back down.
/// 2. **No change to the fundamental.** `v³` contains `3/4·v` as well as the
///    third harmonic. That term is subtracted, so the linear gain is exactly 1.
pub(crate) fn distort(x: &[f64], amplitude: f64, d2: f64, d3: f64) -> Vec<f64> {
    let n = next_power_of_two(x.len());
    let up = 4 * n;
    let fine = RealFft::new(up);
    let coarse = RealFft::new(n);
    let mut up_spectrum = HalfSpectrum::new(up);
    let spectrum = coarse.forward(x);
    for k in 0..spectrum.bin_count() {
        up_spectrum.real[k] = spectrum.real[k] * 4.0;
        up_spectrum.imag[k] = spectrum.imag[k] * 4.0;
    }
    let mut y = fine.inverse(&up_spectrum);
    let a2 = 2.0 * d2 / amplitude;
    let a3 = 4.0 * d3 / (amplitude * amplitude);
    for v in &mut y {
        let s = *v;
        *v = s + a2 * s * s + a3 * s * s * s - 3.0 * d3 * s;
    }
    let filtered = fine.forward(&y);
    let mut down_spectrum = HalfSpectrum::new(n);
    for k in 0..down_spectrum.bin_count() {
        down_spectrum.real[k] = filtered.real[k] * 0.25;
        down_spectrum.imag[k] = filtered.imag[k] * 0.25;
    }
    let mut out = coarse.inverse(&down_spectrum);
    out.truncate(x.len());
    out
}

/// Pink noise (power ∝ 1/f) with a given RMS, from a seeded generator.
pub(crate) fn pink_noise(count: usize, target_rms: f64, seed: u64) -> Vec<f64> {
    if !(count > 0 && target_rms > 0.0) {
        return vec![0.0; count];
    }
    let n = next_power_of_two(count).max(4);
    let mut spectrum = HalfSpectrum::new(n);
    let mut rng = SplitMix64::new(seed);
    for k in 1..spectrum.bin_count() {
        let magnitude = 1.0 / (k as f64).sqrt();
        let phase = 2.0 * PI * rng.next_unit();
        spectrum.real[k] = magnitude * phase.cos();
        spectrum.imag[k] = magnitude * phase.sin();
    }
    let mut x = RealFft::new(n).inverse(&spectrum);
    let current = rms(&x);
    if current > 0.0 {
        let scale = target_rms / current;
        for v in &mut x {
            *v *= scale;
        }
    }
    x.truncate(count);
    x
}

/// Resamples by a very small ratio with a 32-tap Blackman-windowed sinc: clock drift.
pub(crate) fn resample(x: &[f64], ratio: f64) -> Vec<f64> {
    let taps = 32i64;
    let half = taps / 2;
    let out_count = (x.len() as f64 / ratio) as usize;
    let mut out = vec![0.0; out_count];
    for (n, slot) in out.iter_mut().enumerate() {
        let position = n as f64 * ratio;
        let base = position.floor() as i64;
        let fraction = position - base as f64;
        let mut sum = 0.0;
        for t in (-half + 1)..=half {
            let index = base + t;
            if index < 0 || index >= x.len() as i64 {
                continue;
            }
            let d = t as f64 - fraction;
            let sinc = if d.abs() < 1e-12 {
                1.0
            } else {
                (PI * d).sin() / (PI * d)
            };
            let u = ((t + half) as f64 + 0.5 - fraction) / taps as f64;
            let w = 0.42 - 0.5 * (2.0 * PI * u).cos() + 0.08 * (4.0 * PI * u).cos();
            sum += x[index as usize] * sinc * w;
        }
        *slot = sum;
    }
    out
}

/// Multiplies a buffer by a magnitude curve with zero phase.
pub(crate) fn apply_zero_phase_curve(x: &[f64], curve: HeadphoneCurve, sample_rate: f64) -> Vec<f64> {
    let n = next_power_of_two(x.len());
    let fft = RealFft::new(n);
    let mut spectrum = fft.forward(x);
    let interpolator = CurveInterpolator::new(curve);
    let bin_hz = sample_rate / n as f64;
    for k in 0..spectrum.bin_count() {
        let f = (k as f64 * bin_hz).max(1e-6);
        let gain = 10.0f64.powf(interpolator.level(f as f32) as f64 / 20.0);
        spectrum.real[k] *= gain;
        spectrum.imag[k] *= gain;
    }
    let mut out = fft.inverse(&spectrum);
    out.truncate(x.len());
    out
}

// MARK: Assertions

/// Largest absolute difference between two curves over a frequency range, and where: `(dB, hz)`.
pub(crate) fn worst_difference(a: &[f64], b: &[f64], grid: &[f64], low_hz: f64, high_hz: f64) -> (f64, f64) {
    let (mut worst, mut at) = (0.0, 0.0);
    for (i, &f) in grid.iter().enumerate() {
        if f >= low_hz && f <= high_hz {
            let d = (a[i] - b[i]).abs();
            if d > worst {
                (worst, at) = (d, f);
            }
        }
    }
    (worst, at)
}

/// Mean of a curve over a frequency range.
pub(crate) fn mean(values: &[f64], grid: &[f64], low_hz: f64, high_hz: f64) -> f64 {
    let (mut sum, mut used) = (0.0, 0usize);
    for (i, &f) in grid.iter().enumerate() {
        if f >= low_hz && f <= high_hz {
            sum += values[i];
            used += 1;
        }
    }
    if used > 0 {
        sum / used as f64
    } else {
        0.0
    }
}

/// Shifts a curve so that its mean over 800–1250 Hz is 0 dB.
pub(crate) fn normalized_to_1khz(values: &[f64], grid: &[f64]) -> Vec<f64> {
    let reference = mean(values, grid, 800.0, 1250.0);
    values.iter().map(|v| v - reference).collect()
}

/// Prints the number, then asserts on it. The limits of a test can be read off the log.
#[track_caller]
pub(crate) fn expect(value: f64, at_most: f64, label: &str) {
    println!("  {label}: {value:.3} (limit {at_most:.3})");
    assert!(value <= at_most, "{label}: {value} is over {at_most}");
}

#[track_caller]
pub(crate) fn near(actual: f64, expected: f64, accuracy: f64) {
    assert!(
        (actual - expected).abs() <= accuracy,
        "{actual}, expected {expected} ± {accuracy}"
    );
}

pub(crate) fn to_f64(values: &[f32]) -> Vec<f64> {
    values.iter().map(|&v| v as f64).collect()
}
