//! Small numeric helpers shared by the measurement code.
//!
//! Port source: `Sources/JoseonHeadphones/Measurement/MeasurementFFT.swift`.
//! Everything here is private to the crate: the public surface of the
//! measurement code is signals, curves and results.

use realfft::num_complex::Complex;
use realfft::{ComplexToReal, RealFftPlanner, RealToComplex};
use std::sync::Arc;

/// Smallest power of two that is at least `n`.
pub(crate) fn next_power_of_two(n: usize) -> usize {
    n.max(1).next_power_of_two()
}

/// One-sided spectrum of a real signal of length `n`: bins `0 ... n/2`, as the
/// plain mathematical DFT values.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct HalfSpectrum {
    pub real: Vec<f64>,
    pub imag: Vec<f64>,
    pub n: usize,
}

impl HalfSpectrum {
    pub fn new(n: usize) -> Self {
        Self {
            real: vec![0.0; n / 2 + 1],
            imag: vec![0.0; n / 2 + 1],
            n,
        }
    }

    pub fn bin_count(&self) -> usize {
        self.n / 2 + 1
    }

    /// Hz of bin `k` at `sample_rate`.
    #[cfg(test)]
    pub fn frequency(&self, k: usize, sample_rate: f64) -> f64 {
        k as f64 * sample_rate / self.n as f64
    }

    pub fn power(&self, k: usize) -> f64 {
        self.real[k] * self.real[k] + self.imag[k] * self.imag[k]
    }

    #[cfg(test)]
    pub fn magnitude(&self, k: usize) -> f64 {
        self.power(k).sqrt()
    }
}

/// Real FFT in `f64`.
///
/// `f64`, not `f32`: a 5 s sweep is a quarter of a million samples, and the
/// deconvolution divides by a spectrum that spans 60 dB. The 24-bit mantissa of
/// `f32` would show in the bass.
pub(crate) struct RealFft {
    pub n: usize,
    forward: Arc<dyn RealToComplex<f64>>,
    inverse: Arc<dyn ComplexToReal<f64>>,
}

impl RealFft {
    pub fn new(n: usize) -> Self {
        assert!(
            n >= 4 && n.is_power_of_two(),
            "FFT length must be a power of two, got {n}"
        );
        let mut planner = RealFftPlanner::<f64>::new();
        Self {
            n,
            forward: planner.plan_fft_forward(n),
            inverse: planner.plan_fft_inverse(n),
        }
    }

    /// Forward transform of `x`, zero padded (or truncated) to `n`.
    pub fn forward(&self, x: &[f64]) -> HalfSpectrum {
        let mut padded = vec![0.0; self.n];
        let used = x.len().min(self.n);
        padded[..used].copy_from_slice(&x[..used]);
        let mut bins = self.forward.make_output_vec();
        self.forward
            .process(&mut padded, &mut bins)
            .expect("the buffers have the length of the plan");
        let mut out = HalfSpectrum::new(self.n);
        for (k, bin) in bins.iter().enumerate() {
            out.real[k] = bin.re;
            out.imag[k] = bin.im;
        }
        // The imaginary part of DC and Nyquist is zero for a real signal.
        out.imag[0] = 0.0;
        out.imag[self.n / 2] = 0.0;
        out
    }

    /// Inverse transform back to a real signal of length `n`.
    pub fn inverse(&self, s: &HalfSpectrum) -> Vec<f64> {
        assert_eq!(s.n, self.n, "spectrum length does not match this FFT");
        let half = self.n / 2;
        let mut bins: Vec<Complex<f64>> = (0..=half).map(|k| Complex::new(s.real[k], s.imag[k])).collect();
        bins[0].im = 0.0;
        bins[half].im = 0.0;
        let mut out = vec![0.0; self.n];
        self.inverse
            .process(&mut bins, &mut out)
            .expect("the buffers have the length of the plan");
        let scale = 1.0 / self.n as f64;
        for v in &mut out {
            *v *= scale;
        }
        out
    }
}

// MARK: Small array math

/// Root mean square. Zero for an empty slice.
pub(crate) fn rms(x: &[f64]) -> f64 {
    if x.is_empty() {
        return 0.0;
    }
    (x.iter().map(|v| v * v).sum::<f64>() / x.len() as f64).sqrt()
}

/// dB of an amplitude ratio, with a floor of −200 so a silent band prints a number.
pub(crate) fn amplitude_db(amplitude: f64) -> f64 {
    let a = amplitude.abs();
    if !(a > 0.0) {
        return -200.0;
    }
    (20.0 * a.log10()).max(-200.0)
}

/// dB of a power ratio, with the same floor.
pub(crate) fn power_db(power: f64) -> f64 {
    if !(power > 0.0) {
        return -200.0;
    }
    (10.0 * power.log10()).max(-200.0)
}

/// Index of the largest `|x|`, or 0 for an empty slice.
pub(crate) fn peak_index(x: &[f64]) -> usize {
    let (mut best, mut best_value) = (0, 0.0);
    for (i, v) in x.iter().enumerate() {
        let a = v.abs();
        if a > best_value {
            (best, best_value) = (i, a);
        }
    }
    best
}

/// Sub-sample peak position around `i`, from a parabola through `|x|` at `i-1, i, i+1`.
///
/// Gives `i` when the neighbours are missing or the fit is degenerate.
pub(crate) fn parabolic_peak_offset(x: &[f64], i: usize) -> f64 {
    if i == 0 || i + 1 >= x.len() {
        return i as f64;
    }
    let (a, b, c) = (x[i - 1].abs(), x[i].abs(), x[i + 1].abs());
    let denominator = a - 2.0 * b + c;
    if denominator == 0.0 {
        return i as f64;
    }
    let delta = 0.5 * (a - c) / denominator;
    if !(delta.is_finite() && delta.abs() <= 1.0) {
        return i as f64;
    }
    i as f64 + delta
}

/// Index into a cyclic buffer of length `n`.
pub(crate) fn cyclic_index(i: i64, n: usize) -> usize {
    i.rem_euclid(n as i64) as usize
}

/// `printf("%.*f")` as the macOS app prints numbers into its sentences.
pub(crate) fn fixed(value: f64, decimals: usize) -> String {
    if value.is_nan() {
        return "nan".to_string();
    }
    format!("{value:.decimals$}")
}

/// Seeded generator, so "random" phases are the same in every run.
pub(crate) struct SplitMix64 {
    state: u64,
}

impl SplitMix64 {
    pub fn new(seed: u64) -> Self {
        Self { state: seed }
    }

    pub fn next(&mut self) -> u64 {
        self.state = self.state.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.state;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    /// Uniform in `[0, 1)`.
    pub fn next_unit(&mut self) -> f64 {
        (self.next() >> 11) as f64 * (1.0 / 9_007_199_254_740_992.0)
    }

    /// Standard normal, Box–Muller.
    #[cfg(test)]
    pub fn next_gaussian(&mut self) -> f64 {
        let u1 = self.next_unit().max(1e-12);
        let u2 = self.next_unit();
        (-2.0 * u1.ln()).sqrt() * (2.0 * std::f64::consts::PI * u2).cos()
    }
}
