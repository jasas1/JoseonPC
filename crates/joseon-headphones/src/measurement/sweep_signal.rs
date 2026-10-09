//! A measurement stimulus: the samples to play, and what the analysis must know
//! about how they were made.
//!
//! Port source: `Sources/JoseonHeadphones/Measurement/SweepSignal.swift`.
//!
//! Nothing here plays anything. `samples` is a vector. To make sound is the job
//! of the app, after the user presses a button that says it will.

use super::fft::{amplitude_db, next_power_of_two, rms, HalfSpectrum, RealFft, SplitMix64};
use std::f64::consts::PI;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SweepKind {
    /// Farina's exponential sine sweep. Every octave gets the same energy, and
    /// the harmonic distortion products land at *earlier* times in the
    /// deconvolved impulse response, where a window removes them. Use this one.
    ExponentialSweep,
    /// One exact period of pink noise. It can loop with no alignment, but the
    /// crest factor costs about 9 dB of signal-to-noise against a sweep at the
    /// same peak level, and distortion cannot be separated.
    PeriodicPinkNoise,
}

impl SweepKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ExponentialSweep => "exponentialSweep",
            Self::PeriodicPinkNoise => "periodicPinkNoise",
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct SweepSignal {
    pub kind: SweepKind,
    pub sample_rate: f64,
    /// First frequency of the sweep, or the low edge of the noise band.
    pub start_hz: f64,
    /// Last frequency of the sweep, or the high edge of the noise band. Always under Nyquist.
    pub end_hz: f64,
    pub duration_seconds: f64,
    /// Peak level of the stimulus in dBFS (a 0 dBFS signal touches ±1.0).
    pub level_dbfs: f64,
    pub samples: Vec<f64>,
    /// Length of the raised-cosine fade at the start, in seconds. 0 for pink noise.
    pub fade_in_seconds: f64,
    /// Length of the raised-cosine fade at the end, in seconds. 0 for pink noise.
    ///
    /// On an exponential sweep the fade-out is on the **top** frequencies. See
    /// `highest_full_energy_hz`.
    pub fade_out_seconds: f64,
}

/// The arguments of `SweepSignal::exponential_sweep`, with the defaults of the macOS app.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SweepOptions {
    pub start_hz: f64,
    pub end_hz: f64,
    pub seconds: f64,
    /// Peak level. −6 dBFS leaves headroom for the interface.
    pub level_dbfs: f64,
    pub fade_in_seconds: f64,
    pub fade_out_seconds: f64,
}

impl Default for SweepOptions {
    fn default() -> Self {
        Self {
            start_hz: 20.0,
            end_hz: 20_000.0,
            seconds: 5.0,
            level_dbfs: -6.0,
            fade_in_seconds: 0.010,
            fade_out_seconds: 0.020,
        }
    }
}

/// The arguments of `SweepSignal::periodic_pink_noise`, with the defaults of the macOS app.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PinkNoiseOptions {
    pub seconds: f64,
    pub level_dbfs: f64,
    pub start_hz: f64,
    pub end_hz: f64,
    pub seed: u64,
}

impl Default for PinkNoiseOptions {
    fn default() -> Self {
        Self {
            seconds: 4.0,
            level_dbfs: -6.0,
            start_hz: 20.0,
            end_hz: 20_000.0,
            seed: 0x5EED_10AD,
        }
    }
}

impl SweepSignal {
    /// Shortest and longest sweep the factory makes. Under 2 s the bass
    /// signal-to-noise ratio is poor. Over 10 s the room and the clocks drift
    /// more than the sweep gains.
    pub const DURATION_RANGE_SECONDS: (f64, f64) = (2.0, 10.0);

    // MARK: Where the stimulus stops

    /// The highest frequency this stimulus drove at **full** level, under the Nyquist clamp.
    ///
    /// The instantaneous frequency of an exponential sweep is `f₁·e^(t/L)`, so
    /// the fade-out covers the top of the band:
    /// `highest_full_energy_hz = f₂ · e^(−fade_out / L)`. A 5 s 20 Hz–20 kHz
    /// sweep with a 20 ms fade-out stops at 19.45 kHz. Above that the
    /// deconvolution divides by a spectrum the fade took away.
    pub fn highest_full_energy_hz(&self) -> f64 {
        let nyquist = self.sample_rate / 2.0;
        match self.kind {
            SweepKind::ExponentialSweep => {
                let rate = self.sweep_rate_seconds();
                if !(rate > 0.0 && self.fade_out_seconds > 0.0) {
                    return self.end_hz.min(nyquist);
                }
                (self.end_hz * (-self.fade_out_seconds / rate).exp())
                    .min(self.end_hz)
                    .min(nyquist)
            }
            SweepKind::PeriodicPinkNoise => self.end_hz.min(nyquist),
        }
    }

    // MARK: Exponential sine sweep

    /// Farina exponential sine sweep with raised-cosine fades.
    ///
    /// `x(t) = sin( ω₁·L·( e^(t/L) − 1 ) )`, `L = T / ln(ω₂/ω₁)`
    ///
    /// `end_hz` is clamped to 95 % of Nyquist and `seconds` to
    /// `DURATION_RANGE_SECONDS`. The fades do not bias the result: the analysis
    /// divides by the spectrum of *these* samples, fades included.
    pub fn exponential_sweep(sample_rate: f64, options: SweepOptions) -> SweepSignal {
        let f1 = options.start_hz.min(sample_rate * 0.4).max(1.0);
        let f2 = options.end_hz.min(sample_rate * 0.5 * 0.95).max(f1 * 1.5);
        let (shortest, longest) = Self::DURATION_RANGE_SECONDS;
        let duration = options.seconds.max(shortest).min(longest);
        let count = ((duration * sample_rate).round() as usize).max(16);
        let amplitude = 10.0f64.powf(options.level_dbfs / 20.0);

        let l = duration / (f2 / f1).ln(); // seconds per neper of frequency
        let k = 2.0 * PI * f1 * l;
        let mut x = vec![0.0; count];
        for (i, slot) in x.iter_mut().enumerate() {
            let t = i as f64 / sample_rate;
            *slot = amplitude * (k * ((t / l).exp() - 1.0)).sin();
        }
        apply_fades(&mut x, sample_rate, options.fade_in_seconds, options.fade_out_seconds);

        SweepSignal {
            kind: SweepKind::ExponentialSweep,
            sample_rate,
            start_hz: f1,
            end_hz: f2,
            duration_seconds: count as f64 / sample_rate,
            level_dbfs: options.level_dbfs,
            samples: x,
            fade_in_seconds: options.fade_in_seconds.max(0.0),
            fade_out_seconds: options.fade_out_seconds.max(0.0),
        }
    }

    /// Seconds of sweep per neper of frequency: `T / ln(f₂/f₁)`.
    pub fn sweep_rate_seconds(&self) -> f64 {
        if !(self.kind == SweepKind::ExponentialSweep && self.end_hz > self.start_hz && self.start_hz > 0.0) {
            return 0.0;
        }
        self.duration_seconds / (self.end_hz / self.start_hz).ln()
    }

    /// How far **before** the linear impulse response the `n`-th harmonic lands, in seconds:
    /// `Δt(n) = L · ln(n)`.
    pub fn harmonic_time_offset_seconds(&self, n: usize) -> f64 {
        if n < 2 {
            return 0.0;
        }
        self.sweep_rate_seconds() * (n as f64).ln()
    }

    /// Fraction of the octaves of the sweep whose `n`-th harmonic still fits under `end_hz`.
    pub fn harmonic_band_coverage(&self, n: usize) -> f64 {
        if !(n >= 2 && self.end_hz > self.start_hz && self.start_hz > 0.0) {
            return 1.0;
        }
        let usable = (self.end_hz / (n as f64 * self.start_hz)).ln();
        if !(usable > 0.0) {
            return 0.0;
        }
        (usable / (self.end_hz / self.start_hz).ln()).min(1.0)
    }

    /// Farina's inverse filter: the sweep reversed in time, with a −6 dB/octave
    /// envelope, scaled so that its convolution with the sweep has unit gain
    /// through the swept band.
    ///
    /// The analysis does **not** use this. It is here because the classic recipe
    /// is the one people check a rig against. `None` for pink noise.
    pub fn inverse_filter(&self) -> Option<Vec<f64>> {
        if self.kind != SweepKind::ExponentialSweep {
            return None;
        }
        let n = self.samples.len();
        let l = self.sweep_rate_seconds();
        if !(n > 0 && l > 0.0) {
            return None;
        }
        let mut f: Vec<f64> = (0..n)
            .map(|j| self.samples[n - 1 - j] * (-(j as f64) / (self.sample_rate * l)).exp())
            .collect();

        // Scale for unit magnitude through the band, numerically: this accounts
        // for the fades and the clamped end frequency.
        let length = next_power_of_two(2 * n);
        let fft = RealFft::new(length);
        let x = fft.forward(&self.samples);
        let f_spectrum = fft.forward(&f);
        let lo = (self.start_hz * 2.0f64.powf(1.0 / 3.0) * length as f64 / self.sample_rate).round() as usize;
        let hi = (self.end_hz * 2.0f64.powf(-1.0 / 3.0) * length as f64 / self.sample_rate).round() as usize;
        let (mut sum, mut used) = (0.0, 0usize);
        if hi > lo {
            for k in lo..=hi.min(x.bin_count() - 1) {
                let re = x.real[k] * f_spectrum.real[k] - x.imag[k] * f_spectrum.imag[k];
                let im = x.real[k] * f_spectrum.imag[k] + x.imag[k] * f_spectrum.real[k];
                sum += (re * re + im * im).sqrt();
                used += 1;
            }
        }
        if !(used > 0 && sum > 0.0) {
            return Some(f);
        }
        let scale = used as f64 / sum;
        for v in &mut f {
            *v *= scale;
        }
        Some(f)
    }

    // MARK: Periodic pink noise

    /// One exact period of pink noise, band limited to `start_hz ... end_hz`.
    ///
    /// The period is rounded **up to a power of two samples**, so the app can
    /// loop it and the analysis can average periods with no alignment.
    /// Made in the frequency domain: magnitude `1/√f` in band, raised-cosine
    /// edges over a third of an octave, phases from a seeded generator.
    pub fn periodic_pink_noise(sample_rate: f64, options: PinkNoiseOptions) -> SweepSignal {
        let f1 = options.start_hz.min(sample_rate * 0.4).max(1.0);
        let f2 = options.end_hz.min(sample_rate * 0.5 * 0.95).max(f1 * 1.5);
        let (shortest, longest) = Self::DURATION_RANGE_SECONDS;
        let duration = options.seconds.max(shortest).min(longest);
        let n = next_power_of_two((duration * sample_rate).round() as usize);

        let mut spectrum = HalfSpectrum::new(n);
        let mut rng = SplitMix64::new(options.seed);
        let df = sample_rate / n as f64;
        let edge = 2.0f64.powf(1.0 / 3.0);
        let ramp = |u: f64| {
            if u <= 0.0 {
                0.0
            } else {
                0.5 * (1.0 - (PI * u.min(1.0)).cos())
            }
        };
        for k in 1..spectrum.bin_count() {
            let f = k as f64 * df;
            let mut magnitude = 1.0 / f.sqrt(); // pink: power ∝ 1/f
            if f < f1 {
                magnitude *= ramp((f / (f1 / edge)).ln() / edge.ln());
            }
            if f > f2 {
                magnitude *= ramp(((f2 * edge) / f).ln() / edge.ln());
            }
            let phase = 2.0 * PI * rng.next_unit();
            spectrum.real[k] = magnitude * phase.cos();
            spectrum.imag[k] = magnitude * phase.sin();
        }
        let last = spectrum.bin_count() - 1;
        spectrum.imag[last] = 0.0; // Nyquist must be real

        let mut x = RealFft::new(n).inverse(&spectrum);
        let peak = x.iter().fold(0.0f64, |m, v| m.max(v.abs()));
        let amplitude = 10.0f64.powf(options.level_dbfs / 20.0);
        if peak > 0.0 {
            let scale = amplitude / peak;
            for v in &mut x {
                *v *= scale;
            }
        }

        SweepSignal {
            kind: SweepKind::PeriodicPinkNoise,
            sample_rate,
            start_hz: f1,
            end_hz: f2,
            duration_seconds: n as f64 / sample_rate,
            level_dbfs: options.level_dbfs,
            samples: x,
            fade_in_seconds: 0.0,
            fade_out_seconds: 0.0,
        }
    }

    /// Samples in one period. Only meaningful for `PeriodicPinkNoise`.
    pub fn period_samples(&self) -> usize {
        self.samples.len()
    }

    /// RMS of the stimulus in dBFS, where a full-scale sine reads −3.01.
    ///
    /// The gap between this and `level_dbfs` is the crest factor.
    pub fn rms_level_dbfs(&self) -> f64 {
        amplitude_db(rms(&self.samples))
    }
}

/// Raised-cosine fade in and out, in place.
fn apply_fades(x: &mut [f64], sample_rate: f64, fade_in: f64, fade_out: f64) {
    let n = x.len();
    let in_count = ((fade_in * sample_rate) as i64).max(0).min((n / 2) as i64) as usize;
    let out_count = ((fade_out * sample_rate) as i64).max(0).min((n / 2) as i64) as usize;
    if in_count > 1 {
        for i in 0..in_count {
            x[i] *= 0.5 * (1.0 - (PI * i as f64 / (in_count - 1) as f64).cos());
        }
    }
    if out_count > 1 {
        for j in 0..out_count {
            x[n - 1 - j] *= 0.5 * (1.0 - (PI * j as f64 / (out_count - 1) as f64).cos());
        }
    }
}
