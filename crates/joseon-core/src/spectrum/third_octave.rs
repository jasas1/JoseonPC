//! IEC 61260 third-octave band levels, summed out of the FFT power spectra the
//! analyzer already computes. No extra transform runs for this.
//!
//! Port source: `Sources/JoseonCore/Spectrum/ThirdOctaveBands.swift`.
//!
//! ## The normalisation
//!
//! The display path is not reusable here. It sums a small fixed number of bins
//! per display bin and adds drawn tone lobes, which is calibrated for tones and
//! only approximately right for noise. A band level has to be right for both,
//! so this reads `SpectrumResolution::power` directly.
//!
//! `power[k] = 4 |X[k]|^2 / (N * Sw2)` with `Sw2 = sum_n w[n]^2`. Parseval for
//! the windowed block `y[n] = x[n] w[n]` says
//! `sum_{k=0}^{N-1} |X[k]|^2 = N * sum_n y[n]^2`, and for a real block the
//! one-sided half (bins `1 ... N/2 - 1`) holds half of that. So
//!
//! ```text
//! sum_k power[k] = 4 / (N * Sw2) * (1/2) * N * sum_n y[n]^2
//!                = 2 * sum_n (x[n] w[n])^2 / Sw2.
//! ```
//!
//! If the mean square of the block is `ms` then `sum_n (x w)^2 = ms * Sw2`
//! (in expectation for stationary noise; up to one half-cycle of ripple for a
//! sine over a window that spans many periods). Both window sums cancel:
//!
//! ```text
//! sum_k power[k] = 2 * ms
//! ```
//!
//! So the RMS level of a band is `10 * log10(sum_{k in band} power[k] / 2)`
//! dBFS RMS, and a full-scale sine inside a band reads −3.01 dBFS.
//!
//! ## Band edges
//!
//! Base-10 bands: exact centre `fc(n) = 1000 * 10^(n/10)` for `n = -17 ... 13`,
//! edges `fc * 10^(±1/20)`. The nominal centres are only labels. Every sum uses
//! the exact edges, so neighbouring bands tile the axis with no gap and no
//! overlap.
//!
//! A bin `k` covers `(k ± 0.5) * df`. Bins that straddle a band edge are taken
//! with the fraction of their width that falls inside.
//!
//! ## Which FFT feeds which band
//!
//! The shortest FFT that puts at least `MIN_BINS_PER_BAND` bins across the band
//! width `0.2308 * fc`. Shortest, because a short window is a fast meter. At
//! 48 kHz: 20–63 Hz from the 32768-point FFT, 80–250 Hz from the 8192-point,
//! 315 Hz–20 kHz from the 2048-point.
//!
//! ## Effective integration time per band
//!
//! The time weighting is IEC 61672 "fast": a one-pole on band power with
//! `FAST_TAU` = 125 ms, stepped by the audio clock. But the FFT window in front
//! of it is an averager too, and for the long windows it is the slower of the
//! two. Only the top group is a true 125 ms fast meter. The bottom six bands
//! integrate over about 600 ms and lag by a third of a second: 4.6 Hz of
//! bandwidth at 20 Hz cannot be measured in 125 ms.

use super::resolution::SpectrumResolution;
use crate::contracts::ThirdOctaveReading;

pub(crate) const BAND_COUNT: usize = 31;
const FLOOR_DB: f32 = ThirdOctaveReading::FLOOR_DB;
/// A resolution can serve a band only when it puts at least this many bins across it.
pub(crate) const MIN_BINS_PER_BAND: f64 = 3.0;
/// IEC 61672 "fast": one pole on band power, 125 ms.
pub(crate) const FAST_TAU: f64 = 0.125;
/// Bands whose exact centre sits above this fraction of Nyquist report the floor.
pub(crate) const NYQUIST_FRACTION: f64 = 0.9;
/// Exponent of the first base-10 band centre: `fc = 1000 * 10^(n/10)`, 20 Hz ... 20 kHz.
const FIRST_EXPONENT: i32 = -17;

/// Exact (not nominal) centre of band `b`.
pub(crate) fn exact_center_hz(b: usize) -> f64 {
    1_000.0 * 10f64.powf((b as i32 + FIRST_EXPONENT) as f64 / 10.0)
}

pub(crate) fn lower_edge_hz(b: usize) -> f64 {
    exact_center_hz(b) * 10f64.powf(-0.05)
}

pub(crate) fn upper_edge_hz(b: usize) -> f64 {
    exact_center_hz(b) * 10f64.powf(0.05)
}

/// Band power to dBFS RMS. The `/ 2` is the whole calibration; see the module header.
pub(crate) fn level_db(power: f32) -> f32 {
    if !(power > 0.0 && power.is_finite()) {
        return FLOOR_DB;
    }
    (10.0 * (power / 2.0).log10()).max(FLOOR_DB)
}

pub(crate) struct ThirdOctaveBank {
    /// Which resolution serves each band: 0 low, 1 mid, 2 high, `None` = floor.
    layer_of: [Option<usize>; BAND_COUNT],
    /// Contiguous band range each resolution owns, `None` when it owns none.
    /// The "shortest FFT with enough bins" rule is monotone in frequency, so
    /// these are ranges.
    layer_bands: [Option<(usize, usize)>; 3],
    first_bin: [usize; BAND_COUNT],
    bin_count: [usize; BAND_COUNT],
    weight_offset: [usize; BAND_COUNT],
    /// Per-bin fraction-inside-the-band weights, all bands end to end.
    weights: Vec<f32>,

    /// Latest instantaneous band power, `channel * BAND_COUNT + band`. Channel 0 left, 1 right.
    target: [f32; 2 * BAND_COUNT],
    /// The fast-weighted band power: what the reading is made of.
    smooth: [f32; 2 * BAND_COUNT],
    /// A resolution that serves bands has transformed at least once.
    seen: [bool; 3],
    configured: bool,
}

impl ThirdOctaveBank {
    pub fn new() -> Self {
        Self {
            layer_of: [None; BAND_COUNT],
            layer_bands: [None; 3],
            first_bin: [0; BAND_COUNT],
            bin_count: [0; BAND_COUNT],
            weight_offset: [0; BAND_COUNT],
            weights: Vec::new(),
            target: [0.0; 2 * BAND_COUNT],
            smooth: [0.0; 2 * BAND_COUNT],
            seen: [false; 3],
            configured: false,
        }
    }

    /// Builds the band-to-bin map. Runs only when the analyzer configures,
    /// never from `process`. Returns the FFT-bin range each resolution has to
    /// compute left/right power over, or `None` where it serves no band.
    pub fn configure(
        &mut self,
        sample_rate: f64,
        resolutions: &[SpectrumResolution; 3],
    ) -> [Option<(usize, usize)>; 3] {
        *self = Self::new();
        let nyquist = sample_rate / 2.0;

        for b in 0..BAND_COUNT {
            let center = exact_center_hz(b);
            let (lower, upper) = (lower_edge_hz(b), upper_edge_hz(b));
            if center > nyquist * NYQUIST_FRACTION {
                continue;
            }
            // Shortest FFT (highest layer index) with enough bins across the band.
            let Some(chosen) = [2usize, 1, 0]
                .into_iter()
                .find(|&layer| (upper - lower) / resolutions[layer].df as f64 >= MIN_BINS_PER_BAND)
            else {
                continue;
            };
            let resolution = &resolutions[chosen];
            let df = resolution.df as f64;
            let max_bin = resolution.half as i64 - 1;
            // Bin k spans (k - 0.5) df ... (k + 0.5) df. These are the bins that overlap the band.
            let k0 = ((lower / df - 0.5).floor() as i64 + 1).max(1);
            let k1 = ((upper / df + 0.5).ceil() as i64 - 1).min(max_bin);
            if k1 < k0 {
                continue;
            }
            self.layer_of[b] = Some(chosen);
            self.first_bin[b] = k0 as usize;
            self.bin_count[b] = (k1 - k0 + 1) as usize;
            self.weight_offset[b] = self.weights.len();
            for k in k0..=k1 {
                let (bin_lo, bin_hi) = ((k as f64 - 0.5) * df, (k as f64 + 0.5) * df);
                let overlap = bin_hi.min(upper) - bin_lo.max(lower);
                self.weights.push((overlap / df).min(1.0).max(0.0) as f32);
            }
            self.layer_bands[chosen] = Some((self.layer_bands[chosen].map_or(b, |(first, _)| first), b));
        }
        self.configured = true;

        self.layer_bands.map(|bands| {
            let (first, last) = bands?;
            let mut range: Option<(usize, usize)> = None;
            for b in (first..=last).filter(|&b| self.bin_count[b] > 0) {
                let (lo, hi) = (self.first_bin[b], self.first_bin[b] + self.bin_count[b] - 1);
                range = Some(range.map_or((lo, hi), |(a, z)| (a.min(lo), z.max(hi))));
            }
            range
        })
    }

    /// New instantaneous band power for the bands this resolution owns. Called
    /// right after the resolution transformed, so it reads the power spectrum
    /// of that hop.
    pub fn capture(&mut self, layer: usize, resolution: &SpectrumResolution) {
        let Some((first, last)) = self.layer_bands[layer] else {
            return;
        };
        self.seen[layer] = true;
        let half = resolution.half;
        for c in 0..2 {
            let pw = &resolution.power[c * half..(c + 1) * half];
            for b in first..=last {
                let n = self.bin_count[b];
                if n == 0 {
                    continue;
                }
                let bins = &pw[self.first_bin[b]..self.first_bin[b] + n];
                let weights = &self.weights[self.weight_offset[b]..self.weight_offset[b] + n];
                let sum: f32 = bins.iter().zip(weights).map(|(p, w)| p * w).sum();
                self.target[c * BAND_COUNT + b] = sum.max(0.0);
            }
        }
    }

    /// Steps the fast integrator by `seconds` of audio. One pole on power, audio-clock driven.
    pub fn advance(&mut self, seconds: f64) {
        if !(self.configured && seconds > 0.0) {
            return;
        }
        let alpha = (1.0 - (-seconds / FAST_TAU).exp()) as f32;
        for (smooth, target) in self.smooth.iter_mut().zip(&self.target) {
            *smooth += alpha * (target - *smooth);
        }
    }

    /// True once every resolution that serves a band has transformed at least once.
    pub fn has_data(&self) -> bool {
        self.configured && (0..3).all(|layer| self.layer_bands[layer].is_none() || self.seen[layer])
    }

    pub fn reading(&self) -> Option<ThirdOctaveReading> {
        if !self.has_data() {
            return None;
        }
        let left = self.smooth[..BAND_COUNT].iter().map(|&p| level_db(p)).collect();
        let right = self.smooth[BAND_COUNT..].iter().map(|&p| level_db(p)).collect();
        Some(ThirdOctaveReading::new(left, right))
    }

    /// Clears the integrators. The band map and the "has transformed" flags are
    /// kept: the analyzer's own `reset` keeps the FFT history too, so the meter
    /// refills and does not blank.
    pub fn reset(&mut self) {
        self.target = [0.0; 2 * BAND_COUNT];
        self.smooth = [0.0; 2 * BAND_COUNT];
    }

    // MARK: Test hooks

    /// Which resolution (0 low, 1 mid, 2 high, `None` floor) serves each band.
    #[cfg(test)]
    pub fn layer_assignment(&self) -> [Option<usize>; BAND_COUNT] {
        self.layer_of
    }

    /// FFT bins summed for a band: first bin, bin count, and the sum of their
    /// weights (the band width in bins).
    #[cfg(test)]
    pub fn footprint(&self, band: usize) -> (usize, usize, f32) {
        let offset = self.weight_offset[band];
        let sum = self.weights[offset..offset + self.bin_count[band]].iter().sum();
        (self.first_bin[band], self.bin_count[band], sum)
    }
}
