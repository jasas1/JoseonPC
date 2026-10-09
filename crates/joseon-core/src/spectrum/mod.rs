//! Multi-resolution FFT spectrum analyzer.
//!
//! Port source: `Sources/JoseonCore/Spectrum/SpectrumAnalyzer.swift`.
//!
//! Three Hann-windowed FFTs run in parallel over one shared history ring:
//!
//! | band | window at 48 kHz | duration | hop  | refresh |
//! |------|------------------|----------|------|---------|
//! | low  | 32768            | 683 ms   | 5461 | 8.8 Hz  |
//! | mid  | 8192             | 171 ms   | 1024 | 46.9 Hz |
//! | high | 2048             | 43 ms    | 768  | 62.5 Hz |
//!
//! The hop rates differ on purpose: frames are read at 60 Hz, so a faster
//! transform is work nobody sees, and a 683 ms window cannot say anything new
//! every 16 ms.
//!
//! Sizes scale with the sample rate (nearest power of two), so the window
//! *durations* stay the same from 44.1 kHz to 192 kHz.
//!
//! ## Two crossfades, not one
//!
//! A tone reads the same level from any of the three FFTs, but broadband
//! content does not: the coarser FFT collects noise over four times the
//! bandwidth, so it sits 6 dB higher. So there are two separate transitions:
//!
//! - **Blend**, smoothstep in log frequency over half an octave either side of
//!   200 Hz and 2 kHz. This is which FFT the display bin comes from. The blend
//!   is in the power domain and comes *after* the release smoothing, which runs
//!   per resolution. So the three curves in `SpectrumReading::mid_layers`
//!   blended with their weights are exactly `mid`.
//! - **Noise bandwidth ramp**, one and a half octaves either side of 100 Hz and
//!   1 kHz. Each resolution widens its per-bin power sum to hit a common target
//!   bandwidth, so the two curves in a blend agree on noise as well as on tones.
//!
//! Levels are peak-amplitude calibrated: a full-scale sine reads 0 dBFS at any
//! frequency and any FFT size.
//!
//! Threading: one thread owns the analyzer. Allocation: everything is allocated
//! in `configure`, which runs on the first call and again only when the sample
//! rate or the display grid changes. `process` does not allocate in the steady state.

// This module is a port, line for line where it can be. Bounds like
// `<= half - 1` stay as in the Swift source, so the two read side by side.
// `.max(a).min(b)` stays too: `clamp` panics on a NaN bound.
#![allow(clippy::int_plus_one, clippy::manual_clamp)]

mod resolution;
mod support;
mod third_octave;

#[cfg(test)]
mod tests;

pub use support::note_for_hz;

use crate::contracts::{
    BandEnergy, PeakReading, SpectrumAnalyzing, SpectrumLayer, SpectrumOutput, SpectrumReading, SpectrumSettings,
    ThirdOctaveReading,
};
use resolution::{SpectrumResolution, CHANNEL_COUNT, CH_LEFT, CH_MID, CH_RIGHT, CH_SIDE};
use support::{nearest_pow2, next_pow2};
use third_octave::ThirdOctaveBank;

// Window lengths at 48 kHz. Scaled to other rates by nearest power of two.
const LOW_SIZE_AT_48K: f64 = 32_768.0;
const MID_SIZE_AT_48K: f64 = 8_192.0;
const HIGH_SIZE_AT_48K: f64 = 2_048.0;
const REFERENCE_RATE: f64 = 48_000.0;

// Crossover centres and half-width of the crossfade, in octaves.
const LOW_MID_HZ: f32 = 200.0;
const MID_HIGH_HZ: f32 = 2_000.0;
const CROSSFADE_OCTAVES: f32 = 0.5;
/// Half-width of the noise-bandwidth ramp, in octaves. Wider than the blend on purpose.
const BANDWIDTH_OCTAVES: f32 = 1.5;

const FLOOR_DB: f32 = SpectrumReading::FLOOR_DB;
/// `FLOOR_DB` as power.
const FLOOR_POWER: f32 = 1e-12;
const HISTOGRAM_BUCKETS: usize = 141;

/// At most this many entries in the top-peaks list.
pub const MAX_TOP_PEAKS: usize = 5;
/// Two peaks closer than this are the same tone as far as the list is concerned.
const TOP_PEAK_SEPARATION_OCTAVES: f32 = 1.0 / 6.0;
/// Seconds of measurement before `lowest_strong_hz` answers at all.
const LOWEST_STRONG_WARMUP_SECONDS: f64 = 3.0;
/// How far below the loudest part of the long-term average still counts as strong content.
const LOWEST_STRONG_RANGE_DB: f32 = 12.0;
/// The answer has to hold still (within 1/12 octave) this long before it is published.
const LOWEST_STRONG_STABLE_SECONDS: f64 = 1.0;
/// Never below this: under 20 Hz a number would be about the analyzer, not about the music.
const LOWEST_STRONG_MIN_HZ: f32 = 20.0;
/// A tonal peak stands this far over the lowest point on each side within `TONAL_REACH_OCTAVES`...
const TONAL_PROMINENCE_DB: f32 = 8.0;
const TONAL_REACH_OCTAVES: f32 = 1.0 / 6.0;
/// ...and is narrower than this at −3 dB (or than 1.3 times the analyzer's own
/// lobe, where that is wider: below about 40 Hz a pure tone measures 1/11
/// octave in a 683 ms window).
const TONAL_WIDTH_OCTAVES: f32 = 1.0 / 12.0;

/// A run of display bins that comes from one resolution (`b` is `None`) or from a blend of two.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Segment {
    lo: usize,
    hi: usize,
    a: usize,
    b: Option<usize>,
}

/// The top-peaks list while it is built: strongest first.
#[derive(Default)]
struct PeakList {
    hz: [f32; MAX_TOP_PEAKS],
    level: [f32; MAX_TOP_PEAKS],
    bin: [usize; MAX_TOP_PEAKS],
    count: usize,
}

impl PeakList {
    fn shift_down(&mut self, from: usize, to: usize) {
        self.hz[to] = self.hz[from];
        self.level[to] = self.level[from];
        self.bin[to] = self.bin[from];
    }

    fn insert(&mut self, bin: usize, level: f32, hz: f32, ratio: f32) {
        // Drop it if a stronger entry already owns this part of the spectrum.
        // Otherwise take over the weaker entry that does.
        let mut replace = None;
        for k in 0..self.count {
            let other = self.hz[k];
            let r = if hz > other { hz / other } else { other / hz };
            if r < ratio {
                if self.level[k] >= level {
                    return;
                }
                replace = Some(k);
                break;
            }
        }
        if let Some(replace) = replace {
            for k in replace..self.count - 1 {
                self.shift_down(k + 1, k);
            }
            self.count -= 1;
        } else if self.count == MAX_TOP_PEAKS && level <= self.level[self.count - 1] {
            return;
        }
        let mut slot = self.count.min(MAX_TOP_PEAKS - 1);
        while slot > 0 && self.level[slot - 1] < level {
            self.shift_down(slot - 1, slot);
            slot -= 1;
        }
        self.hz[slot] = hz;
        self.level[slot] = level;
        self.bin[slot] = bin;
        self.count = (self.count + 1).min(MAX_TOP_PEAKS);
    }
}

pub struct SpectrumAnalyzer {
    settings: SpectrumSettings,

    /// Low, mid, high. `None` before the first configure.
    resolutions: Option<Box<[SpectrumResolution; 3]>>,
    configured_rate: f64,
    configured_bins: usize,
    configured_min_hz: f32,
    configured_max_hz: f32,
    configured_tilt: f32,

    /// History rings. The length is a power of two.
    history_l: Vec<f32>,
    history_r: Vec<f32>,
    samples_written: usize,

    /// Crossfade weight per layer and display bin.
    weights: [Vec<f32>; 3],
    tilt_db: Vec<f32>,
    frequencies: Vec<f32>,

    /// Per resolution and channel, after attack/release: `(layer * 4 + channel) * bins`.
    layer_smooth: Vec<f32>,
    /// `channel * bins`: the blend of `layer_smooth`, untilted dB.
    smooth: Vec<f32>,
    /// Blend of the unsmoothed mid curves, as power.
    target_mid_power: Vec<f32>,
    /// First and last display bin each layer feeds. Empty when first > last.
    layer_range: [(isize, isize); 3],
    layer_latency: [f32; 3],
    segments: Vec<Segment>,
    /// Widest −3 dB width that still counts as a tone, per display bin.
    width_limit_octaves: Vec<f32>,
    /// Scratch for `update_lowest_strong`.
    smoothed_average: Vec<f32>,
    peak_hold_db: Vec<f32>,
    /// Running power mean since reset.
    average_power: Vec<f32>,
    /// Seconds that went into `average_power`, per entry of `segments`, since
    /// `reset`. Every display bin of a segment is fed by the same
    /// resolution(s), so they share one count. A segment counts time only while
    /// all the resolutions that feed it have data. One clock for all bins would
    /// make the bins of the long FFT average the floor for the 0.8 s before its
    /// first transform.
    segment_seconds: Vec<f64>,

    /// IEC 61260 third-octave band levels, off the same power spectra.
    pub(crate) third_octave_bank: ThirdOctaveBank,

    band_start_bin: [isize; 8],
    band_end_bin: [isize; 8],
    histogram: [u32; HISTOGRAM_BUCKETS],

    audio_time: f64,
    last_smooth_time: f64,
    /// The measurement clock since `reset` (`lowest_strong_hz` runs on it). Not
    /// the averaging denominator: that is `segment_seconds`.
    average_seconds: f64,
    current_peak: PeakReading,
    current_bands: BandEnergy,
    median_mid_db: f32,
    median_stale: bool,
    peaks_stale: bool,
    bins_per_octave: f32,

    lowest_candidate_hz: f32,
    lowest_candidate_since: f64,
    lowest_published_hz: f32,

    peaks: PeakList,
    /// The first entry of the list is `peak` itself and `peak` is not tonal (no note name).
    first_peak_is_broad: bool,
}

impl Default for SpectrumAnalyzer {
    fn default() -> Self {
        Self::new(SpectrumSettings::default())
    }
}

impl SpectrumAnalyzer {
    pub fn new(settings: SpectrumSettings) -> Self {
        Self {
            settings,
            resolutions: None,
            configured_rate: 0.0,
            configured_bins: 0,
            configured_min_hz: 0.0,
            configured_max_hz: 0.0,
            configured_tilt: f32::NAN,
            history_l: Vec::new(),
            history_r: Vec::new(),
            samples_written: 0,
            weights: [Vec::new(), Vec::new(), Vec::new()],
            tilt_db: Vec::new(),
            frequencies: Vec::new(),
            layer_smooth: Vec::new(),
            smooth: Vec::new(),
            target_mid_power: Vec::new(),
            layer_range: [(0, -1); 3],
            layer_latency: [0.0; 3],
            segments: Vec::new(),
            width_limit_octaves: Vec::new(),
            smoothed_average: Vec::new(),
            peak_hold_db: Vec::new(),
            average_power: Vec::new(),
            segment_seconds: Vec::new(),
            third_octave_bank: ThirdOctaveBank::new(),
            band_start_bin: [0; 8],
            band_end_bin: [0; 8],
            histogram: [0; HISTOGRAM_BUCKETS],
            audio_time: 0.0,
            last_smooth_time: 0.0,
            average_seconds: 0.0,
            current_peak: PeakReading::default(),
            current_bands: BandEnergy::default(),
            median_mid_db: FLOOR_DB,
            median_stale: true,
            peaks_stale: true,
            bins_per_octave: 91.0,
            lowest_candidate_hz: 0.0,
            lowest_candidate_since: 0.0,
            lowest_published_hz: 0.0,
            peaks: PeakList::default(),
            first_peak_is_broad: false,
        }
    }

    // MARK: Configuration

    fn configure_if_needed(&mut self, sample_rate: f64) {
        let bins = self.settings.display_bins.clamp(2, 1 << 14);
        let min_hz = self.settings.min_hz.max(0.1);
        let max_hz = self.settings.max_hz.max(min_hz * 2.0);
        let structural = sample_rate != self.configured_rate
            || bins != self.configured_bins
            || min_hz != self.configured_min_hz
            || max_hz != self.configured_max_hz;
        if structural {
            self.configure(sample_rate, bins, min_hz, max_hz);
        }
        if self.settings.tilt_db_per_octave != self.configured_tilt {
            self.configured_tilt = self.settings.tilt_db_per_octave;
            for (tilt, f) in self.tilt_db.iter_mut().zip(&self.frequencies) {
                *tilt = self.configured_tilt * (f / 1_000.0).log2();
            }
        }
    }

    fn configure(&mut self, sample_rate: f64, bins: usize, min_hz: f32, max_hz: f32) {
        self.configured_rate = sample_rate;
        self.configured_bins = bins;
        self.configured_min_hz = min_hz;
        self.configured_max_hz = max_hz;
        self.configured_tilt = f32::NAN;

        let scale = sample_rate / REFERENCE_RATE;
        let low_size = nearest_pow2(LOW_SIZE_AT_48K * scale, 4_096, 1 << 17);
        let mid_size = nearest_pow2(MID_SIZE_AT_48K * scale, 1_024, 1 << 15);
        let high_size = nearest_pow2(HIGH_SIZE_AT_48K * scale, 256, 1 << 13);

        // Hop rates differ on purpose. Lows: 683 ms of window, so a
        // 6th-of-a-window hop is 8.8 Hz, which is faster than the band moves
        // and is what drives the peak readout. Highs: 62.5 Hz, just over the
        // frame rate.
        let made = (|| {
            Some([
                SpectrumResolution::new(low_size, low_size / 6, sample_rate)?,
                SpectrumResolution::new(mid_size, mid_size / 8, sample_rate)?,
                SpectrumResolution::new(high_size, high_size * 3 / 8, sample_rate)?,
            ])
        })();

        // The ring has to hold the longest window plus one hop of slack.
        let history_capacity = next_pow2(low_size * 2);
        self.history_l = vec![0.0; history_capacity];
        self.history_r = vec![0.0; history_capacity];
        self.samples_written = 0;
        self.audio_time = 0.0;
        self.last_smooth_time = 0.0;

        // Log-spaced display grid, identical to `SpectrumReading::silent`.
        let ratio = max_hz / min_hz;
        let step = ratio.powf(1.0 / (bins - 1) as f32);
        self.frequencies = (0..bins)
            .map(|i| min_hz * ratio.powf(i as f32 / (bins - 1) as f32))
            .collect();

        self.tilt_db = vec![0.0; bins];
        self.weights = [vec![0.0; bins], vec![0.0; bins], vec![0.0; bins]];
        for (i, &f) in self.frequencies.iter().enumerate() {
            let low = 1.0 - smoothstep_octaves(f, LOW_MID_HZ, CROSSFADE_OCTAVES);
            let high = smoothstep_octaves(f, MID_HIGH_HZ, CROSSFADE_OCTAVES);
            self.weights[0][i] = low;
            self.weights[2][i] = high;
            self.weights[1][i] = (1.0 - low - high).max(0.0);
        }

        self.layer_smooth = vec![FLOOR_DB; 3 * CHANNEL_COUNT * bins];
        self.target_mid_power = vec![FLOOR_POWER; bins];
        self.smoothed_average = vec![0.0; bins];
        self.smooth = vec![FLOOR_DB; CHANNEL_COUNT * bins];

        // Which resolution, or which two, each display bin comes from.
        self.segments.clear();
        self.layer_range = [(bins as isize, -1); 3];
        for i in 0..bins {
            let mut order: Vec<usize> = (0..3).filter(|&layer| self.weights[layer][i] > 0.0).collect();
            order.sort_by(|&x, &y| self.weights[y][i].total_cmp(&self.weights[x][i]));
            let a = order.first().copied().unwrap_or(1);
            let pair = match order.get(1) {
                Some(&b) => (a.min(b), Some(a.max(b))),
                None => (a, None),
            };
            for &layer in order.iter().take(2) {
                let range = &mut self.layer_range[layer];
                *range = (range.0.min(i as isize), range.1.max(i as isize));
            }
            match self.segments.last_mut() {
                Some(last) if last.a == pair.0 && last.b == pair.1 => last.hi = i,
                _ => self.segments.push(Segment {
                    lo: i,
                    hi: i,
                    a: pair.0,
                    b: pair.1,
                }),
            }
        }

        self.bins_per_octave = 1.0 / step.log2();
        self.median_stale = true;
        self.peaks_stale = false;
        self.peaks.count = 0;
        self.first_peak_is_broad = false;
        self.lowest_candidate_hz = 0.0;
        self.lowest_candidate_since = 0.0;
        self.lowest_published_hz = 0.0;
        self.peak_hold_db = vec![FLOOR_DB; bins];
        self.average_power = vec![0.0; bins];
        self.average_seconds = 0.0;
        self.segment_seconds = vec![0.0; self.segments.len()];
        self.current_peak = PeakReading::default();
        self.current_bands = BandEnergy::default();
        self.width_limit_octaves = vec![TONAL_WIDTH_OCTAVES; bins];
        self.layer_latency = [0.0; 3];
        self.third_octave_bank = ThirdOctaveBank::new();

        let Some(mut resolutions) = made else {
            self.resolutions = None;
            return;
        };
        // The peak readout and the band energies come from the long FFT across
        // the whole band, so its mid channel is the one resolution/channel pair
        // that needs every bin.
        resolutions[0].full_spectrum_channel = Some(CH_MID);

        // The bandwidth ramp is wider than the blend and sits an octave lower.
        // So by the time a coarser resolution starts to contribute, the target
        // bandwidth has already reached what that resolution can deliver, and
        // the 4x bandwidth change is spread over 1.5 octaves.
        let [log2_df_low, log2_df_mid, log2_df_high] = [0, 1, 2].map(|layer| resolutions[layer].df.log2());
        let mix = |s_low: f32, s_high: f32| {
            (1.0 - s_low) * log2_df_low + s_low * (1.0 - s_high) * log2_df_mid + s_high * log2_df_high
        };
        let bandwidth = |f: f32| {
            let s_low = smoothstep_octaves(f, LOW_MID_HZ / 2.0, BANDWIDTH_OCTAVES);
            let s_high = smoothstep_octaves(f, MID_HIGH_HZ / 2.0, BANDWIDTH_OCTAVES);
            // Three bins, not the five of the Hann main lobe: tonal peaks are
            // drawn from their own curve, so the noise curve keeps the finest
            // bandwidth the FFT really has.
            3.0 * mix(s_low, s_high).exp2()
        };
        // Bin width of the lobe a tone is drawn with: the FFT's own outside the
        // crossfades, and inside one the same for both resolutions, moving with
        // the blend weight.
        let tone_bin = |f: f32| {
            let s_low = smoothstep_octaves(f, LOW_MID_HZ, CROSSFADE_OCTAVES);
            let s_high = smoothstep_octaves(f, MID_HIGH_HZ, CROSSFADE_OCTAVES);
            mix(s_low, s_high).exp2()
        };
        for (resolution, weight) in resolutions.iter_mut().zip(&self.weights) {
            resolution.update_map(&self.frequencies, step, weight, &bandwidth, &tone_bin);
        }
        self.layer_latency = [0, 1, 2].map(|layer| (resolutions[layer].size as f64 / 2.0 / sample_rate) as f32);

        // −3 dB width of a Hann lobe is 1.44 bins; the display adds one display bin to it.
        for (limit, &f) in self.width_limit_octaves.iter_mut().zip(&self.frequencies) {
            let own = 1.44 * tone_bin(f) / (f * std::f32::consts::LN_2) + step.log2();
            *limit = TONAL_WIDTH_OCTAVES.max(1.3 * own);
        }

        // Band energy bins on the long FFT.
        let low = &resolutions[0];
        let nyquist_bin = low.half as isize - 1;
        for b in 0..8 {
            self.band_start_bin[b] = ((BandEnergy::EDGES_HZ[b] / low.df).ceil() as isize).max(1);
            self.band_end_bin[b] = ((BandEnergy::EDGES_HZ[b + 1] / low.df).ceil() as isize - 1).min(nyquist_bin);
        }

        // Third-octave map. It is built after `update_map`, because it widens
        // the left/right power range each resolution computes, and that has to
        // sit on top of the display's own range.
        let ranges = self.third_octave_bank.configure(sample_rate, &resolutions);
        for (resolution, range) in resolutions.iter_mut().zip(ranges) {
            resolution.extra_power_range = range;
        }
        self.resolutions = Some(Box::new(resolutions));
    }

    // MARK: History

    fn append_history(&mut self, left: &[f32], right: &[f32]) {
        let capacity = self.history_l.len();
        let mask = capacity - 1;
        let count = left.len();
        let mut offset = count.saturating_sub(capacity);
        let mut position = (self.samples_written + offset) & mask;
        while offset < count {
            let chunk = (count - offset).min(capacity - position);
            self.history_l[position..position + chunk].copy_from_slice(&left[offset..offset + chunk]);
            self.history_r[position..position + chunk].copy_from_slice(&right[offset..offset + chunk]);
            position = (position + chunk) & mask;
            offset += chunk;
        }
    }

    // MARK: Composite, smoothing, output

    /// Instant attack, exponential release, per resolution, then the blend.
    /// `release_seconds` is the time constant of a one-pole in the dB domain.
    /// Driven by the audio clock, so it is deterministic in tests.
    fn apply_smoothing(&mut self, resolutions: &[SpectrumResolution; 3]) {
        let n = self.configured_bins;
        let dt = (self.audio_time - self.last_smooth_time).max(0.0);
        self.last_smooth_time = self.audio_time;
        let tau = self.settings.release_seconds;
        let coefficient: f32 = if tau > 0.0 && dt > 0.0 {
            (-dt as f32 / tau).exp()
        } else if dt > 0.0 {
            0.0
        } else {
            1.0
        };
        let rest = 1.0 - coefficient;

        for layer in 0..3 {
            let (first, last) = self.layer_range[layer];
            if last < first {
                continue;
            }
            let (first, last) = (first as usize, last as usize);
            for c in 0..CHANNEL_COUNT {
                let target = &resolutions[layer].display_db[c * n + first..=c * n + last];
                let base = (layer * CHANNEL_COUNT + c) * n;
                for (state, &target) in self.layer_smooth[base + first..=base + last].iter_mut().zip(target) {
                    // Hold the target at the display floor. Without this a
                    // digital-silence target of about −300 dB would make the
                    // release look instant on the way down.
                    let target = target.max(FLOOR_DB);
                    *state = (*state * coefficient + target * rest).max(target);
                }
            }
        }
        for c in 0..CHANNEL_COUNT {
            let sources = [0, 1, 2].map(|layer| {
                let base = (layer * CHANNEL_COUNT + c) * n;
                &self.layer_smooth[base..base + n]
            });
            blend(
                &self.segments,
                &self.weights,
                sources,
                &mut self.smooth[c * n..(c + 1) * n],
                false,
            );
        }

        // Peak hold of mid: linear dB ramp down, jumps up to the displayed curve.
        let fall = -self.settings.peak_decay_db_per_second * dt as f32;
        let mid = &self.smooth[CH_MID * n..(CH_MID + 1) * n];
        for (hold, &level) in self.peak_hold_db.iter_mut().zip(mid) {
            *hold = (*hold + fall).max(level);
        }
    }

    /// Running power mean of the untilted mid curve since `reset`, per display
    /// bin over the time that bin has been measured (see `segment_seconds`).
    fn accumulate_average(&mut self, seconds: f64, has_data: [bool; 3]) {
        if !(seconds > 0.0) {
            return;
        }
        self.average_seconds += seconds;
        for (segment, measured) in self.segments.iter().zip(&mut self.segment_seconds) {
            // Before its FFT has run, `target_mid_power` holds the floor there: not a measurement.
            if !(has_data[segment.a] && segment.b.is_none_or(|b| has_data[b])) {
                continue;
            }
            *measured += seconds;
            let w = (seconds / *measured) as f32;
            let range = segment.lo..=segment.hi;
            for (mean, &new) in self.average_power[range.clone()]
                .iter_mut()
                .zip(&self.target_mid_power[range])
            {
                *mean += (new - *mean) * w;
            }
        }
    }

    /// The long-term average in untilted dB.
    fn average_db(&self) -> Vec<f32> {
        let n = self.configured_bins;
        if !(self.average_seconds > 0.0) {
            return vec![FLOOR_DB; n];
        }
        let mut out: Vec<f32> = self.average_power.iter().map(|&p| 10.0 * (p + 1e-30).log10()).collect();
        // Nothing measured yet in these bins: the floor, not the logarithm of an empty mean.
        for (segment, &measured) in self.segments.iter().zip(&self.segment_seconds) {
            if measured <= 0.0 {
                out[segment.lo..=segment.hi].fill(FLOOR_DB);
            }
        }
        out
    }

    /// Adds the tilt and clamps to the floor.
    fn shaped(&self, source: &[f32]) -> Vec<f32> {
        source
            .iter()
            .zip(&self.tilt_db)
            .map(|(level, tilt)| (level + tilt).max(FLOOR_DB))
            .collect()
    }

    // MARK: Peak and band energy

    /// Band energy comes from the longest FFT: it has the finest bins at every
    /// frequency, and it is a slow meter that does not need a 60 Hz refresh.
    ///
    /// Plain power sums over the FFT bins of each band, same calibration as the
    /// display, so a full-scale sine inside a band reads 0 dB. Taken from the
    /// mid channel, so the numbers agree with `SpectrumReading::mid`.
    fn update_bands(&mut self, low: &SpectrumResolution) {
        let pw = &low.power[CH_MID * low.half..(CH_MID + 1) * low.half];
        let max_bin = low.half as isize - 1;
        let mut values = [FLOOR_DB; 8];
        for (b, value) in values.iter_mut().enumerate() {
            let (start, end) = (self.band_start_bin[b], self.band_end_bin[b].min(max_bin));
            if end < start {
                continue;
            }
            let mut sum = 0.0f32;
            for &p in &pw[start as usize..=end as usize] {
                sum += p;
            }
            *value = (10.0 * sum.max(1e-30).log10()).max(FLOOR_DB);
        }
        let [sub_bass, bass, low_mid, mid, upper_mid, presence, brilliance, air] = values;
        self.current_bands = BandEnergy {
            sub_bass,
            bass,
            low_mid,
            mid,
            upper_mid,
            presence,
            brilliance,
            air,
        };
    }

    /// `peak` and the top-peaks list, from the mid display curve as it stands
    /// after smoothing. Both come from the curve, so a marker sits on it, and a
    /// broad maximum (a kick drum's hump) gets no note name.
    fn find_peaks(&mut self, low: Option<&SpectrumResolution>) {
        self.peaks_stale = false;
        self.peaks.count = 0;
        self.first_peak_is_broad = false;
        let n = self.configured_bins;
        if !(n >= 3 && self.samples_written > 0) {
            self.current_peak = PeakReading::default();
            return;
        }
        let s = &self.smooth[CH_MID * n..(CH_MID + 1) * n];

        let (mut top, mut g) = (s[0], 0);
        for (i, &level) in s.iter().enumerate() {
            if level > top {
                (top, g) = (level, i);
            }
        }
        if !(top > FLOOR_DB + 0.5) {
            self.current_peak = PeakReading::default();
            return;
        }

        if self.median_stale {
            self.median_mid_db = median_db(s, &mut self.histogram);
            self.median_stale = false;
        }
        let gate = (self.median_mid_db + 10.0).max(-80.0);
        let (bins_per_octave, limits) = (self.bins_per_octave, &self.width_limit_octaves);
        let refine = |bin: usize| refined_frequency(&self.frequencies, low, bins_per_octave, bin);

        let top_hz = refine(g);
        let top_is_tonal = top >= gate && g > 0 && g < n - 1 && is_tonal(s, g, bins_per_octave, limits);
        let (note_name, cents) = if top_is_tonal {
            note_for_hz(top_hz)
        } else {
            (String::new(), 0.0)
        };
        self.current_peak = PeakReading {
            frequency_hz: top_hz,
            level_db: top.max(FLOOR_DB),
            note_name,
            cents,
        };

        // Every other tonal local maximum over the gate, strongest first, a sixth of an octave apart.
        let ratio = TOP_PEAK_SEPARATION_OCTAVES.exp2();
        let mut i = 1;
        while i < n - 1 {
            let v = s[i];
            if v >= gate && v > s[i - 1] && v >= s[i + 1] && is_tonal(s, i, bins_per_octave, limits) {
                self.peaks.insert(i, v, self.frequencies[i], ratio);
                i += 2;
            } else {
                i += 1;
            }
        }
        for k in 0..self.peaks.count {
            self.peaks.hz[k] = refine(self.peaks.bin[k]);
        }

        // The contract: a list that is not empty starts with `peak`.
        if self.peaks.count > 0 && !top_is_tonal {
            let count = self.peaks.count.min(MAX_TOP_PEAKS - 1);
            for k in (1..=count).rev() {
                self.peaks.shift_down(k - 1, k);
            }
            self.peaks.hz[0] = top_hz;
            self.peaks.level[0] = top;
            self.peaks.bin[0] = g;
            self.peaks.count = count + 1;
            self.first_peak_is_broad = true;
        }
    }

    // MARK: Lowest strong content

    /// Runs when the long FFT has hopped (8.8 Hz): that is as fast as the bottom of the average moves.
    fn update_lowest_strong(&mut self) {
        let n = self.configured_bins;
        if !(n >= 2 && self.average_seconds > 0.0) {
            return;
        }
        let Some(first) = self.frequencies.iter().position(|&f| f >= LOWEST_STRONG_MIN_HZ) else {
            return;
        };

        // A third of an octave, as a running power mean over the display bins.
        let half = ((self.bins_per_octave / 6.0).round() as usize).max(1);
        let p = &self.average_power;
        let out = &mut self.smoothed_average;
        let mut sum = 0.0f32;
        let (mut lo, mut next) = (first, first); // the sum covers lo..next
        let mut best = 0.0f32;
        for i in first..n {
            let want_lo = i.saturating_sub(half).max(first);
            let want_hi = (i + half).min(n - 1);
            while next <= want_hi {
                sum += p[next];
                next += 1;
            }
            while lo < want_lo {
                sum -= p[lo];
                lo += 1;
            }
            let mean = sum.max(0.0) / (next - lo) as f32;
            out[i] = mean;
            if mean > best {
                best = mean;
            }
        }
        // Silence, or nothing but the display floor: no answer.
        if !(best > FLOOR_POWER * 100.0) {
            self.publish_lowest(0.0);
            return;
        }
        let threshold = best * 10f32.powf(-LOWEST_STRONG_RANGE_DB / 10.0);
        let Some(mut found) = (first..n).find(|&i| out[i] >= threshold) else {
            self.publish_lowest(0.0);
            return;
        };
        // The smoothing spreads a tone a sixth of an octave down. Inside that
        // reach, the lowest bin of the unsmoothed average that is itself over
        // the threshold is where the content is.
        if let Some(sharper) = (found..=(found + half).min(n - 1)).find(|&i| p[i] >= threshold) {
            found = sharper;
        }
        let hz = self.frequencies[found].round().max(LOWEST_STRONG_MIN_HZ);
        self.publish_lowest(hz);
    }

    fn publish_lowest(&mut self, hz: f32) {
        if !(hz > 0.0) {
            self.lowest_candidate_hz = 0.0;
            self.lowest_published_hz = 0.0;
            return;
        }
        let moved = self.lowest_candidate_hz <= 0.0 || (hz / self.lowest_candidate_hz).log2().abs() > 1.0 / 12.0;
        if moved {
            self.lowest_candidate_hz = hz;
            self.lowest_candidate_since = self.average_seconds;
        }
        let stable = self.average_seconds - self.lowest_candidate_since >= LOWEST_STRONG_STABLE_SECONDS;
        if stable && self.average_seconds >= LOWEST_STRONG_WARMUP_SECONDS {
            self.lowest_published_hz = hz;
            self.lowest_candidate_hz = hz; // follow a slow drift; do not call it a move
        }
    }

    /// Test access to the three resolutions.
    #[cfg(test)]
    pub(crate) fn resolutions(&self) -> Option<&[SpectrumResolution; 3]> {
        self.resolutions.as_deref()
    }
}

impl SpectrumAnalyzing for SpectrumAnalyzer {
    fn settings(&self) -> &SpectrumSettings {
        &self.settings
    }

    fn set_settings(&mut self, settings: SpectrumSettings) {
        self.settings = settings;
    }

    fn process(&mut self, left: &[f32], right: &[f32], sample_rate: f64) {
        let count = left.len().min(right.len());
        if !(count > 0 && sample_rate > 0.0 && sample_rate.is_finite()) {
            return;
        }
        let (left, right) = (&left[..count], &right[..count]);
        self.configure_if_needed(sample_rate);
        let Some(mut resolutions) = self.resolutions.take() else {
            return;
        };

        self.append_history(left, right);
        self.samples_written += count;
        self.audio_time = self.samples_written as f64 / self.configured_rate;

        let capacity = self.history_l.len();
        let mut hopped = [false; 3];
        for (resolution, hopped) in resolutions.iter_mut().zip(&mut hopped) {
            if let Some(end) = resolution.due_hop(self.samples_written, capacity) {
                resolution.transform(&self.history_l, &self.history_r, end);
                *hopped = true;
            }
        }
        if hopped.contains(&true) {
            // The unsmoothed mid curve as power, for the long-term average.
            let n = self.configured_bins;
            let sources = [0, 1, 2].map(|layer| &resolutions[layer].display_db[CH_MID * n..(CH_MID + 1) * n]);
            blend(&self.segments, &self.weights, sources, &mut self.target_mid_power, true);
        }

        // Third-octave bands: a weighted power sum over the bins of the
        // transform that just ran, then the fast integrator stepped by the
        // duration of this block.
        let seconds = count as f64 / self.configured_rate;
        for layer in 0..3 {
            if hopped[layer] {
                self.third_octave_bank.capture(layer, &resolutions[layer]);
            }
        }
        self.third_octave_bank.advance(seconds);

        self.accumulate_average(seconds, [0, 1, 2].map(|layer| resolutions[layer].has_data));
        if hopped[0] {
            self.update_bands(&resolutions[0]);
            self.update_lowest_strong();
            self.median_stale = true;
        }
        self.peaks_stale = true;
        self.resolutions = Some(resolutions);
    }

    fn read(&mut self) -> SpectrumOutput {
        let rate = if self.configured_rate > 0.0 {
            self.configured_rate
        } else {
            REFERENCE_RATE
        };
        self.configure_if_needed(rate);
        let n = self.configured_bins;

        let resolutions = self.resolutions.take();
        if let Some(resolutions) = resolutions.as_deref() {
            self.apply_smoothing(resolutions);
        }
        self.find_peaks(resolutions.as_deref().map(|r| &r[0]));
        self.resolutions = resolutions;

        let channel = |c: usize| self.shaped(&self.smooth[c * n..(c + 1) * n]);
        // The three mid curves before the blend, for the spectrogram: it delays
        // each one by its own latency and blends after that. Same calibration,
        // release and tilt as `mid`, and `10 log10(sum(weight * 10^(level / 10)))`
        // over the layers gives `mid` back.
        let mid_layers = (0..3)
            .map(|layer| {
                let base = (layer * CHANNEL_COUNT + CH_MID) * n;
                SpectrumLayer {
                    levels_db: self.shaped(&self.layer_smooth[base..base + n]),
                    weights: self.weights[layer].clone(),
                    latency_seconds: self.layer_latency[layer],
                }
            })
            .collect();
        let spectrum = SpectrumReading {
            frequencies: self.frequencies.clone(),
            left: channel(CH_LEFT),
            right: channel(CH_RIGHT),
            mid: channel(CH_MID),
            side: channel(CH_SIDE),
            peak_hold: self.shaped(&self.peak_hold_db),
            average: self.shaped(&self.average_db()),
            mid_layers,
        };
        SpectrumOutput {
            spectrum,
            peak: self.current_peak.clone(),
            bands: self.current_bands,
        }
    }

    /// Starts a new measurement: the long-term average and the peak hold. The
    /// live spectrum and the FFT history are kept, so the display does not blank.
    fn reset(&mut self) {
        self.average_power.fill(0.0);
        self.average_seconds = 0.0;
        self.segment_seconds.fill(0.0);
        self.peak_hold_db.fill(FLOOR_DB);
        self.current_peak = PeakReading::default();
        self.current_bands = BandEnergy::default();
        self.peaks.count = 0;
        self.first_peak_is_broad = false;
        self.peaks_stale = false;
        self.lowest_candidate_hz = 0.0;
        self.lowest_candidate_since = 0.0;
        self.lowest_published_hz = 0.0;
        self.third_octave_bank.reset();
    }

    /// The strongest tonal peaks of mid, strongest first, at most five.
    ///
    /// Every entry is a local maximum **of the mid display curve**, in the
    /// smoothing state the display uses, so a marker drawn at (`frequency_hz`,
    /// `level_db`) sits on the curve. Offset rule: `level_db` is the untilted
    /// level; with a display tilt the curve is at
    /// `level_db + tilt_db_per_octave * log2(frequency_hz / 1000)`. The
    /// frequency is refined inside its display bin from the long FFT, which is
    /// what makes the cents meaningful.
    ///
    /// A peak is tonal when it is over the gate the note readout uses (above
    /// −80 dBFS, 10 dB over the median of the curve), stands
    /// `TONAL_PROMINENCE_DB` over the lowest point on each side within
    /// `TONAL_REACH_OCTAVES`, and is narrower than `TONAL_WIDTH_OCTAVES` at −3 dB.
    ///
    /// When the list is not empty its first entry is `peak`. If `peak` itself
    /// is a broad maximum, that first entry has an empty note name and the
    /// tonal peaks follow it.
    fn top_peaks(&mut self) -> Vec<PeakReading> {
        if self.peaks_stale && self.configured_bins >= 2 {
            let resolutions = self.resolutions.take();
            if let Some(resolutions) = resolutions.as_deref() {
                self.apply_smoothing(resolutions);
            }
            self.find_peaks(resolutions.as_deref().map(|r| &r[0]));
            self.resolutions = resolutions;
        }
        (0..self.peaks.count)
            .map(|i| {
                if i == 0 && self.first_peak_is_broad {
                    return self.current_peak.clone();
                }
                let hz = self.peaks.hz[i];
                let (note_name, cents) = note_for_hz(hz);
                PeakReading {
                    frequency_hz: hz,
                    level_db: self.peaks.level[i].max(FLOOR_DB),
                    note_name,
                    cents,
                }
            })
            .collect()
    }

    /// Lowest frequency that carries sustained content, in whole Hz, never under 20 Hz.
    ///
    /// The lowest frequency at or over 20 Hz where the long-term average of
    /// mid, smoothed over a third of an octave, is within 12 dB of its maximum.
    /// 0 until three seconds have been measured and the answer has held still
    /// for one second, and 0 on silence.
    fn lowest_strong_hz(&mut self) -> f32 {
        self.lowest_published_hz
    }

    /// RMS level per IEC 61260 third-octave band, per channel, in dBFS RMS
    /// where a full-scale sine reads −3.01. `None` until every resolution that
    /// serves a band has transformed once. Bands whose centre is above 0.9 of
    /// Nyquist read the floor.
    fn third_octave(&mut self) -> Option<ThirdOctaveReading> {
        self.third_octave_bank.reading()
    }
}

/// 0 below `centre` minus `half_width` octaves, 1 above it, smooth in between.
fn smoothstep_octaves(f: f32, centre: f32, half_width: f32) -> f32 {
    let t = ((f / centre).log2() + half_width) / (2.0 * half_width);
    let c = t.max(0.0).min(1.0);
    c * c * (3.0 - 2.0 * c)
}

/// Blends one channel of the three resolutions in the power domain.
///
/// Outside the two crossfades a display bin comes from one resolution and is
/// copied. Inside one it is `wa * Pa + wb * Pb`. The two curves agree on noise
/// (the bandwidth ramp sees to that) and on stationary tones. Where they
/// disagree, a transient the short window has and the long one has not yet,
/// power is what adds.
fn blend(segments: &[Segment], weights: &[Vec<f32>; 3], sources: [&[f32]; 3], dst: &mut [f32], as_power: bool) {
    let to_exponent = (std::f64::consts::LN_10 / 10.0) as f32;
    for segment in segments {
        let range = segment.lo..=segment.hi;
        let out = &mut dst[range.clone()];
        let a = &sources[segment.a][range.clone()];
        let Some(b) = segment.b else {
            if as_power {
                for (out, &level) in out.iter_mut().zip(a) {
                    *out = (level * to_exponent).exp().max(FLOOR_POWER);
                }
            } else {
                out.copy_from_slice(a);
            }
            continue;
        };
        let (weight_a, weight_b) = (&weights[segment.a][range.clone()], &weights[b][range.clone()]);
        let b = &sources[b][range];
        for i in 0..out.len() {
            let power = (b[i] * to_exponent).exp() * weight_b[i] + (a[i] * to_exponent).exp() * weight_a[i];
            let power = power.max(FLOOR_POWER);
            out[i] = if as_power { power } else { 10.0 * power.log10() };
        }
    }
}

/// Prominence and width of the local maximum at display bin `i`.
fn is_tonal(s: &[f32], i: usize, bins_per_octave: f32, width_limit_octaves: &[f32]) -> bool {
    let n = s.len();
    let top = s[i];
    let reach = ((TONAL_REACH_OCTAVES * bins_per_octave).round() as usize).max(2);
    let left_min = s[i.saturating_sub(reach)..i].iter().fold(top, |low, &v| low.min(v));
    let right_min = s[i + 1..=(i + reach).min(n - 1)].iter().fold(top, |low, &v| low.min(v));
    if !(top - left_min.max(right_min) >= TONAL_PROMINENCE_DB) {
        return false;
    }

    // −3 dB width, with the crossing interpolated between display bins.
    let edge = top - 3.0;
    let mut l = i;
    while l > 0 && s[l] > edge && i - l <= reach {
        l -= 1;
    }
    let mut r = i;
    while r < n - 1 && s[r] > edge && r - i <= reach {
        r += 1;
    }
    if !(s[l] <= edge && s[r] <= edge) {
        return false;
    }
    let x_left = l as f32 + (edge - s[l]) / (s[l + 1] - s[l]).max(1e-6);
    let x_right = r as f32 - (edge - s[r]) / (s[r - 1] - s[r]).max(1e-6);
    (x_right - x_left) / bins_per_octave < width_limit_octaves[i]
}

/// The frequency of the peak in display bin `i`, to a fraction of a bin of the long FFT.
///
/// A display bin is 0.76% wide, 13 cents: too coarse for a tuner. The long FFT
/// has the finest bins at every frequency, so the strongest of its bins inside
/// the display bin is found and interpolated (the Hann main lobe is close to a
/// parabola in dB, which lands inside a cent). The answer never leaves the
/// display bin by more than one bin, so the marker stays on the peak it belongs
/// to while the long window is still catching up with a change.
fn refined_frequency(frequencies: &[f32], low: Option<&SpectrumResolution>, bins_per_octave: f32, i: usize) -> f32 {
    let f = frequencies[i];
    let Some(low) = low.filter(|low| low.has_data) else {
        return f;
    };
    let pw = &low.power[CH_MID * low.half..(CH_MID + 1) * low.half];
    let max_bin = low.half as isize - 2;
    let step = (1.0 / bins_per_octave).exp2();
    let k_lo = ((f / step / low.df).floor() as isize - 1).max(2);
    let k_hi = ((f * step / low.df).ceil() as isize + 1).min(max_bin);
    if k_hi < k_lo {
        return f;
    }
    let (mut k, mut value) = (k_lo as usize, pw[k_lo as usize]);
    for (j, &p) in pw.iter().enumerate().take(k_hi as usize + 1).skip(k_lo as usize) {
        if p > value {
            (k, value) = (j, p);
        }
    }
    if !(value > 0.0) {
        return f;
    }
    let db = |p: f32| 10.0 * p.max(1e-30).log10();
    let (y0, y1, y2) = (db(pw[k - 1]), db(pw[k]), db(pw[k + 1]));
    let denominator = y0 - 2.0 * y1 + y2;
    let delta = if denominator.abs() > 1e-9 {
        (0.5 * (y0 - y2) / denominator).max(-0.5).min(0.5)
    } else {
        0.0
    };
    let hz = (k as f32 + delta) * low.df;
    if (hz / f).log2().abs() * bins_per_octave <= 1.5 {
        hz
    } else {
        f
    }
}

/// Median by histogram: one pass, no allocation, 1 dB buckets from −120 to +20 dB.
/// Good enough for a 10 dB gate.
fn median_db(values: &[f32], histogram: &mut [u32; HISTOGRAM_BUCKETS]) -> f32 {
    if values.is_empty() {
        return FLOOR_DB;
    }
    histogram.fill(0);
    for &value in values {
        let bucket = ((value + 120.0).round() as isize).clamp(0, HISTOGRAM_BUCKETS as isize - 1);
        histogram[bucket as usize] += 1;
    }
    let mut seen = 0usize;
    for (bucket, &count) in histogram.iter().enumerate() {
        seen += count as usize;
        if seen * 2 >= values.len() {
            return bucket as f32 - 120.0;
        }
    }
    FLOOR_DB
}
