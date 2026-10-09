//! From a recording of a sweep to a frequency response.
//!
//! Port source: `Sources/JoseonHeadphones/Measurement/SweepAnalysis.swift`.
//!
//! The chain, in order:
//!
//! 1. **Deconvolution.** `H = R·conj(S) / (|S|² + ε)`. The division by the
//!    spectrum of the sweep that was played removes the fades and the band
//!    edges. ε is Kirkeby regularization: −50 dB under the mean sweep power in
//!    the swept band, 0 dB outside it, with a third-octave raised-cosine ramp.
//! 2. **Alignment.** The peak of that impulse response is the round-trip delay.
//! 3. **Window.** Half-Hann in, half-Hann out. The leading edge is short, so
//!    the harmonic impulse responses (which are `L·ln(n)` seconds *before* the
//!    linear one) stay outside. The trailing edge is adaptive.
//! 4. **Magnitude** on the 200-point log grid, with fractional-octave smoothing.
//!
//! Everything is a function over slices. No I/O, no state, no sound.

use super::fft::{
    amplitude_db, cyclic_index, next_power_of_two, parabolic_peak_offset, peak_index, power_db, rms, HalfSpectrum,
    RealFft,
};
use super::sweep_signal::{SweepKind, SweepSignal};
use crate::embedded_curves::STANDARD_FREQUENCIES_HZ;
use std::collections::BTreeMap;
use std::f64::consts::PI;

// MARK: Options

/// How much smoothing to apply to the magnitude response.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum OctaveSmoothing {
    None,
    #[default]
    Twelfth,
    Sixth,
    Third,
}

impl OctaveSmoothing {
    pub const ALL: [OctaveSmoothing; 4] = [Self::None, Self::Twelfth, Self::Sixth, Self::Third];

    /// The `b` in "1/b octave", or `None` for no smoothing.
    pub fn denominator(self) -> Option<f64> {
        match self {
            Self::None => None,
            Self::Twelfth => Some(12.0),
            Self::Sixth => Some(6.0),
            Self::Third => Some(3.0),
        }
    }

    /// The name in the method sentence. Same strings as the macOS app.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Twelfth => "1/12 octave",
            Self::Sixth => "1/6 octave",
            Self::Third => "1/3 octave",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Options {
    /// Regularization inside the swept band, in dB under the mean sweep power.
    pub in_band_regularization_db: f64,
    /// Regularization outside the swept band, in dB relative to the mean sweep
    /// power. 0 dB means "where the sweep had no energy, return nothing".
    pub out_of_band_regularization_db: f64,
    /// Width of the raised-cosine ramp between the two, in octaves, centred on each band edge.
    pub regularization_ramp_octaves: f64,
    /// Window length before the impulse peak. Never more than half the distance
    /// back to the 2nd-harmonic impulse response.
    pub pre_window_seconds: f64,
    /// Shortest trailing window the adaptive rule may choose. 100 ms holds the
    /// ring of a 30 Hz high-pass.
    pub minimum_post_window_seconds: f64,
    /// Longest trailing window.
    pub maximum_post_window_seconds: f64,
    /// Fraction of the trailing window spent in the half-Hann fade-out.
    pub post_window_fade_fraction: f64,
    /// How far above the measured noise floor the envelope must fall before the
    /// trailing window may close.
    pub decay_stop_margin_db: f64,
    /// Default smoothing for `magnitude_response`.
    pub smoothing: OctaveSmoothing,
    /// FFT length used for the windowed response. 65536 gives 0.73 Hz bins at 48 kHz.
    pub response_fft_size: usize,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            in_band_regularization_db: -50.0,
            out_of_band_regularization_db: 0.0,
            regularization_ramp_octaves: 1.0 / 3.0,
            pre_window_seconds: 0.005,
            minimum_post_window_seconds: 0.100,
            maximum_post_window_seconds: 0.300,
            post_window_fade_fraction: 0.25,
            decay_stop_margin_db: 3.0,
            smoothing: OctaveSmoothing::Twelfth,
            response_fft_size: 65_536,
        }
    }
}

// MARK: Results

/// A deconvolved impulse response, before the window.
#[derive(Debug, Clone, PartialEq)]
pub struct ImpulseResponse {
    /// The cyclic impulse response, one FFT length long. Harmonic distortion
    /// products are before `peak_index` (they wrap round the end of the buffer
    /// for a short recording).
    pub samples: Vec<f64>,
    pub sample_rate: f64,
    /// Sample of the linear impulse peak: the play-to-record delay.
    pub peak_index: usize,
    /// The same peak to a fraction of a sample, from a parabolic fit.
    pub peak_sub_sample: f64,
    /// Noise floor in dB relative to the peak, measured just before the peak.
    pub noise_floor_db_relative_peak: f64,
    /// Where the n-th harmonic impulse response is, in samples before `peak_index`.
    pub harmonic_offset_samples: BTreeMap<usize, usize>,
}

impl ImpulseResponse {
    pub fn delay_seconds(&self) -> f64 {
        self.peak_index as f64 / self.sample_rate
    }
}

/// A windowed impulse response, ready to transform.
#[derive(Debug, Clone, PartialEq)]
pub struct WindowedImpulseResponse {
    pub samples: Vec<f64>,
    pub sample_rate: f64,
    /// Index of the impulse peak inside `samples` (equals `pre_samples`).
    pub peak_offset: usize,
    pub pre_samples: usize,
    pub post_samples: usize,
}

impl WindowedImpulseResponse {
    /// Lowest frequency the window can resolve, `1 / window seconds`. Below it
    /// the curve is the shape of the window, not of the headphone.
    pub fn lowest_resolved_hz(&self) -> f64 {
        self.sample_rate / self.samples.len().max(1) as f64
    }
}

/// A complex frequency response on a linear FFT grid. This is what gets averaged across runs.
#[derive(Debug, Clone, PartialEq)]
pub struct ComplexResponse {
    pub(crate) spectrum: HalfSpectrum,
    pub sample_rate: f64,
}

impl ComplexResponse {
    pub fn fft_size(&self) -> usize {
        self.spectrum.n
    }

    pub fn magnitude_db(&self, grid: &[f64], smoothing: OctaveSmoothing) -> Vec<f64> {
        smoothed_magnitude_db(&self.spectrum, self.sample_rate, grid, smoothing)
    }
}

/// Harmonic distortion read off the impulse responses the sweep separated in time.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct HarmonicDistortion {
    /// √(ΣEₙ / E₁) over the harmonics measured, as a percentage.
    pub total_percent: f64,
    /// Per harmonic, `2` and `3`, as a percentage.
    pub per_harmonic_percent: BTreeMap<usize, f64>,
    /// How much of the band of the sweep each harmonic could be measured over
    /// (1.0 = all of it). The percentages are already divided by this.
    pub band_coverage: BTreeMap<usize, f64>,
}

/// The result of `delay_samples`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Delay {
    pub samples: usize,
    pub sub_sample: f64,
    /// 0...1: the peak divided by the RMS of the whole correlation, scaled so a
    /// clean measurement reads near 1 and pure noise reads near 0.
    pub confidence: f64,
}

/// The result of `average`.
#[derive(Debug, Clone, PartialEq)]
pub struct Average {
    pub mean: ComplexResponse,
    /// Per grid point, the standard deviation of the runs about the mean, in dB.
    pub spread_db: Vec<f64>,
    /// The mean of `spread_db` over 100 Hz ... 10 kHz. 0 for one run.
    pub agreement_db: f64,
}

// MARK: Alignment

/// Play-to-record delay in samples, by plain cross-correlation of `recorded`
/// against `reference`, with the sub-sample position from a parabolic fit.
///
/// This is the matched filter of the textbook: `IFFT(R · conj(S))`. It works for
/// any stimulus. For a sweep, `impulse_response` finds the same delay with a
/// sharper peak.
pub fn delay_samples(recorded: &[f64], reference: &[f64]) -> Delay {
    let length = next_power_of_two(recorded.len().max(reference.len()) + 1).max(4);
    let fft = RealFft::new(length);
    let r = fft.forward(recorded);
    let s = fft.forward(reference);
    let mut product = HalfSpectrum::new(length);
    for k in 0..product.bin_count() {
        // R · conj(S)
        product.real[k] = r.real[k] * s.real[k] + r.imag[k] * s.imag[k];
        product.imag[k] = r.imag[k] * s.real[k] - r.real[k] * s.imag[k];
    }
    let correlation = fft.inverse(&product);
    let peak = peak_index(&correlation);
    let rms = rms(&correlation);
    let confidence = if rms > 0.0 {
        (correlation[peak].abs() / (rms * (length as f64).sqrt())).min(1.0)
    } else {
        0.0
    };
    Delay {
        samples: peak,
        sub_sample: parabolic_peak_offset(&correlation, peak),
        confidence,
    }
}

// MARK: Impulse response

/// Deconvolves one recording of `sweep` into an impulse response.
///
/// The recording may start at any time: the delay comes out as `peak_index`.
pub fn impulse_response(recorded: &[f64], sweep: &SweepSignal, options: &Options) -> ImpulseResponse {
    SweepDeconvolver::new(sweep, recorded.len(), options).impulse_response(recorded)
}

// MARK: Window

/// Cuts the linear impulse out of `ir` with an adaptive half-Hann window.
///
/// The leading edge rejects the harmonic impulse responses. The trailing edge
/// stops where the decay reaches the noise floor: a quiet room gets a long
/// window and a noisy one a short one.
pub fn window(ir: &ImpulseResponse, options: &Options) -> WindowedImpulseResponse {
    let n = ir.samples.len();
    let fs = ir.sample_rate;
    let peak = ir.peak_index as i64;

    // Leading edge: the default, but never more than half the way back to the 2nd harmonic.
    let mut pre = ((options.pre_window_seconds * fs) as usize).max(1);
    if let Some(&h2) = ir.harmonic_offset_samples.get(&2) {
        if h2 > 2 {
            pre = pre.min(h2 / 2);
        }
    }
    pre = pre.min(n / 4);

    // Trailing edge: walk in 1 ms blocks until the envelope is on the noise floor.
    let block = ((0.001 * fs) as usize).max(8);
    let floor_rms = noise_floor_rms(&ir.samples, ir.peak_index, fs, &ir.harmonic_offset_samples);
    let stop_rms = floor_rms * 10.0f64.powf(options.decay_stop_margin_db / 20.0);
    let max_post = ((options.maximum_post_window_seconds * fs) as usize).min(n / 2);
    let min_post = ((options.minimum_post_window_seconds * fs) as usize).min(max_post);
    let mut post = max_post;
    let mut quiet = 0;
    let mut offset = 0;
    while offset + block <= max_post {
        let mut sum = 0.0;
        for i in offset..offset + block {
            let v = ir.samples[cyclic_index(peak + i as i64, n)];
            sum += v * v;
        }
        let block_rms = (sum / block as f64).sqrt();
        if block_rms <= stop_rms {
            quiet += 1;
            if quiet >= 3 {
                post = offset + block;
                break;
            }
        } else {
            quiet = 0;
        }
        offset += block;
    }
    post = post.max(min_post).min(max_post);

    window_fixed(ir, pre, post, options.post_window_fade_fraction, None)
}

/// The same window with the lengths fixed by the caller.
///
/// An average of several runs needs the same window on every run, and so does
/// the noise-only segment that the signal-to-noise estimate compares against.
/// `centre` puts the window at another place than the peak.
pub fn window_fixed(
    ir: &ImpulseResponse,
    pre_samples: usize,
    post_samples: usize,
    fade_fraction: f64,
    centre: Option<usize>,
) -> WindowedImpulseResponse {
    let n = ir.samples.len();
    let peak = centre.unwrap_or(ir.peak_index) as i64;
    let pre = pre_samples.min(n / 4).max(1);
    let post = post_samples.min(n / 2).max(1);
    let mut out = vec![0.0; pre + post];
    let fade = ((post as f64 * fade_fraction) as usize).max(1);
    for i in 0..pre + post {
        let sample = ir.samples[cyclic_index(peak - pre as i64 + i as i64, n)];
        let w = if i < pre {
            // Rising half-Hann across the whole leading edge.
            0.5 * (1.0 - (PI * i as f64 / pre as f64).cos())
        } else if i >= pre + post - fade {
            let j = i - (pre + post - fade);
            0.5 * (1.0 + (PI * j as f64 / fade as f64).cos())
        } else {
            1.0
        };
        out[i] = sample * w;
    }
    WindowedImpulseResponse {
        samples: out,
        sample_rate: ir.sample_rate,
        peak_offset: pre,
        pre_samples: pre,
        post_samples: post,
    }
}

// MARK: Magnitude

/// Complex frequency response of a windowed impulse response.
pub fn complex_response(windowed: &WindowedImpulseResponse, options: &Options) -> ComplexResponse {
    let size = next_power_of_two(options.response_fft_size)
        .max(next_power_of_two(windowed.samples.len()))
        .max(4);
    ComplexResponse {
        spectrum: RealFft::new(size).forward(&windowed.samples),
        sample_rate: windowed.sample_rate,
    }
}

/// Magnitude response in dB on `grid`, smoothed.
pub fn magnitude_response(
    windowed: &WindowedImpulseResponse,
    grid: &[f64],
    smoothing: OctaveSmoothing,
    options: &Options,
) -> Vec<f64> {
    complex_response(windowed, options).magnitude_db(grid, smoothing)
}

/// The 200-point log-spaced grid of the embedded curves, 20 Hz ... 20 kHz, as `f64`.
pub fn standard_grid() -> Vec<f64> {
    STANDARD_FREQUENCIES_HZ.iter().map(|&hz| hz as f64).collect()
}

/// Fractional-octave smoothing of a spectrum onto a log grid.
///
/// Power is averaged inside each band with a Hann weight over log frequency.
/// Where a band is narrower than one FFT bin, the value is interpolated between
/// bins, so the bass never goes to zero for want of a bin.
pub(crate) fn smoothed_magnitude_db(
    spectrum: &HalfSpectrum,
    sample_rate: f64,
    grid: &[f64],
    smoothing: OctaveSmoothing,
) -> Vec<f64> {
    let bins = spectrum.bin_count();
    let power: Vec<f64> = (0..bins).map(|k| spectrum.power(k)).collect();
    let bin_hz = sample_rate / spectrum.n as f64;

    let interpolated_db = |f: f64| -> f64 {
        let x = f / bin_hz;
        let k = x as i64;
        if !(k >= 1 && k + 1 < bins as i64) {
            let clamped = k.max(1).min(bins as i64 - 1) as usize;
            return power_db(power[clamped]);
        }
        let k = k as usize;
        let t = x - k as f64;
        power_db(power[k]) * (1.0 - t) + power_db(power[k + 1]) * t
    };

    let Some(b) = smoothing.denominator() else {
        return grid.iter().map(|&f| interpolated_db(f)).collect();
    };

    let half_width = 2.0f64.powf(1.0 / (2.0 * b));
    let span = half_width.ln();
    grid.iter()
        .map(|&f| {
            let low_hz = f / half_width;
            let high_hz = f * half_width;
            let k_low = ((low_hz / bin_hz).ceil() as i64).max(1);
            let k_high = ((high_hz / bin_hz).floor() as i64).min(bins as i64 - 1);
            if k_high < k_low {
                return interpolated_db(f);
            }
            let (mut sum, mut weight) = (0.0, 0.0);
            for k in k_low as usize..=k_high as usize {
                let u = (k as f64 * bin_hz / f).ln() / span; // −1 ... +1 across the band
                let w = 0.5 * (1.0 + (PI * u.abs().min(1.0)).cos());
                sum += w * power[k];
                weight += w;
            }
            if weight > 0.0 {
                power_db(sum / weight)
            } else {
                interpolated_db(f)
            }
        })
        .collect()
}

// MARK: Average of runs

/// Complex average of several runs, with a run-to-run agreement number.
///
/// Complex, not magnitude: an average of magnitudes keeps the noise, an average
/// of the complex responses cancels it, 3 dB per doubling of runs. Each response
/// is first shifted onto the first one by a fractional delay from the peak of
/// their cross-correlation.
///
/// Panics when `runs` is empty or the runs do not share an FFT length.
pub fn average(runs: &[ComplexResponse], grid: &[f64], smoothing: OctaveSmoothing) -> Average {
    assert!(!runs.is_empty(), "average needs at least one run");
    let first = &runs[0];
    if runs.len() == 1 {
        return Average {
            mean: first.clone(),
            spread_db: vec![0.0; grid.len()],
            agreement_db: 0.0,
        };
    }

    let n = first.spectrum.n;
    let fs = first.sample_rate;
    let fft = RealFft::new(n);
    let mut aligned = vec![first.spectrum.clone()];
    for run in &runs[1..] {
        assert_eq!(run.spectrum.n, n, "runs must share an FFT length");
        aligned.push(align_phase(&run.spectrum, &first.spectrum, &fft));
    }

    let mut mean = HalfSpectrum::new(n);
    let scale = 1.0 / aligned.len() as f64;
    for s in &aligned {
        for k in 0..mean.bin_count() {
            mean.real[k] += s.real[k] * scale;
            mean.imag[k] += s.imag[k] * scale;
        }
    }
    let mean = ComplexResponse {
        spectrum: mean,
        sample_rate: fs,
    };

    // Spread: how far the smoothed magnitude of each run is from the average.
    let mean_db = mean.magnitude_db(grid, smoothing);
    let mut spread = vec![0.0; grid.len()];
    for s in &aligned {
        let db = smoothed_magnitude_db(s, fs, grid, smoothing);
        for i in 0..grid.len() {
            let d = db[i] - mean_db[i];
            spread[i] += d * d;
        }
    }
    let divisor = aligned.len() as f64;
    for v in &mut spread {
        *v = (*v / divisor).sqrt();
    }

    let (mut sum, mut used) = (0.0, 0usize);
    for (i, &f) in grid.iter().enumerate() {
        if f >= 100.0 && f <= 10_000.0 {
            sum += spread[i];
            used += 1;
        }
    }
    Average {
        mean,
        spread_db: spread,
        agreement_db: if used > 0 { sum / used as f64 } else { 0.0 },
    }
}

/// Shifts `s` onto `reference` by the fractional delay between them.
fn align_phase(s: &HalfSpectrum, reference: &HalfSpectrum, fft: &RealFft) -> HalfSpectrum {
    let n = s.n;
    let mut cross = HalfSpectrum::new(n);
    for k in 0..cross.bin_count() {
        cross.real[k] = s.real[k] * reference.real[k] + s.imag[k] * reference.imag[k];
        cross.imag[k] = s.imag[k] * reference.real[k] - s.real[k] * reference.imag[k];
    }
    let correlation = fft.inverse(&cross);
    let peak = peak_index(&correlation);
    let mut tau = parabolic_peak_offset(&correlation, peak);
    if tau > (n / 2) as f64 {
        tau -= n as f64; // negative lags wrap round
    }
    if !(tau.abs() > 1e-6) {
        return s.clone();
    }

    let mut out = HalfSpectrum::new(n);
    for k in 0..out.bin_count() {
        let phase = -2.0 * PI * k as f64 * tau / n as f64;
        let (sn, c) = phase.sin_cos();
        out.real[k] = s.real[k] * c - s.imag[k] * sn;
        out.imag[k] = s.real[k] * sn + s.imag[k] * c;
    }
    out
}

// MARK: Signal to noise

/// The noise floor of a deconvolved noise-only recording, as a magnitude
/// spectrum, measured with the same window as the response.
///
/// One window of one noise recording is a poor estimate of a noise floor. So
/// the buffer is cut into `segments` windows and their **powers** are averaged
/// (Welch's method). The macOS app uses 32 segments. Phase is discarded.
pub fn noise_floor_response(
    deconvolved_noise: &[f64],
    sample_rate: f64,
    pre_samples: usize,
    post_samples: usize,
    usable_samples: usize,
    segments: usize,
    options: &Options,
) -> ComplexResponse {
    let length = pre_samples + post_samples;
    let usable = usable_samples.min(deconvolved_noise.len()).max(length);
    // Spread the segments evenly over the noise there is. They overlap when it is short.
    let available = ((usable - length) / (length / 2).max(1) + 1).max(1);
    let count = segments.min(available).max(1);
    let stride = if count > 1 { (usable - length) / (count - 1) } else { 0 };

    let ir = ImpulseResponse {
        samples: deconvolved_noise.to_vec(),
        sample_rate,
        peak_index: 0,
        peak_sub_sample: 0.0,
        noise_floor_db_relative_peak: 0.0,
        harmonic_offset_samples: BTreeMap::new(),
    };
    let mut power: Vec<f64> = Vec::new();
    for segment in 0..count {
        let windowed = window_fixed(
            &ir,
            pre_samples,
            post_samples,
            options.post_window_fade_fraction,
            Some(segment * stride + pre_samples),
        );
        let spectrum = complex_response(&windowed, options).spectrum;
        if power.is_empty() {
            power = vec![0.0; spectrum.bin_count()];
        }
        for k in 0..spectrum.bin_count() {
            power[k] += spectrum.power(k);
        }
    }

    let size = next_power_of_two(options.response_fft_size)
        .max(next_power_of_two(length))
        .max(4);
    let mut mean = HalfSpectrum::new(size);
    for k in 0..power.len().min(mean.bin_count()) {
        mean.real[k] = (power[k] / count as f64).sqrt();
    }
    ComplexResponse {
        spectrum: mean,
        sample_rate,
    }
}

/// The number of noise windows the macOS app averages.
pub const NOISE_SEGMENTS: usize = 32;

/// Signal-to-noise ratio per band in dB: the measured response over the measured noise floor.
///
/// Both sides are the energy in one analysis window, through the same
/// deconvolution and the same window. The noise floor is always read at a third
/// of an octave: a wider band is a steadier estimate.
pub fn signal_to_noise_db(
    signal: &ComplexResponse,
    noise: &ComplexResponse,
    grid: &[f64],
    smoothing: OctaveSmoothing,
) -> Vec<f64> {
    let s = signal.magnitude_db(grid, smoothing);
    let n = noise.magnitude_db(grid, OctaveSmoothing::Third);
    s.iter().zip(&n).map(|(s, n)| s - n).collect()
}

// MARK: Distortion

/// Total harmonic distortion from the 2nd and 3rd harmonic impulse responses.
///
/// The sweep puts the n-th harmonic `L·ln(n)` seconds before the linear
/// impulse, so the energy ratio of the two windows is the harmonic amplitude
/// ratio. Each harmonic is measured over only the part of the band whose
/// harmonic still fits under the end frequency of the sweep (`band_coverage`).
///
/// This is a **quality number**, not a distortion measurement: one figure for
/// the whole band, with the amplifier and the microphone in it.
pub fn harmonic_distortion(ir: &ImpulseResponse, sweep: &SweepSignal, options: &Options) -> HarmonicDistortion {
    if sweep.kind != SweepKind::ExponentialSweep {
        return HarmonicDistortion::default();
    }
    let n = ir.samples.len();
    let fs = ir.sample_rate;
    let pre = ((options.pre_window_seconds * fs) as usize).max(1);
    let post = ((options.minimum_post_window_seconds * fs) as usize).min(n / 8);
    let half = pre.max(post);

    let fundamental = symmetric_energy(&ir.samples, ir.peak_index as i64, half);
    if !(fundamental > 0.0) {
        return HarmonicDistortion::default();
    }

    let mut out = HarmonicDistortion::default();
    let mut total = 0.0;
    for order in [2, 3] {
        let Some(&offset) = ir.harmonic_offset_samples.get(&order) else {
            continue;
        };
        if offset <= half {
            continue;
        }
        let cover = sweep.harmonic_band_coverage(order);
        if !(cover > 0.05) {
            continue;
        }
        let energy = symmetric_energy(&ir.samples, ir.peak_index as i64 - offset as i64, half) / cover;
        out.per_harmonic_percent
            .insert(order, (energy / fundamental).sqrt() * 100.0);
        out.band_coverage.insert(order, cover);
        total += energy;
    }
    out.total_percent = (total / fundamental).sqrt() * 100.0;
    out
}

fn symmetric_energy(x: &[f64], centre: i64, half_width: usize) -> f64 {
    let n = x.len();
    let half = half_width as i64;
    let fade = (half / 4).max(1);
    let mut sum = 0.0;
    for i in -half..=half {
        let v = x[cyclic_index(centre + i, n)];
        let edge = half - i.abs();
        let w = if edge < fade {
            0.5 * (1.0 + (PI * (fade - edge) as f64 / fade as f64).cos())
        } else {
            1.0
        };
        sum += (v * w) * (v * w);
    }
    sum
}

// MARK: Deconvolver

/// Holds the FFT plan and the regularized inverse of one sweep, so an average of
/// N runs does not do again the work that depends only on the stimulus.
pub(crate) struct SweepDeconvolver {
    pub length: usize,
    pub sample_rate: f64,
    fft: RealFft,
    harmonic_offsets: BTreeMap<usize, usize>,
    /// `conj(S) / (|S|² + ε)`.
    kernel: HalfSpectrum,
}

impl SweepDeconvolver {
    pub fn new(sweep: &SweepSignal, recorded_count: usize, options: &Options) -> Self {
        let length = next_power_of_two(recorded_count.max(sweep.samples.len()) + 1).max(4);
        let sample_rate = sweep.sample_rate;
        let fft = RealFft::new(length);

        let s = fft.forward(&sweep.samples);
        let bin_hz = sample_rate / length as f64;
        let ramp = 2.0f64.powf(options.regularization_ramp_octaves / 2.0);
        let inner_low = sweep.start_hz * ramp;
        let inner_high = sweep.end_hz / ramp;
        let outer_low = sweep.start_hz / ramp;
        let outer_high = sweep.end_hz * ramp;

        // Mean in-band power is the reference ε is stated against.
        let (mut sum, mut used) = (0.0, 0usize);
        for k in 1..s.bin_count() {
            let f = k as f64 * bin_hz;
            if f >= inner_low && f <= inner_high {
                sum += s.power(k);
                used += 1;
            }
        }
        let reference = if used > 0 { sum / used as f64 } else { 1.0 };

        let mut kernel = HalfSpectrum::new(length);
        for k in 0..s.bin_count() {
            let shape_db = regularization_db(
                k as f64 * bin_hz,
                [outer_low, inner_low, inner_high, outer_high],
                options.in_band_regularization_db,
                options.out_of_band_regularization_db,
            );
            let epsilon = reference * 10.0f64.powf(shape_db / 10.0);
            let denominator = s.power(k) + epsilon;
            if !(denominator > 0.0) {
                continue;
            }
            kernel.real[k] = s.real[k] / denominator;
            kernel.imag[k] = -s.imag[k] / denominator;
        }

        let mut harmonic_offsets = BTreeMap::new();
        for order in [2, 3, 4, 5] {
            let samples = (sweep.harmonic_time_offset_seconds(order) * sample_rate).round() as i64;
            if samples > 0 {
                harmonic_offsets.insert(order, samples as usize);
            }
        }

        Self {
            length,
            sample_rate,
            fft,
            harmonic_offsets,
            kernel,
        }
    }

    /// Deconvolves a recording into an impulse response.
    pub fn impulse_response(&self, recorded: &[f64]) -> ImpulseResponse {
        let h = self.deconvolve(recorded);
        let peak = peak_index(&h);
        let floor = noise_floor_rms(&h, peak, self.sample_rate, &self.harmonic_offsets);
        let peak_value = h[peak].abs();
        ImpulseResponse {
            peak_index: peak,
            peak_sub_sample: parabolic_peak_offset(&h, peak),
            noise_floor_db_relative_peak: if peak_value > 0.0 {
                amplitude_db(floor / peak_value)
            } else {
                0.0
            },
            harmonic_offset_samples: self.harmonic_offsets.clone(),
            sample_rate: self.sample_rate,
            samples: h,
        }
    }

    /// The raw deconvolution, with no search for a peak.
    pub fn deconvolve(&self, recorded: &[f64]) -> Vec<f64> {
        let r = self.fft.forward(recorded);
        let mut product = HalfSpectrum::new(self.length);
        for k in 0..product.bin_count() {
            product.real[k] = r.real[k] * self.kernel.real[k] - r.imag[k] * self.kernel.imag[k];
            product.imag[k] = r.real[k] * self.kernel.imag[k] + r.imag[k] * self.kernel.real[k];
        }
        self.fft.inverse(&product)
    }
}

/// Raised-cosine ramp, in dB, between the in-band and out-of-band regularization.
/// `edges` is outer low, inner low, inner high, outer high.
pub(crate) fn regularization_db(f: f64, edges: [f64; 4], in_band_db: f64, out_of_band_db: f64) -> f64 {
    let [outer_low, inner_low, inner_high, outer_high] = edges;
    if f >= inner_low && f <= inner_high {
        return in_band_db;
    }
    if f <= outer_low || f >= outer_high {
        return out_of_band_db;
    }
    let t = if f < inner_low {
        (f / outer_low).ln() / (inner_low / outer_low).ln()
    } else {
        (outer_high / f).ln() / (outer_high / inner_high).ln()
    };
    let blend = 0.5 * (1.0 - (PI * t.clamp(0.0, 1.0)).cos());
    out_of_band_db + (in_band_db - out_of_band_db) * blend
}

/// RMS of the quiet stretch just before the impulse peak: noise, and nothing else.
///
/// The stretch ends 20 ms before the peak, so the leading edge of the impulse
/// stays out. It starts no further back than halfway to the 2nd harmonic, so
/// distortion stays out too.
pub(crate) fn noise_floor_rms(x: &[f64], peak: usize, sample_rate: f64, harmonics: &BTreeMap<usize, usize>) -> f64 {
    let n = x.len();
    let mut gap = (0.020 * sample_rate) as i64;
    let mut span = (0.100 * sample_rate) as i64;
    if let Some(&h2) = harmonics.get(&2) {
        if h2 > 16 {
            gap = gap.min(h2 as i64 / 8);
            span = span.min(h2 as i64 / 2 - gap);
        }
    }
    if !(span > 8) {
        return 0.0;
    }
    let mut sum = 0.0;
    for i in 0..span {
        let v = x[cyclic_index(peak as i64 - gap - span + i, n)];
        sum += v * v;
    }
    (sum / span as f64).sqrt()
}
