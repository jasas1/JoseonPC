//! Inter-sample (true) peak detector.
//!
//! Port source: `Sources/JoseonCore/Loudness/TruePeakDetector.swift`.
//!
//! The signal is oversampled with a polyphase FIR interpolator, and the largest
//! absolute value on the fine grid is the true peak. BS.1770-4 Annex 2 prints a
//! 48-tap 4x filter. This one is a 64-tap windowed-sinc design (Kaiser, beta
//! 9), which is longer and flatter.
//!
//! The prototype is centred on an integer tap, so phase 0 is exactly a unit
//! impulse. Two useful results: the reported true peak can never fall below the
//! sample peak, and that phase needs no multiply.
//!
//! Measured against ideal sine reconstruction: never over-reads. The worst
//! under-read is the 4x grid limit itself, about −0.44 dB for a full-scale
//! sine at 0.8 x Nyquist.

/// Taps per polyphase branch. Must be even (phase 0 needs a centre tap).
pub(crate) const TAPS_PER_PHASE: usize = 16;
const KAISER_BETA: f64 = 9.0;
/// Largest oversampling factor the coefficient store is sized for.
const MAX_FACTOR: usize = 4;

/// The oversampling factor the app uses at `sample_rate`. 4x up to the 96 kHz
/// family. High rates already resolve inter-sample peaks well, so 176.4 kHz and
/// up drop to 2x, and 352.8 kHz and up to 1x.
pub(crate) fn factor_for_sample_rate(sample_rate: f64) -> usize {
    if sample_rate >= 352_800.0 {
        1
    } else if sample_rate >= 176_400.0 {
        2
    } else {
        4
    }
}

#[derive(Debug, Clone)]
pub(crate) struct TruePeakDetector {
    factor: usize,
    /// Reversed coefficients for phases 1 ..< factor. Phase 0 is the impulse.
    coefficients: [[f32; TAPS_PER_PHASE]; MAX_FACTOR - 1],
    /// Doubled delay line, so each branch reads one contiguous window.
    history: [f32; 2 * TAPS_PER_PHASE],
    cursor: usize,
}

impl TruePeakDetector {
    pub fn new() -> Self {
        let mut detector = Self {
            factor: 0,
            coefficients: [[0.0; TAPS_PER_PHASE]; MAX_FACTOR - 1],
            history: [0.0; 2 * TAPS_PER_PHASE],
            cursor: 0,
        };
        detector.configure(4);
        detector
    }

    /// Rebuilds the interpolator. No allocation: the store is sized for `MAX_FACTOR`.
    pub fn configure(&mut self, factor: usize) {
        let factor = factor.clamp(1, MAX_FACTOR);
        if factor != self.factor {
            self.factor = factor;
            self.build_coefficients();
        }
        self.clear();
    }

    pub fn clear(&mut self) {
        self.history = [0.0; 2 * TAPS_PER_PHASE];
        self.cursor = 0;
    }

    /// Feeds one sample. Returns the largest absolute value on the oversampled grid.
    #[inline(always)]
    pub fn push(&mut self, x: f32) -> f32 {
        const TAPS: usize = TAPS_PER_PHASE;
        self.history[self.cursor] = x;
        self.history[self.cursor + TAPS] = x;
        // After the write the window oldest..newest is history[cursor+1 ..= cursor+TAPS].
        let window: &[f32; TAPS] = self.history[self.cursor + 1..self.cursor + 1 + TAPS]
            .try_into()
            .expect("the window is TAPS long");
        // Phase 0 is a unit impulse at tap TAPS/2, so it is the delayed sample.
        let mut peak = window[TAPS / 2 - 1].abs();
        for branch in &self.coefficients[..self.factor - 1] {
            // Summed in tap order, one add at a time, as the macOS app does it.
            let mut acc = 0.0f32;
            for j in 0..TAPS {
                acc += branch[j] * window[j];
            }
            let magnitude = acc.abs();
            if magnitude > peak {
                peak = magnitude;
            }
        }
        self.cursor += 1;
        if self.cursor == TAPS {
            self.cursor = 0;
        }
        peak
    }

    /// Largest oversampled magnitude over the samples.
    pub fn max_abs(&mut self, x: &[f32]) -> f32 {
        let mut peak = 0.0f32;
        for &sample in x {
            let value = self.push(sample);
            if value > peak {
                peak = value;
            }
        }
        peak
    }

    // MARK: Filter design

    fn build_coefficients(&mut self) {
        let factor = self.factor;
        if factor <= 1 {
            return;
        }
        let n = factor * TAPS_PER_PHASE;
        let centre = (n / 2) as f64;
        let i0_beta = bessel_i0(KAISER_BETA);
        let prototype: Vec<f64> = (0..n)
            .map(|m| {
                // Kaiser window of length n+1 sampled on 0 ..< n, so its peak sits on `centre`.
                let t = 2.0 * m as f64 / n as f64 - 1.0;
                let w = bessel_i0(KAISER_BETA * (1.0 - t * t).max(0.0).sqrt()) / i0_beta;
                sinc((m as f64 - centre) / factor as f64) * w
            })
            .collect();
        // Phase p takes every factor-th tap. Each branch is normalised to unity
        // DC gain and stored reversed, so the hot loop is a straight dot product.
        for p in 1..factor {
            let branch: Vec<f64> = prototype[p..].iter().step_by(factor).copied().collect();
            let mut sum = 0.0;
            for &tap in &branch {
                sum += tap;
            }
            let gain = if sum == 0.0 { 1.0 } else { sum };
            for j in 0..TAPS_PER_PHASE {
                self.coefficients[p - 1][j] = (branch[TAPS_PER_PHASE - 1 - j] / gain) as f32;
            }
        }
    }

    /// Phase `p` of the current design in convolution order: element k
    /// multiplies x[n - k]. For tests.
    #[cfg(test)]
    pub fn phase_coefficients(&self, p: usize) -> [f32; TAPS_PER_PHASE] {
        let mut out = [0.0; TAPS_PER_PHASE];
        if p == 0 || p >= self.factor {
            out[TAPS_PER_PHASE / 2] = 1.0;
        } else {
            for (k, tap) in out.iter_mut().enumerate() {
                *tap = self.coefficients[p - 1][TAPS_PER_PHASE - 1 - k];
            }
        }
        out
    }
}

fn sinc(x: f64) -> f64 {
    if x == 0.0 {
        return 1.0;
    }
    let p = std::f64::consts::PI * x;
    p.sin() / p
}

fn bessel_i0(x: f64) -> f64 {
    let (mut sum, mut term) = (1.0, 1.0);
    let half = x / 2.0;
    let mut k = 1.0;
    while k < 200.0 {
        term *= (half / k) * (half / k);
        sum += term;
        if term < sum * 1e-17 {
            break;
        }
        k += 1.0;
    }
    sum
}
