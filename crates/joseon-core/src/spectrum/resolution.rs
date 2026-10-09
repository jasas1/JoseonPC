//! One FFT resolution of the multi-resolution spectrum analyzer.
//!
//! It owns the window, the FFT plan, the per-channel power spectra and the map
//! from FFT bins to display bins. The analyzer owns three of these (lows, mids,
//! highs) and crossfades their display output.
//!
//! Port source: `Sources/JoseonCore/Spectrum/SpectrumResolution.swift`. The
//! constants, the order of operations and the index ranges are the same. vDSP
//! calls are plain loops here. The FFT is `realfft`, which returns the
//! mathematical DFT; vDSP's packed real FFT returns twice that, so the scale
//! constant carries the factor 4.
//!
//! ## Window choice: periodic Hann
//!
//! Hann, not Blackman-Harris. Two reasons, both measurable:
//!
//! 1. **Resolution.** Hann's main lobe is 4 bins wide null to null;
//!    Blackman-Harris (4-term) is 8. At 48 kHz the 32768-point low FFT has
//!    1.465 Hz bins, so 30 Hz and 40 Hz sit 6.8 bins apart. Hann leaves them
//!    cleanly separated.
//! 2. **Level accuracy.** A Hann-windowed tone puts all of its energy in the 3
//!    bins around it when on-bin, and in 5 bins for any sub-bin offset. So the
//!    power sum over ±2 bins, normalised by `4 / (N * sum(w^2))`, gives the
//!    tone's peak amplitude with under 0.01 dB of ripple, at any frequency and
//!    any FFT size.
//!
//! The price is Hann's −31 dB first sidelobe. It is a known limit.
//!
//! ## Two curves, added in the power domain
//!
//! A sliding power sum written to every bin gives the right level for noise,
//! but it turns a pure tone into a flat-topped table as wide as the sum. So
//! each transform builds two curves:
//!
//! 1. **Noise curve.** Tonal main lobes are lifted out of the power spectrum
//!    and replaced by the local noise level. Then a bandwidth-matched sliding
//!    sum runs over the result. The bandwidth is a continuous function of
//!    frequency (see `update_map`), so the displayed noise floor has no steps.
//! 2. **Tone lobes.** Every tonal peak is measured (frequency to a fraction of
//!    a bin, level from the calibrated 5-bin sum less the noise under it) and
//!    then drawn: the Hann main lobe, evaluated at the display bins and added
//!    to the noise curve in the power domain.
//!
//! A noise bin three bins from a real tone is a local maximum whose 5-bin sum
//! still holds the tone's skirt bin, so it can read 20 dB over its own context.
//! `detect_tones` therefore requires the peak bin to carry its share of the
//! 5-bin sum. A real Hann peak always does (48% or more). A bin beside one
//! never does.

use crate::contracts::SpectrumReading;
use realfft::num_complex::Complex;
use realfft::{RealFftPlanner, RealToComplex};
use std::sync::Arc;

/// Channel order inside every per-channel buffer: left, right, mid, side.
pub(crate) const CHANNEL_COUNT: usize = 4;
pub(crate) const CH_LEFT: usize = 0;
pub(crate) const CH_RIGHT: usize = 1;
pub(crate) const CH_MID: usize = 2;
pub(crate) const CH_SIDE: usize = 3;

/// Bins with no FFT data (above Nyquist, or a display bin this resolution does not serve).
const FLOOR_DB: f32 = SpectrumReading::FLOOR_DB;

/// A peak is tonal when its 5-bin power is this many times the 5-bin noise
/// power beside it (14 dB). The test and the fill use the same unbiased noise
/// estimate. A tone that misses the gate stays on the noise curve, where the
/// base sum is three bins wide, so it still reads as a narrow bump.
const TONE_THRESHOLD: f32 = 25.0;
/// The second gate, on the power spectrum averaged over the last transforms
/// (`TONE_AVERAGE_ALPHA`), against the raw second-smallest block mean with no
/// `CONTEXT_GAIN`. 7.4 dB to turn on, and 6 dB to stay on where the bin, or one
/// beside it, was tonal in the previous transform.
///
/// The 14 dB gate is the fast path: it decides from one transform. A quiet
/// steady tone does not reach it in every transform, so it would draw as
/// dashes. The average of 8 transforms takes the variance out of both sides.
/// A tone that stops is released as the average decays, in about 9 transforms.
const TONE_AVERAGE_ON_THRESHOLD: f32 = 5.5;
const TONE_AVERAGE_OFF_THRESHOLD: f32 = 4.0;
const TONE_AVERAGE_ALPHA: f32 = 0.125;
/// Transforms before the averaged gate decides: two time constants.
const TONE_AVERAGE_WARMUP: usize = 16;
/// A channel draws a shared tone as a lobe only where it carries it: its own
/// 5-bin power over its own noise passes the 14 dB gate in this transform, or
/// 3.5 dB on its averaged spectrum. A channel without the tone keeps its noise
/// curve there.
const TONE_CHANNEL_THRESHOLD: f32 = 2.25;
/// Power under which a transform is digital silence: the tonal memory is
/// cleared, so a tone that comes back after a gap has to pass a gate again.
const TONE_SILENCE_POWER: f32 = 1e-20;
/// 3-bin blocks each side that make the noise estimate, starting 3 bins from
/// the peak (outside the main lobe). The estimate is the *second smallest* of
/// the six, so four of the six blocks can hold a neighbour partial of a chord.
const TONE_CONTEXT_BLOCKS: isize = 3;
/// The second smallest of six block means reads low on plain noise: 0.525 of
/// the true level, measured on Hann-windowed white noise. Taken out again here,
/// so the gate and the fill are unbiased.
const CONTEXT_GAIN: f32 = 1.0 / 0.525;
/// Half-width of the main lobe that is lifted out of the noise curve.
const LOBE_HALF: usize = 2;
/// A Hann peak bin holds 48% to 67% of its own 5-bin sum. A bin that holds less
/// than this is a bump on the skirt of a louder neighbour.
const TONE_PEAK_SHARE: f32 = 0.3;
/// Hann main lobe table: entries per bin off centre, from 0 to the first null at 2.
const LOBE_STEPS: usize = 64;
/// `10 * log10(1 + 10^(-x / 10))` table for x = 0 ... 40 dB: entries per dB.
const SUM_STEPS: usize = 8;
const SUM_RANGE_DB: usize = 40;
/// How far outside the display's own bin range a tone is still looked for, on
/// top of the widest sliding sum: its lobe can reach back in.
const TONE_REACH: usize = 4;
/// The narrowest sum the noise curve uses, in bins each side. Three bins in
/// total: a tone has its own curve, so the noise curve can keep the finest
/// resolution the FFT has.
pub(crate) const BASE_TAPS_HALF: usize = 1;
/// A carried sliding sum is rebuilt from scratch this often. Power spectra span
/// 200 dB, so a tone subtracted out of a running sum leaves a residue that
/// single precision cannot cancel.
const CARRY_RESYNC_BINS: usize = 32;
/// Width of the noise smoothing kernel, in octaves from end to end.
pub(crate) const SMOOTH_OCTAVES: f32 = 1.0 / 6.0;
/// Cap on the kernel half-width, so the scratch buffer is always big enough.
pub(crate) const MAX_SMOOTH_HALF_WIDTH: usize = 48;

/// Display map markers in `map_start`.
const MAP_NONE: i32 = -1;
const MAP_INTERP: i32 = -2;
const MAP_FADE: i32 = -3;

/// A tonal peak of the last transform, for tests.
#[cfg(test)]
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct DetectedTone {
    pub bin: usize,
    pub offset: f32,
    pub power_db: f32,
}

pub(crate) struct SpectrumResolution {
    /// FFT length in samples.
    pub size: usize,
    /// Usable spectrum length; bin k is `k * df` Hz for k in 1...half-1.
    pub half: usize,
    /// Samples between transforms.
    pub hop: usize,
    /// Hz per FFT bin.
    pub df: f32,

    fft: Arc<dyn RealToComplex<f32>>,
    fft_scratch: Vec<Complex<f32>>,
    window: Vec<f32>,
    work: Vec<f32>,
    /// Left and right complex spectra, `half + 1` bins each.
    spectrum: [Vec<Complex<f32>>; 2],
    /// Calibrated power per FFT bin, `channel * half + bin`: a full-scale
    /// sine's main lobe sums to 1.0 here.
    pub power: Vec<f32>,

    // One-channel scratch. Each channel is built, mapped and finished before the next starts.
    /// Power with the tonal main lobes taken out.
    detoned: Vec<f32>,
    /// Bandwidth-matched sliding sum of `detoned`.
    noise_p: Vec<f32>,
    /// `10 * log10(noise_p)`.
    noise_db: Vec<f32>,
    /// Shared tonal peak bins of this transform.
    tone_bin: Vec<usize>,
    /// Per FFT bin: any channel found a tone here.
    tone_flag: Vec<u8>,
    /// Per tone, this channel: calibrated power less the noise under it.
    tone_power: Vec<f32>,
    /// Per tone, this channel: position of the tone in bins from its peak bin.
    tone_offset: Vec<f32>,
    tone_count: usize,
    /// Per channel and FFT bin: the power spectrum averaged over the last
    /// transforms, the memory of the tonal gate.
    tone_average: Vec<f32>,
    /// Transforms in `tone_average` so far (0: none yet).
    tone_average_count: usize,
    /// Per FFT bin: the tone flag of the previous transform.
    tone_previous: Vec<u8>,
    /// Per FFT bin: what the averaged gate decided when it last ran.
    tone_average_flag: Vec<u8>,
    /// `peak_rise[k] = min(p[k] - p[k+1], p[k] - p[k-1])`, positive at a local maximum.
    peak_rise: Vec<f32>,
    /// `sum3[j] = p[j] + p[j+1] + p[j+2]`.
    sum3: Vec<f32>,
    /// `sum5[k] = p[k-2] + ... + p[k+2]`.
    sum5: Vec<f32>,
    /// The averaged gate runs every this many transforms. Its spectrum moves
    /// with a time constant of 8 transforms, so a decision on every transform
    /// finds what the last decision found. About 16 decisions a second.
    pub tone_average_every: usize,

    /// Hann main lobe in dB, `LOBE_STEPS` entries per bin off centre.
    lobe_db: Vec<f32>,
    /// Adds two levels in the power domain without leaving dB.
    sum_db: Vec<f32>,
    /// Per FFT bin: the bin width of the drawn lobe as a multiple of this FFT's
    /// own bin width. Outside the crossfades that is 1. Inside a crossfade the
    /// two resolutions that are blended draw the *same* lobe, whose width moves
    /// from the finer FFT's to the coarser one's in step with the blend weights.
    lobe_scale: Vec<f32>,

    /// Level in dB per display bin, `channel * display_bins + bin`. `FLOOR_DB`
    /// where this resolution has nothing.
    pub display_db: Vec<f32>,
    pub display_bins: usize,

    // Display map. Three contiguous index ranges, all inside served_lo...served_hi:
    //   fade   : the display bin sits below the first usable FFT bin -> fade towards the floor
    //   interp : fewer than two FFT bins in the display bin -> interpolate
    //   mean   : two or more FFT bins -> power mean over them
    // An empty range has lo > hi.
    /// Mean range: first FFT bin of the display bin. Otherwise a `MAP_*` marker.
    map_start: Vec<i32>,
    /// Mean range: last FFT bin.
    map_end: Vec<i32>,
    /// Fractional FFT-bin position of the display bin.
    map_pos: Vec<f32>,
    fade: (isize, isize),
    interp: (isize, isize),
    mean: (isize, isize),
    served: (isize, isize),
    smooth_work: Vec<f32>,
    /// Half-width of the noise smoothing kernel in display bins.
    smooth_half_width: usize,
    /// Geometric half-width of a display bin: it spans f / half_step ... f * half_step.
    display_half_step: f32,
    /// Display index of frequency f is `log_f0_scale * ln(f) + log_f0_offset`.
    log_f0_scale: f32,
    log_f0_offset: f32,
    bin_lo: usize,
    bin_hi: usize,
    /// Highest FFT bin the dB conversion has to cover: only the interpolating
    /// display bins read it.
    db_hi: usize,
    /// Bins the display path reads, including the reach of the widest sliding sum.
    used_lo: usize,
    used_hi: usize,
    /// Set for the resolution that also feeds the peak readout and the band
    /// energy: that channel's power spectrum covers the whole band.
    pub full_spectrum_channel: Option<usize>,
    /// Extra FFT-bin range the **left and right** power spectra have to cover,
    /// on top of what the display reads. The third-octave bank sums raw power
    /// over band edges that reach well below the display range of the short FFTs.
    pub extra_power_range: Option<(usize, usize)>,

    /// Half-width, in FFT bins, of the power sum at each bin, with a fractional
    /// part. Integer taps would step the displayed noise floor by 1.5 dB where
    /// the count ticks. The fractional edge weight makes the analysis bandwidth
    /// a continuous function of frequency.
    taps_half: Vec<usize>,
    taps_frac: Vec<f32>,
    /// Bins with a wider sum than the base. Empty when lo > hi.
    wide: (usize, usize),
    widest_taps_half: usize,

    /// Multiplies `re^2 + im^2` of the DFT into squared sine amplitude.
    ///
    /// Parseval over the one-sided spectrum gives `sum|X|^2 = A^2 * N * sum(w^2) / 4`
    /// for a sine of peak amplitude A. So `A^2 = 4 * sum|X|^2 / (N * sum(w^2))`,
    /// summed over the main lobe.
    power_scale: f32,

    /// Absolute sample index of the right edge of the last transform.
    pub hop_end: usize,
    pub has_data: bool,
}

impl SpectrumResolution {
    pub fn new(size: usize, hop: usize, sample_rate: f64) -> Option<Self> {
        if !(size >= 64 && size.is_power_of_two() && hop > 0 && sample_rate > 0.0) {
            return None;
        }
        let half = size / 2;
        let fft = RealFftPlanner::<f32>::new().plan_fft_forward(size);
        let fft_scratch = fft.make_scratch_vec();

        let lobe_db = (0..2 * LOBE_STEPS + 2)
            .map(|i| {
                let u = i as f32 / LOBE_STEPS as f32;
                let mut h = 1.0f32;
                if (u - 1.0).abs() < 1e-4 {
                    h = 0.5;
                } else if u > 1e-4 {
                    h = (std::f32::consts::PI * u).sin() / (std::f32::consts::PI * u) / (1.0 - u * u);
                }
                if u >= 2.0 {
                    -200.0
                } else {
                    (20.0 * h.abs().max(1e-10).log10()).max(-200.0)
                }
            })
            .collect();
        let sum_db = (0..SUM_STEPS * SUM_RANGE_DB + 2)
            .map(|i| 10.0 * (1.0 + 10f32.powf(-(i as f32) / SUM_STEPS as f32 / 10.0)).log10())
            .collect();

        // Periodic Hann. sum(w^2) = 3N/8 exactly, but compute it so the constant
        // follows the window if the window ever changes.
        let mut sum_squares = 0.0f32;
        let window: Vec<f32> = (0..size)
            .map(|i| {
                let w = 0.5 - 0.5 * (2.0 * std::f32::consts::PI * i as f32 / size as f32).cos();
                sum_squares += w * w;
                w
            })
            .collect();

        Some(Self {
            size,
            half,
            hop,
            df: (sample_rate / size as f64) as f32,
            fft,
            fft_scratch,
            window,
            work: vec![0.0; size],
            spectrum: [
                vec![Complex::new(0.0, 0.0); half + 1],
                vec![Complex::new(0.0, 0.0); half + 1],
            ],
            power: vec![0.0; CHANNEL_COUNT * half],
            detoned: vec![0.0; half],
            noise_p: vec![0.0; half],
            noise_db: vec![0.0; half],
            tone_bin: vec![0; half / 2 + 1],
            tone_flag: vec![0; half],
            tone_power: vec![0.0; half / 2 + 1],
            tone_offset: vec![0.0; half / 2 + 1],
            tone_count: 0,
            tone_average: vec![0.0; CHANNEL_COUNT * half],
            tone_average_count: 0,
            tone_previous: vec![0; half],
            tone_average_flag: vec![0; half],
            peak_rise: vec![0.0; half],
            sum3: vec![0.0; half],
            sum5: vec![0.0; half],
            tone_average_every: ((sample_rate / hop as f64 / 16.0).round() as usize).max(1),
            lobe_db,
            sum_db,
            lobe_scale: vec![1.0; half],
            display_db: Vec::new(),
            display_bins: 0,
            map_start: Vec::new(),
            map_end: Vec::new(),
            map_pos: Vec::new(),
            fade: (1, 0),
            interp: (1, 0),
            mean: (1, 0),
            served: (1, 0),
            smooth_work: Vec::new(),
            smooth_half_width: 0,
            display_half_step: 1.0,
            log_f0_scale: 1.0,
            log_f0_offset: 0.0,
            bin_lo: 1,
            bin_hi: 1,
            db_hi: 1,
            used_lo: 1,
            used_hi: 1,
            full_spectrum_channel: None,
            extra_power_range: None,
            taps_half: vec![0; half],
            taps_frac: vec![0.0; half],
            wide: (1, 0),
            widest_taps_half: 2,
            power_scale: 4.0 / (size as f32 * sum_squares),
            hop_end: 0,
            has_data: false,
        })
    }

    // MARK: Display map

    /// Rebuilds the FFT-bin -> display-bin map. Called only when the sample
    /// rate or the display grid changes, never from `process` in the steady state.
    ///
    /// `weight` is the crossfade weight of this resolution per display bin.
    /// Bins with zero weight are skipped, which keeps the long FFT cheap: the
    /// 32768-point transform is mapped over its ~200 lowest bins only.
    pub fn update_map(
        &mut self,
        frequencies: &[f32],
        bin_ratio: f32,
        weight: &[f32],
        target_bandwidth_hz: &dyn Fn(f32) -> f32,
        tone_bin_hz: &dyn Fn(f32) -> f32,
    ) {
        let n = frequencies.len();
        let half = self.half;
        let df = self.df;
        // A new map serves other bins: what the old one remembered about them is stale.
        self.tone_average_count = 0;
        self.tone_previous.fill(0);
        self.tone_average_flag.fill(0);
        if self.display_bins != n {
            self.display_bins = n;
            self.display_db = vec![FLOOR_DB; CHANNEL_COUNT * n];
            self.map_start = vec![MAP_NONE; n];
            self.map_end = vec![0; n];
            self.map_pos = vec![0.0; n];
            self.smooth_work = vec![0.0; n + MAX_SMOOTH_HALF_WIDTH + 1];
        }
        self.display_db.fill(FLOOR_DB);

        let half_step = bin_ratio.sqrt(); // geometric half-width of a display bin
        self.display_half_step = half_step;
        self.log_f0_scale = 1.0 / bin_ratio.ln();
        self.log_f0_offset = -frequencies[0].ln() * self.log_f0_scale;
        let max_bin = (half - 1) as isize; // bin 0 packs DC and Nyquist; never use it
                                           // Width of a display bin in FFT bins. Continuous in f, so the three
                                           // passes cover contiguous index ranges.
        let span_per_hz = (half_step - 1.0 / half_step) / df;

        let empty = (n as isize, -1isize);
        let (mut fade, mut interp, mut mean) = (empty, empty, empty);
        let widen = |range: &mut (isize, isize), i: usize| {
            range.0 = range.0.min(i as isize);
            range.1 = range.1.max(i as isize);
        };
        let mut lo = max_bin;
        let mut hi = 1isize;
        let mut db_top = 1isize;
        let mut any = false;

        for i in 0..n {
            self.map_start[i] = MAP_NONE;
            if !(weight[i] > 0.0) {
                continue;
            }
            let f = frequencies[i];
            let x = f / df;
            if x < 1.0 {
                // Below the first usable FFT bin. Fade towards the floor.
                self.map_start[i] = MAP_FADE;
                self.map_pos[i] = x;
                widen(&mut fade, i);
                lo = lo.min(1);
                hi = hi.max(3);
                any = true;
            } else if x > max_bin as f32 {
                continue; // above Nyquist: nothing to show
            } else if f * span_per_hz >= 2.0 {
                // Two or more FFT bins in this display bin: power mean over them.
                // A mean, not a max: a max is biased upward by an amount that
                // depends on how many bins fall in the display bin, and that
                // count steps with frequency. Tonal peaks keep their height
                // through the tone curve.
                let k0 = ((f / half_step / df).round() as isize).max(1);
                let k1 = ((f * half_step / df).round() as isize).max(k0 + 1).min(max_bin);
                self.map_start[i] = k0 as i32;
                self.map_end[i] = k1 as i32;
                self.map_pos[i] = x;
                widen(&mut mean, i);
                lo = lo.min(k0 - 2);
                hi = hi.max(k1 + 2);
                any = true;
            } else {
                // Fewer than two FFT bins inside: interpolate in the dB domain,
                // so the low end is a smooth curve and not a staircase.
                self.map_start[i] = MAP_INTERP;
                self.map_pos[i] = x;
                widen(&mut interp, i);
                let c = x as isize;
                lo = lo.min(c - 2);
                hi = hi.max(c + 2);
                db_top = db_top.max(c + 2);
                any = true;
            }
        }
        self.fade = fade;
        self.interp = interp;
        self.mean = mean;
        self.served = (fade.0.min(interp.0).min(mean.0), fade.1.max(interp.1).max(mean.1));
        let bins_per_octave = 1.0 / bin_ratio.log2();
        self.smooth_half_width =
            ((SMOOTH_OCTAVES * bins_per_octave / 2.0).round() as usize).clamp(1, MAX_SMOOTH_HALF_WIDTH);

        self.bin_lo = if any { lo.max(1) as usize } else { 1 };
        self.bin_hi = if any { hi.min(max_bin) as usize } else { 1 };
        if self.bin_hi < self.bin_lo {
            self.bin_hi = self.bin_lo;
        }
        self.db_hi = (db_top.max(self.bin_lo as isize) as usize).min(self.bin_hi);

        // Noise-bandwidth match. A tone keeps its level however wide the sum is
        // (it is drawn from the tone curve), but broadband content scales with
        // the summed bandwidth. The finer FFT is widened inside a crossfade, so
        // pink noise does not step up ~6 dB where the analyzer changes FFT size.
        self.lobe_scale.fill(1.0);
        self.wide = (self.bin_hi + 1, self.bin_lo - 1);
        self.widest_taps_half = BASE_TAPS_HALF;
        for k in self.bin_lo..=self.bin_hi {
            let hz = k as f32 * df;
            let wanted = target_bandwidth_hz(hz) / df;
            let exact = ((wanted - 1.0) / 2.0).max(BASE_TAPS_HALF as f32);
            let w = (exact as usize).min(half / 4);
            self.taps_half[k] = w;
            self.taps_frac[k] = if w < half / 4 { exact - w as f32 } else { 0.0 };
            self.widest_taps_half = self.widest_taps_half.max(w + 1);
            if w > BASE_TAPS_HALF || self.taps_frac[k] > 0.0 {
                self.wide.0 = self.wide.0.min(k);
                self.wide.1 = self.wide.1.max(k);
            }
        }

        // The peak search reads three 3-bin context blocks past the widest
        // sliding sum, so the power spectrum has to be valid that far outside
        // the display's own range.
        let reach = self.widest_taps_half + TONE_REACH + 3 * TONE_CONTEXT_BLOCKS as usize + 3;
        self.used_lo = self.bin_lo.saturating_sub(reach).max(1);
        self.used_hi = (self.bin_hi + reach).min(half - 1);
        for k in self.used_lo..=self.used_hi {
            self.lobe_scale[k] = (tone_bin_hz(k as f32 * df) / df).max(0.125);
        }
    }

    // MARK: Transform

    /// Advances the hop clock. Returns the sample index to transform at, or
    /// `None` when nothing is due.
    ///
    /// Only the newest due hop is run. A big block therefore costs one
    /// transform, not many, which bounds the cost of `process` when the caller
    /// is late.
    pub fn due_hop(&mut self, samples_written: usize, history_capacity: usize) -> Option<usize> {
        let behind = samples_written - self.hop_end;
        if behind < self.hop {
            return None;
        }
        self.hop_end += (behind / self.hop) * self.hop;
        if self.hop_end < self.size {
            return None;
        }
        // The window must still be inside the history ring.
        if samples_written - self.hop_end > history_capacity - self.size {
            self.hop_end = samples_written;
        }
        Some(self.hop_end)
    }

    /// Window, transform, derive mid/side, calibrate, and map to display bins.
    /// `history_*` are rings whose length is a power of two; `end` is the
    /// absolute sample index of the right edge of the window.
    pub fn transform(&mut self, history_l: &[f32], history_r: &[f32], end: usize) {
        let mask = history_l.len() - 1;
        let start = end.wrapping_sub(self.size) & mask;
        self.forward_fft(history_l, start, CH_LEFT);
        self.forward_fft(history_r, start, CH_RIGHT);
        for c in 0..CHANNEL_COUNT {
            self.compute_power(c);
        }
        self.detect_tones();
        for c in 0..CHANNEL_COUNT {
            self.remove_tones(c);
            self.sliding_power_sum();
            self.noise_to_decibels();
            self.map_channel(c);
        }
        self.has_data = true;
    }

    fn forward_fft(&mut self, history: &[f32], start: usize, channel: usize) {
        // Window straight out of the ring buffer.
        let size = self.size;
        let first = (history.len() - start).min(size);
        for ((out, sample), w) in self.work[..first]
            .iter_mut()
            .zip(&history[start..start + first])
            .zip(&self.window)
        {
            *out = sample * w;
        }
        for ((out, sample), w) in self.work[first..].iter_mut().zip(history).zip(&self.window[first..]) {
            *out = sample * w;
        }
        // The lengths are fixed at construction, so this cannot fail.
        self.fft
            .process_with_scratch(&mut self.work, &mut self.spectrum[channel], &mut self.fft_scratch)
            .expect("FFT buffer lengths are fixed at construction");
    }

    /// Bins of the spectrum a channel needs this frame. Everything outside the
    /// display's reach is skipped, except for the one channel that also feeds
    /// the peak readout and the band energy.
    fn spectrum_range(&self, c: usize) -> (usize, usize) {
        if self.full_spectrum_channel == Some(c) {
            return (1, self.half - 1);
        }
        if c == CH_LEFT || c == CH_RIGHT {
            if let Some((lo, hi)) = self.extra_power_range {
                return (self.used_lo.min(lo), self.used_hi.max(hi));
            }
        }
        (self.used_lo, self.used_hi)
    }

    /// The FFT is linear, so mid = (L+R)/2 and side = (L-R)/2 are formed on the
    /// complex spectra: two transforms per hop, not four. Only over the bins
    /// something reads. Bin 0 is never computed and stays 0.
    fn compute_power(&mut self, c: usize) {
        let (lo, hi) = self.spectrum_range(c);
        if hi < lo {
            return;
        }
        let scale = self.power_scale;
        let [left, right] = &self.spectrum;
        let out = &mut self.power[c * self.half + lo..=c * self.half + hi];
        let (left, right) = (&left[lo..=hi], &right[lo..=hi]);
        match c {
            CH_LEFT => out.iter_mut().zip(left).for_each(|(p, l)| *p = l.norm_sqr() * scale),
            CH_RIGHT => out.iter_mut().zip(right).for_each(|(p, r)| *p = r.norm_sqr() * scale),
            CH_MID => {
                for ((p, l), r) in out.iter_mut().zip(left).zip(right) {
                    *p = ((l + r) * 0.5).norm_sqr() * scale;
                }
            }
            _ => {
                for ((p, l), r) in out.iter_mut().zip(left).zip(right) {
                    *p = ((l - r) * 0.5).norm_sqr() * scale;
                }
            }
        }
    }

    /// Lists the tonal peaks of this transform: the *union* over the four channels.
    ///
    /// A tone is a property of the signal, not of a channel. A quiet one-sided
    /// tone sits about 3 dB closer to the noise in mid than in the channel that
    /// carries it, which is enough to pass the gate in one and miss it in the
    /// other. One decision for all four keeps the curves consistent.
    fn detect_tones(&mut self) {
        self.tone_count = 0;
        let half = self.half;
        let reach = self.widest_taps_half + TONE_REACH;
        let first = self.bin_lo.saturating_sub(reach).max(3);
        let last = (self.bin_hi + reach).min(half - 4);
        if last < first {
            return;
        }
        let Self {
            power,
            tone_average,
            tone_flag: flags,
            tone_previous: previous,
            tone_average_flag: average_flags,
            peak_rise,
            sum3,
            sum5,
            ..
        } = self;
        flags[first..=last].fill(0);

        // The averaged spectra. The context blocks read 11 bins each side of a
        // candidate. The first time, the memory is the same as one transform.
        let avg_lo = first.saturating_sub(11);
        let avg_hi = (last + 11).min(half - 1);
        let alpha = if self.tone_average_count > 0 {
            TONE_AVERAGE_ALPHA
        } else {
            1.0
        };
        let rest = 1.0 - alpha;
        let mut any_signal = false;
        for c in 0..CHANNEL_COUNT {
            let range = c * half + avg_lo..=c * half + avg_hi;
            for (average, &p) in tone_average[range.clone()].iter_mut().zip(&power[range]) {
                *average = *average * rest + p * alpha;
                any_signal |= p >= TONE_SILENCE_POWER;
            }
        }
        self.tone_average_count += 1;
        // Digital silence in every channel: forget the tones. The averaged
        // spectrum keeps its shape as it decays, and the gate reads ratios, so
        // without this a tone that stopped into silence would stay tonal until
        // the average underflowed. The memory starts over, warm-up included.
        if !any_signal {
            previous[first..=last].fill(0);
            average_flags[first..=last].fill(0);
            for c in 0..CHANNEL_COUNT {
                tone_average[c * half + avg_lo..=c * half + avg_hi].fill(0.0);
            }
            self.tone_average_count = 0;
        }
        let averaged = self.tone_average_count >= TONE_AVERAGE_WARMUP;
        let decide = averaged && self.tone_average_count.is_multiple_of(self.tone_average_every);
        if decide {
            average_flags[first..=last].fill(0);
        }

        // One pass per channel and spectrum, ORed into a per-bin flag.
        for c in 0..CHANNEL_COUNT {
            // The fast path: one transform, the 14 dB gate.
            let pw = &power[c * half..(c + 1) * half];
            mark_local_maxima(pw, first, last, peak_rise);
            let (block_lo, block_hi) = sliding_sums(pw, avg_lo, avg_hi, sum3, sum5);
            let gate = |tone: f32, block: f32, corrected: bool, _: usize| {
                tone > TONE_THRESHOLD * 5.0 / 3.0 * if corrected { block * CONTEXT_GAIN } else { block }
            };
            flag_peaks(
                pw,
                peak_rise,
                sum3,
                sum5,
                (first, last),
                (block_lo, block_hi),
                flags,
                gate,
            );

            // The memory: the averaged spectrum, the lower gate, and the lower still to stay on.
            if !decide {
                continue;
            }
            let avg = &tone_average[c * half..(c + 1) * half];
            mark_local_maxima(avg, first, last, peak_rise);
            let (block_lo, block_hi) = sliding_sums(avg, avg_lo, avg_hi, sum3, sum5);
            let previous = &*previous;
            let gate = |tone: f32, block: f32, _: bool, k: usize| {
                let noise = 5.0 * block / 3.0;
                let was_tonal = previous[k - 1] | previous[k] | previous[k + 1] != 0;
                (tone > TONE_AVERAGE_ON_THRESHOLD * noise) | ((tone > TONE_AVERAGE_OFF_THRESHOLD * noise) & was_tonal)
            };
            flag_peaks(
                avg,
                peak_rise,
                sum3,
                sum5,
                (first, last),
                (block_lo, block_hi),
                average_flags,
                gate,
            );
        }
        // What the averaged gate holds, this transform or the last time it ran.
        if averaged {
            for (flag, held) in flags[first..=last].iter_mut().zip(&average_flags[first..=last]) {
                *flag |= held;
            }
        }
        previous[first..=last].copy_from_slice(&flags[first..=last]);

        let mut k = first;
        while k <= last {
            if flags[k] != 0 && self.tone_count < self.tone_bin.len() {
                self.tone_bin[self.tone_count] = k;
                self.tone_count += 1;
                k += 2;
            } else {
                k += 1;
            }
        }
    }

    /// Test hook: the tonal peak bins of the last transform, and what the last
    /// channel (side) drew for them.
    #[cfg(test)]
    pub fn detected_tones(&self) -> Vec<DetectedTone> {
        (0..self.tone_count)
            .map(|t| DetectedTone {
                bin: self.tone_bin[t],
                offset: self.tone_offset[t],
                power_db: 10.0 * self.tone_power[t].max(1e-30).log10(),
            })
            .collect()
    }

    /// Writes `detoned` for one channel: its power with every shared tonal main
    /// lobe replaced by its own local noise level. Records the channel's own
    /// calibrated level for each tone.
    fn remove_tones(&mut self, c: usize) {
        let half = self.half;
        let pw = &self.power[c * half..(c + 1) * half];
        let q = &mut self.detoned;
        q[self.used_lo..=self.used_hi].copy_from_slice(&pw[self.used_lo..=self.used_hi]);
        if self.tone_count == 0 {
            return;
        }
        let avg = &self.tone_average[c * half..(c + 1) * half];
        let steady = self.tone_average_count >= TONE_AVERAGE_WARMUP;
        let sum5 = |p: &[f32], k: usize| p[k - 2] + p[k - 1] + p[k] + p[k + 1] + p[k + 2];
        for t in 0..self.tone_count {
            let k = self.tone_bin[t];
            let fill = context_noise(pw, k).unwrap_or(0.0);
            let sum = sum5(pw, k);
            // Does this channel carry the tone? See `TONE_CHANNEL_THRESHOLD`.
            if steady && sum <= TONE_THRESHOLD * 5.0 * fill {
                let average_block = context_block(avg, k).map_or(0.0, |(block, _)| block);
                if sum5(avg, k) <= TONE_CHANNEL_THRESHOLD * 5.0 * average_block {
                    self.tone_power[t] = 0.0;
                    self.tone_offset[t] = 0.0;
                    continue;
                }
            }
            // The noise under the lobe stays on the noise curve, so it is not counted twice.
            self.tone_power[t] = (sum - 5.0 * fill).max(0.0);
            // Where the tone sits between bins. For a Hann window the ratio of
            // the two largest bin amplitudes gives the offset exactly:
            // r = |X[k±1]| / |X[k]|, d = (2r - 1) / (r + 1).
            let (centre, left, right) = (pw[k], pw[k - 1], pw[k + 1]);
            let mut offset = 0.0f32;
            if centre > 0.0 {
                let ratio = (left.max(right) / centre).sqrt();
                offset = ((2.0 * ratio - 1.0) / (ratio + 1.0)).max(0.0).min(0.5);
                if left > right {
                    offset = -offset;
                }
            }
            self.tone_offset[t] = offset;
            for value in &mut q[k - LOBE_HALF..=k + LOBE_HALF] {
                *value = value.min(fill);
            }
        }
    }

    /// `noise_p[k] = sum(detoned[k-w...k+w]) + frac * (detoned[k-w-1] + detoned[k+w+1])`,
    /// with `w` and `frac` from the bandwidth ramp.
    fn sliding_power_sum(&mut self) {
        let half = self.half;
        let pw = &self.detoned;
        let out = &mut self.noise_p;
        let w0 = BASE_TAPS_HALF;
        let (bin_lo, bin_hi) = (self.bin_lo, self.bin_hi);
        let vec_lo = bin_lo.max(w0);
        let vec_hi = bin_hi.min(half - 1 - w0);
        for k in bin_lo..=bin_hi {
            if !(vec_lo..=vec_hi).contains(&k) {
                out[k] = clamped_sum(pw, k as isize, w0 as isize);
            }
        }
        if vec_hi >= vec_lo {
            for (sum, bins) in out[vec_lo..=vec_hi]
                .iter_mut()
                .zip(pw[vec_lo - 1..=vec_hi + 1].windows(3))
            {
                *sum = bins[0] + bins[1] + bins[2];
            }
        }
        // Bins inside a bandwidth ramp need a wider sum. `taps_half` only grows
        // with frequency, so the sum is carried forward: one add and one
        // subtract to slide it, two more on the rare bin where the width grows.
        let (wide_lo, wide_hi) = self.wide;
        if wide_hi < wide_lo {
            return;
        }
        let at = |index: isize| {
            if index >= 0 && index <= half as isize - 1 {
                pw[index as usize]
            } else {
                0.0
            }
        };
        let mut carried = 0.0f32;
        let mut carried_width: isize = -1;
        let mut since_full = 0;
        let mut k = wide_lo;
        while k <= wide_hi {
            let w = self.taps_half[k] as isize;
            let frac = self.taps_frac[k];
            let ki = k as isize;
            if w == w0 as isize && frac == 0.0 {
                carried_width = -1;
                k += 1;
                continue;
            }
            // The common case, as a tight loop: a run of bins with the same
            // half-width, away from both ends of the spectrum. It does exactly
            // what the first branch below does for one bin.
            // Bins from `k` on whose upper edge bin `k + w + 1` is inside the spectrum.
            let room = (half as isize - 1 - (ki + w)).max(0) as usize;
            if carried_width == w && ki - 1 - w >= 0 && room > 0 {
                let budget = CARRY_RESYNC_BINS.saturating_sub(since_full);
                let n = budget.min(wide_hi - k + 1).min(room);
                let wu = w as usize;
                let (drops, adds, edges) = (&pw[k - 1 - wu..][..n], &pw[k + wu..][..n], &pw[k + wu + 1..][..n]);
                let (taps, fracs, sums) = (&self.taps_half[k..][..n], &self.taps_frac[k..][..n], &mut out[k..][..n]);
                let mut done = 0;
                for i in 0..n {
                    if taps[i] != wu || !(carried > 0.0) || (wu == w0 && fracs[i] == 0.0) {
                        break;
                    }
                    carried += adds[i] - drops[i];
                    let mut sum = carried.max(0.0);
                    sum += fracs[i] * drops[i];
                    sum += fracs[i] * edges[i];
                    sums[i] = sum;
                    done += 1;
                }
                if done > 0 {
                    since_full += done;
                    k += done;
                    continue;
                }
            }
            if carried_width == w && since_full < CARRY_RESYNC_BINS && carried > 0.0 {
                carried += at(ki + w) - at(ki - 1 - w);
                since_full += 1;
            } else if carried_width >= 0 && w > carried_width && since_full < CARRY_RESYNC_BINS && carried > 0.0 {
                // Slide by one, then widen to the new half-width.
                carried += at(ki + carried_width) - at(ki - 1 - carried_width);
                let mut width = carried_width;
                while width < w {
                    width += 1;
                    carried += at(ki - width);
                    carried += at(ki + width);
                }
                carried_width = w;
                since_full += 1;
            } else {
                carried = clamped_sum(pw, ki, w);
                carried_width = w;
                since_full = 0;
            }
            let mut sum = carried.max(0.0);
            if frac > 0.0 {
                let (a, b) = (ki - w - 1, ki + w + 1);
                if a >= 0 {
                    sum += frac * pw[a as usize];
                }
                if b <= half as isize - 1 {
                    sum += frac * pw[b as usize];
                }
            }
            out[k] = sum;
            k += 1;
        }
    }

    fn noise_to_decibels(&mut self) {
        if self.db_hi < self.bin_lo {
            return;
        }
        let range = self.bin_lo..=self.db_hi;
        for (db, &p) in self.noise_db[range.clone()].iter_mut().zip(&self.noise_p[range]) {
            // No negative power reaches log10, and digital silence stays finite.
            *db = 10.0 * (p.max(0.0) + 1e-30).log10();
        }
    }

    // MARK: Display mapping

    fn map_channel(&mut self, c: usize) {
        let n = self.display_bins;
        if n == 0 {
            return;
        }
        let Self {
            display_db,
            noise_db: db,
            noise_p,
            map_start,
            map_end,
            map_pos,
            ..
        } = self;
        let out = &mut display_db[c * n..(c + 1) * n];
        let (lo, hi) = (self.bin_lo, self.db_hi);

        // Below the first usable FFT bin: fade towards the floor, 18 dB per
        // octave, so the curve leaves the plot and holds no plateau at bin 1.
        if self.fade.1 >= self.fade.0 {
            let edge = db[lo.max(1)];
            for i in self.fade.0 as usize..=self.fade.1 as usize {
                if map_start[i] == MAP_FADE {
                    let octaves = -map_pos[i].max(1e-6).log2();
                    out[i] = (edge - 18.0 * octaves).max(FLOOR_DB);
                }
            }
        }

        // Finer than the FFT grid: interpolate. The value at the centre of the
        // display bin, not the loudest point inside it.
        if self.interp.1 >= self.interp.0 {
            // The tangents only change when the display bin moves to the next
            // pair of FFT bins, so the segment is computed once per pair.
            let mut segment = usize::MAX;
            let (mut y1, mut y2, mut m1, mut m2) = (0.0f32, 0.0f32, 0.0f32, 0.0f32);
            for i in self.interp.0 as usize..=self.interp.1 as usize {
                if map_start[i] != MAP_INTERP {
                    continue;
                }
                let x = map_pos[i];
                let i1 = (x as usize).clamp(lo, hi);
                if i1 != segment {
                    segment = i1;
                    (y1, y2, m1, m2) = hermite_segment(db, lo, hi, i1);
                }
                let t = x - i1 as f32;
                let (t2, t3) = (t * t, t * t * t);
                out[i] = (2.0 * t3 - 3.0 * t2 + 1.0) * y1
                    + (t3 - 2.0 * t2 + t) * m1
                    + (-2.0 * t3 + 3.0 * t2) * y2
                    + (t3 - t2) * m2;
            }
        }

        // Two or more FFT bins: power mean.
        if self.mean.1 >= self.mean.0 {
            for i in self.mean.0 as usize..=self.mean.1 as usize {
                let start = map_start[i];
                if start < 0 {
                    out[i] = FLOOR_DB;
                    continue;
                }
                let (start, end) = (start as usize, map_end[i] as usize);
                let sum: f32 = noise_p[start..=end].iter().sum();
                out[i] = 10.0 * (sum / (end - start + 1) as f32).max(1e-30).log10();
            }
        }

        smooth_noise(out, self.served, self.smooth_half_width, &mut self.smooth_work);
        if self.tone_count > 0 {
            self.map_tones(c);
        }
    }

    /// Draws every tone's main lobe onto the noise curve, in the power domain.
    ///
    /// The lobe is evaluated at the display bins, not sampled at FFT bins and
    /// interpolated: the tone's position is known to a fraction of a bin and
    /// the window's lobe is known exactly. A display bin takes the lobe at the
    /// point of its own span that is nearest to the tone, which is the highest
    /// point inside it, so the bin that holds the tone reads the calibrated level.
    fn map_tones(&mut self, c: usize) {
        let n = self.display_bins;
        let out = &mut self.display_db[c * n..(c + 1) * n];
        let (lobe, sums) = (&self.lobe_db, &self.sum_db);
        let (lobe_steps, sum_steps, sum_range) = (LOBE_STEPS as f32, SUM_STEPS as f32, SUM_RANGE_DB as f32);
        let first = self.interp.0.min(self.mean.0);
        let last = self.interp.1.max(self.mean.1);
        if last < first {
            return;
        }
        for t in 0..self.tone_count {
            let power = self.tone_power[t];
            if !(power > 1e-20) {
                continue;
            }
            let k = self.tone_bin[t];
            let x0 = k as f32 + self.tone_offset[t];
            let scale = self.lobe_scale[k];
            let peak_db = 10.0 * power.log10();
            let f0 = ((x0 - 2.0 * scale) * self.df).max(1e-3);
            let f1 = (x0 + 2.0 * scale) * self.df;
            let i0 = ((self.log_f0_scale * f0.ln() + self.log_f0_offset).floor() as isize).max(first);
            let i1 = ((self.log_f0_scale * f1.ln() + self.log_f0_offset).ceil() as isize).min(last);
            if i1 < i0 {
                continue;
            }
            let per_unit = lobe_steps / scale;
            for i in i0 as usize..=i1 as usize {
                if self.map_start[i] == MAP_NONE || self.map_start[i] == MAP_FADE {
                    continue;
                }
                let x = self.map_pos[i];
                let (x_lo, x_hi) = (x / self.display_half_step, x * self.display_half_step);
                let distance = if x0 < x_lo {
                    x_lo - x0
                } else if x0 > x_hi {
                    x0 - x_hi
                } else {
                    0.0
                };
                let u = distance * per_unit;
                if !(u < 2.0 * lobe_steps) {
                    continue;
                }
                let ui = u as usize;
                let tone_db = peak_db + lobe[ui] + (u - ui as f32) * (lobe[ui + 1] - lobe[ui]);
                // out = 10 log10(10^(out/10) + 10^(tone/10)), from the table.
                let noise = out[i];
                let louder = noise.max(tone_db);
                let gap = (noise - tone_db).abs();
                if gap >= sum_range {
                    out[i] = louder;
                    continue;
                }
                let g = gap * sum_steps;
                let gi = g as usize;
                out[i] = louder + sums[gi] + (g - gi as f32) * (sums[gi + 1] - sums[gi]);
            }
        }
    }
}

/// Sets `flags[k]` for every bin in `bins` that is the top of a Hann lobe and
/// passes `gate(tone, block, corrected, k)`. `tone` is the 5-bin sum at the
/// bin. `block` is the context estimate from `second_smallest_block`.
///
/// A bin is a candidate when it stands over both neighbours (`rise > 0`) and
/// holds its share of the 5-bin sum. The test has no branch per bin: on noise a
/// third of all bins are local maxima, in no order a branch predictor can
/// learn. A flagged bin cannot have a flagged neighbour from the same spectrum,
/// because two neighbours cannot both stand over each other.
#[allow(clippy::too_many_arguments)]
#[inline(always)]
fn flag_peaks(
    p: &[f32],
    rise: &[f32],
    sum3: &[f32],
    sum5: &[f32],
    bins: (usize, usize),
    blocks: (isize, isize),
    flags: &mut [u8],
    gate: impl Fn(f32, f32, bool, usize) -> bool,
) {
    let (first, last) = bins;
    let (block_lo, block_hi) = blocks;
    let candidate = |k: usize| (rise[k] > 0.0) & (p[k] >= TONE_PEAK_SHARE * sum5[k]);
    // Near an edge some of the six context blocks are missing.
    let near_edge = |k: usize, flags: &mut [u8]| {
        if candidate(k) {
            if let Some((block, corrected)) = second_smallest_block(sum3, k, block_lo, block_hi) {
                flags[k] |= gate(sum5[k], block, corrected, k) as u8;
            }
        }
    };
    // Bins with all six blocks: k - 11 >= block_lo and k + 9 <= block_hi.
    let inner_first = first.max((block_lo + 11).max(0) as usize);
    let inner_end = (last as isize + 1).min(block_hi - 8).max(inner_first as isize) as usize;
    for k in first..inner_first.min(last + 1) {
        near_edge(k, flags);
    }
    if inner_end > inner_first {
        let n = inner_end - inner_first;
        let at = |offset: isize| {
            let start = (inner_first as isize + offset) as usize;
            &sum3[start..start + n]
        };
        let (a0, a1, a2, b0, b1, b2) = (at(3), at(6), at(9), at(-5), at(-8), at(-11));
        let (p, rise, sum5) = (
            &p[inner_first..inner_end],
            &rise[inner_first..inner_end],
            &sum5[inner_first..inner_end],
        );
        for (i, flag) in flags[inner_first..inner_end].iter_mut().enumerate() {
            let (mut low, mut second) = (f32::MAX, f32::MAX);
            for v in [a0[i], b0[i], a1[i], b1[i], a2[i], b2[i]] {
                insert_smallest(v, &mut low, &mut second);
            }
            let tone = sum5[i];
            let tonal = (rise[i] > 0.0) & (p[i] >= TONE_PEAK_SHARE * tone) & gate(tone, second, true, inner_first + i);
            *flag |= tonal as u8;
        }
    }
    for k in inner_end.max(inner_first.min(last + 1))..=last {
        near_edge(k, flags);
    }
}

/// `rise[k] = min(p[k] - p[k+1], p[k] - p[k-1])` over `first...last`: positive
/// where the bin stands over both neighbours. A bin equal to a neighbour reads
/// 0 and is not a peak.
fn mark_local_maxima(p: &[f32], first: usize, last: usize, rise: &mut [f32]) {
    for k in first..=last {
        rise[k] = (p[k] - p[k + 1]).min(p[k] - p[k - 1]);
    }
}

/// `sum3` and `sum5` of one spectrum over `lo...hi`. Returns the range of
/// `sum3` a context block can start at: `lo` (never bin 0), and `hi - 2` so
/// that the three bins of the block are inside.
fn sliding_sums(p: &[f32], lo: usize, hi: usize, sum3: &mut [f32], sum5: &mut [f32]) -> (isize, isize) {
    for j in lo..=hi - 2 {
        sum3[j] = p[j] + p[j + 1] + p[j + 2];
    }
    for k in lo + 2..=hi - 2 {
        sum5[k] = sum3[k - 2] + p[k + 1] + p[k + 2];
    }
    (lo.max(1) as isize, hi as isize - 2)
}

/// Keeps the two smallest block values, `b0 <= b1`.
///
/// No branch: on noise a third of all bins are local maxima, each one runs this
/// six times, and which way a compare goes there is a coin toss. With branches
/// this was the most expensive thing in `process`.
#[inline(always)]
fn insert_smallest(v: f32, b0: &mut f32, b1: &mut f32) {
    *b1 = b1.min(b0.max(v));
    *b0 = b0.min(v);
}

/// The context estimate on the sliding 3-bin *sums* (three times the block
/// mean): the second smallest of the six blocks beside `k`, and whether it is
/// that. With fewer than three blocks near an edge it is the smallest.
#[inline(always)]
fn second_smallest_block(sum3: &[f32], k: usize, lo: isize, hi: isize) -> Option<(f32, bool)> {
    let (mut b0, mut b1) = (f32::MAX, f32::MAX);
    let mut seen = 0;
    for block in 0..TONE_CONTEXT_BLOCKS {
        let above = k as isize + 3 + 3 * block;
        let below = k as isize - 5 - 3 * block;
        if above <= hi {
            insert_smallest(sum3[above as usize], &mut b0, &mut b1);
            seen += 1;
        }
        if below >= lo {
            insert_smallest(sum3[below as usize], &mut b0, &mut b1);
            seen += 1;
        }
    }
    match seen {
        0 => None,
        1 | 2 => Some((b0, false)),
        _ => Some((b1, true)),
    }
}

/// The raw block estimate: the second smallest of the six block means beside
/// `k`, three each side, outside the main lobe. `pw` is one channel, `half` long.
///
/// Block means, not a side mean: in a chord the next partial often sits inside
/// one side. A low order statistic, not the bare minimum: the minimum reads
/// about 2.5 dB low, lets noise through the gate and notches the noise curve
/// beside every real peak.
#[inline(always)]
fn context_block(pw: &[f32], k: usize) -> Option<(f32, bool)> {
    let half = pw.len() as isize;
    let (mut b0, mut b1) = (f32::MAX, f32::MAX);
    let mut seen = 0;
    let mean = |start: isize| {
        let s = start as usize;
        (pw[s] + pw[s + 1] + pw[s + 2]) / 3.0
    };
    for block in 0..TONE_CONTEXT_BLOCKS {
        let above = k as isize + 3 + 3 * block;
        let below = k as isize - 5 - 3 * block;
        if above + 2 <= half - 1 {
            insert_smallest(mean(above), &mut b0, &mut b1);
            seen += 1;
        }
        if below >= 1 {
            insert_smallest(mean(below), &mut b0, &mut b1);
            seen += 1;
        }
    }
    match seen {
        0 => None,
        1 | 2 => Some((b0, false)),
        _ => Some((b1, true)),
    }
}

/// Noise power per bin near `k`, with the bias of the estimate taken out.
#[inline(always)]
fn context_noise(pw: &[f32], k: usize) -> Option<f32> {
    context_block(pw, k).map(|(block, corrected)| if corrected { block * CONTEXT_GAIN } else { block })
}

/// Sum of `pw[k-w ..= k+w]`, clamped to the slice.
#[inline(always)]
fn clamped_sum(pw: &[f32], k: isize, w: isize) -> f32 {
    let a = (k - w).max(0) as usize;
    let b = (k + w).min(pw.len() as isize - 1);
    if b < a as isize {
        return 0.0;
    }
    let mut sum = 0.0f32;
    for &p in &pw[a..=b as usize] {
        sum += p;
    }
    sum
}

/// A triangular kernel across *display* bins, applied to the noise curve only.
///
/// One frame of a periodogram scatters by about 2 dB from bin to bin whatever
/// the FFT size: that is the chi-square of the estimator, not structure in the
/// signal. Because the display grid is logarithmic, a fixed kernel in display
/// bins is constant-Q: `SMOOTH_OCTAVES` wide everywhere. Tonal peaks are added
/// after this and stay pointed.
///
/// A triangular kernel is a box filter applied twice, and a box filter is a
/// running sum, so this costs four adds per display bin however wide the kernel is.
fn smooth_noise(out: &mut [f32], served: (isize, isize), half_width: usize, work: &mut [f32]) {
    let (lo, hi) = served;
    let m = half_width as isize;
    if !(m > 0 && hi - lo >= 2 * m) {
        return;
    }
    let lo = lo as usize;
    let n = hi - lo as isize + 1;
    let length = m + 1;
    let src = &mut out[lo..lo + n as usize];
    let at = |src: &[f32], j: isize| src[j.clamp(0, n - 1) as usize];

    // Pass 1, forward box of `length` taps: work[j + m] = sum of source(j ... j+m).
    let mut boxed = 0.0f32;
    for u in 0..length {
        boxed += at(src, -m + u);
    }
    work[0] = boxed;
    // Only the two ends need the clamp. The interior is most of the range.
    let interior_end = n - length;
    for j in -m + 1..1.min(n) {
        boxed += at(src, j + length - 1) - src[0];
        work[(j + m) as usize] = boxed;
    }
    if interior_end >= 1 {
        let (first, count) = (1usize, interior_end as usize);
        let (enter, leave) = (&src[first + m as usize..][..count], &src[first - 1..][..count]);
        for ((slot, enter), leave) in work[first + m as usize..][..count].iter_mut().zip(enter).zip(leave) {
            boxed += enter - leave;
            *slot = boxed;
        }
    }
    for j in interior_end.max(0) + 1..n {
        boxed += src[(n - 1) as usize] - src[(j - 1) as usize];
        work[(j + m) as usize] = boxed;
    }

    // Pass 2, backward box over the first: the two together are the triangle, centred.
    let norm = 1.0 / (length * length) as f32;
    let mut acc = 0.0f32;
    for t in 0..length {
        acc += work[(m - t) as usize];
    }
    src[0] = acc * norm;
    for i in 1..n as usize {
        acc += work[i + m as usize] - work[i - 1];
        src[i] = acc * norm;
    }
}

/// End values and clamped tangents of the monotone cubic Hermite
/// (Fritsch-Carlson) segment `i1 ... i1+1`, in the dB domain. Used where
/// display bins are finer than FFT bins, which is the whole low end of a log
/// display. Monotone, not Catmull-Rom: a plain cubic overshoots on noisy data.
///
/// The Fritsch-Carlson clamp is normally written with `a = m1 / delta` and
/// `b = m2 / delta`. Both tests it needs, the signs and `a^2 + b^2 > 9`, are
/// made here without the divisions.
pub(crate) fn hermite_segment(y: &[f32], lo: usize, hi: usize, i1: usize) -> (f32, f32, f32, f32) {
    let y0 = y[i1.saturating_sub(1).max(lo)];
    let y1 = y[i1];
    let y2 = y[(i1 + 1).min(hi)];
    let y3 = y[(i1 + 2).min(hi)];

    let delta = y2 - y1;
    if delta == 0.0 {
        return (y1, y2, 0.0, 0.0);
    }
    let mut m1 = 0.5 * (y2 - y0);
    let mut m2 = 0.5 * (y3 - y1);
    if m1 * delta < 0.0 {
        m1 = 0.0;
    }
    if m2 * delta < 0.0 {
        m2 = 0.0;
    }
    let sum = m1 * m1 + m2 * m2;
    let limit = 9.0 * delta * delta;
    if sum > limit {
        let scale = 3.0 * delta.abs() / sum.sqrt();
        m1 *= scale;
        m2 *= scale;
    }
    (y1, y2, m1, m2)
}
