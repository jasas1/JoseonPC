//! Stereo field analysis: phase correlation, balance, width, the same per
//! listening band, and the vectorscope point cloud.
//!
//! Port source: `Sources/JoseonCore/Stereo/StereoAnalyzer.swift`.
//!
//! Design
//! - All running numbers use one exponential window of about 300 ms. The window
//!   is a one-pole leaky integrator over the raw products `L·L`, `R·R` and
//!   `L·R`, so `correlation = ΣLR / √(ΣLL · ΣRR)`. Mid and side get their own
//!   accumulators: for a near-mono signal the derived form cancels to noise.
//! - The band numbers come from a filter bank of 8th-order Linkwitz-Riley
//!   band-passes (48 dB/octave), one per `BandEnergy::EDGES_HZ` band, built for
//!   the live sample rate. Left and right run identical coefficients.
//! - A band whose level sits under `SILENCE_MEAN_SQUARE` (−100 dBFS), or more
//!   than 80 dB under the broadband level, reports 0 correlation and 0 balance.
//!   So empty bands read neutral and do not amplify filter skirt leakage.
//! - `width` is the side RMS over the mid RMS: 0 for mono, 1 for a hard-panned
//!   channel, clamped at `MAX_WIDTH` where there is no mid energy at all.
//! - `process` allocates nothing after the first call at a given sample rate.
//!   It works in chunks of `MAX_CHUNK_FRAMES`, so a large block allocates
//!   nothing too. `read` allocates the point list.
//! - One thread owns the analyzer.

mod filter_bank;

#[cfg(test)]
mod tests;

use crate::contracts::{BandEnergy, StereoAnalyzing, StereoReading};
use filter_bank::BandSplitFilterBank;

/// Time constant of the running window in seconds.
pub const WINDOW_SECONDS: f64 = 0.3;
/// Span of the vectorscope point cloud in seconds.
pub const SCOPE_SECONDS: f64 = 0.05;
/// `width` is clamped here. An anti-phase signal has no mid energy at all.
pub const MAX_WIDTH: f32 = 4.0;
/// Below this mean square (−100 dBFS) the input counts as silent.
pub const SILENCE_MEAN_SQUARE: f64 = 1e-10;
/// A band this far under the broadband level (80 dB) counts as empty.
pub const BAND_RELATIVE_FLOOR: f64 = 1e-8;
/// Frames per internal processing chunk. Bounds every scratch buffer.
pub const MAX_CHUNK_FRAMES: usize = 1024;

const BAND_COUNT: usize = BandEnergy::EDGES_HZ.len() - 1;

/// Sum of products, one add at a time.
fn dot(a: &[f64], b: &[f64]) -> f64 {
    let mut sum = 0.0;
    for (x, y) in a.iter().zip(b) {
        sum += x * y;
    }
    sum
}

pub struct StereoAnalyzer {
    scope_point_count: usize,
    configured_sample_rate: f64,
    /// Per-sample decay of the running window.
    per_sample_decay: f64,

    acc_ll: f64,
    acc_rr: f64,
    acc_lr: f64,
    acc_mm: f64,
    acc_ss: f64,
    /// Sum of the window weights. Turns the accumulators into mean squares.
    acc_norm: f64,
    /// Chunks that had a NaN or an Inf sample. For tests and diagnostics.
    sanitized_chunk_count: usize,

    band_ll: [f64; BAND_COUNT],
    band_rr: [f64; BAND_COUNT],
    band_lr: [f64; BAND_COUNT],
    bank: Option<BandSplitFilterBank>,

    // Scratch, sized once for `MAX_CHUNK_FRAMES`.
    work_l: Vec<f64>,
    work_r: Vec<f64>,

    /// Vectorscope ring, one `[x, y]` per input frame over `SCOPE_SECONDS`.
    scope: Vec<[f32; 2]>,
    scope_write: usize,
    scope_filled: usize,
}

impl Default for StereoAnalyzer {
    fn default() -> Self {
        Self::new()
    }
}

impl StereoAnalyzer {
    pub fn new() -> Self {
        Self {
            scope_point_count: 2048,
            configured_sample_rate: 0.0,
            per_sample_decay: 0.0,
            acc_ll: 0.0,
            acc_rr: 0.0,
            acc_lr: 0.0,
            acc_mm: 0.0,
            acc_ss: 0.0,
            acc_norm: 0.0,
            sanitized_chunk_count: 0,
            band_ll: [0.0; BAND_COUNT],
            band_rr: [0.0; BAND_COUNT],
            band_lr: [0.0; BAND_COUNT],
            bank: None,
            work_l: vec![0.0; MAX_CHUNK_FRAMES],
            work_r: vec![0.0; MAX_CHUNK_FRAMES],
            scope: Vec::new(),
            scope_write: 0,
            scope_filled: 0,
        }
    }

    /// Chunks that had a NaN or an Inf sample since the analyzer was made.
    pub fn sanitized_chunk_count(&self) -> usize {
        self.sanitized_chunk_count
    }

    /// Rebuilds the filter bank and the scope ring for a new sample rate and
    /// clears the state. Never runs in the steady state.
    fn configure(&mut self, sample_rate: f64) {
        self.configured_sample_rate = sample_rate;
        self.per_sample_decay = (-1.0 / (WINDOW_SECONDS * sample_rate)).exp();
        self.bank = BandSplitFilterBank::new(&BandEnergy::EDGES_HZ, sample_rate);
        // The cap keeps a nonsense sample rate from asking for a huge ring.
        let wanted = ((SCOPE_SECONDS * sample_rate).ceil() as usize).clamp(64, 1 << 20);
        if wanted != self.scope.len() {
            self.scope = vec![[0.0; 2]; wanted];
        }
        self.clear_running_state();
    }

    fn clear_running_state(&mut self) {
        (
            self.acc_ll,
            self.acc_rr,
            self.acc_lr,
            self.acc_mm,
            self.acc_ss,
            self.acc_norm,
        ) = (0.0, 0.0, 0.0, 0.0, 0.0, 0.0);
        self.band_ll = [0.0; BAND_COUNT];
        self.band_rr = [0.0; BAND_COUNT];
        self.band_lr = [0.0; BAND_COUNT];
        if let Some(bank) = &mut self.bank {
            bank.reset_state();
        }
        self.scope_write = 0;
        self.scope_filled = 0;
        self.scope.fill([0.0; 2]);
    }

    fn process_chunk(&mut self, left: &[f32], right: &[f32]) {
        let n = left.len();
        let (work_l, work_r) = (&mut self.work_l[..n], &mut self.work_r[..n]);
        for i in 0..n {
            work_l[i] = left[i] as f64;
            work_r[i] = right[i] as f64;
        }

        // A NaN or an Inf sample would stay in the accumulators and in the
        // filter memory until the next reset. The two energy sums are needed
        // anyway, and they are finite only when every sample is finite. So the
        // check is free, and the repair (non-finite → 0) runs only for a bad block.
        let mut sum_ll = dot(work_l, work_l);
        let mut sum_rr = dot(work_r, work_r);
        let had_non_finite = !(sum_ll + sum_rr).is_finite();
        if had_non_finite {
            self.sanitized_chunk_count += 1;
            // `f32::MAX` squared is finite in f64, so only NaN and Inf land here.
            for value in work_l.iter_mut().chain(work_r.iter_mut()) {
                if !value.is_finite() {
                    *value = 0.0;
                }
            }
            sum_ll = dot(work_l, work_l);
            sum_rr = dot(work_r, work_r);
        }

        // Mid = (L+R)/2, side = (L−R)/2.
        let (mut sum_lr, mut sum_mm, mut sum_ss) = (0.0, 0.0, 0.0);
        for (&l, &r) in work_l.iter().zip(work_r.iter()) {
            let (mid, side) = ((l + r) * 0.5, (l - r) * 0.5);
            sum_lr += l * r;
            sum_mm += mid * mid;
            sum_ss += side * side;
        }

        let decay = self.per_sample_decay.powf(n as f64);
        self.acc_ll = self.acc_ll * decay + sum_ll;
        self.acc_rr = self.acc_rr * decay + sum_rr;
        self.acc_lr = self.acc_lr * decay + sum_lr;
        self.acc_mm = self.acc_mm * decay + sum_mm;
        self.acc_ss = self.acc_ss * decay + sum_ss;
        self.acc_norm = self.acc_norm * decay + n as f64;

        if let Some(bank) = &mut self.bank {
            bank.run(work_l, work_r);
            for band in 0..BAND_COUNT {
                let (band_l, band_r) = (bank.left(band), bank.right(band));
                self.band_ll[band] = self.band_ll[band] * decay + dot(band_l, band_l);
                self.band_rr[band] = self.band_rr[band] * decay + dot(band_r, band_r);
                self.band_lr[band] = self.band_lr[band] * decay + dot(band_l, band_r);
            }
        }

        // Long silence would let every accumulator decay through the denormal
        // range and stay there. Nothing this small is ever read back.
        if self.acc_ll + self.acc_rr < 1e-30 {
            (self.acc_ll, self.acc_rr, self.acc_lr, self.acc_mm, self.acc_ss) = (0.0, 0.0, 0.0, 0.0, 0.0);
            self.band_ll = [0.0; BAND_COUNT];
            self.band_rr = [0.0; BAND_COUNT];
            self.band_lr = [0.0; BAND_COUNT];
        }

        self.append_scope_points(left, right, had_non_finite);
    }

    /// x = (R − L)·0.5, y = (L + R)·0.5. The clamp to −1...1 is for
    /// out-of-range input only.
    ///
    /// The scale is 0.5, not 0.7071, so that the mapping fits the −1...1 box of
    /// the contract for every in-range sample: |x| + |y| = max(|L|, |R|) ≤ 1,
    /// which is the diamond inscribed in the unit square. The full-scale
    /// diamond has its tips exactly at ±1 on both axes, and a hard-clipped
    /// signal runs along its 45° edges and does not hit a clamp.
    ///
    /// `sanitize`: the chunk has a non-finite sample; those samples count as 0.
    fn append_scope_points(&mut self, left: &[f32], right: &[f32], sanitize: bool) {
        let capacity = self.scope.len();
        if capacity == 0 {
            return;
        }
        let n = left.len();
        // Only the newest `capacity` frames can survive.
        let start = n.saturating_sub(capacity);
        let mut write = self.scope_write;
        let clamp = |v: f32| {
            if v > 1.0 {
                1.0
            } else if v < -1.0 {
                -1.0
            } else if v.is_nan() {
                0.0
            } else {
                v
            }
        };
        for i in start..n {
            let (mut l, mut r) = (left[i], right[i]);
            if sanitize {
                if !l.is_finite() {
                    l = 0.0;
                }
                if !r.is_finite() {
                    r = 0.0;
                }
            }
            self.scope[write] = [clamp((r - l) * 0.5), clamp((l + r) * 0.5)];
            write += 1;
            if write == capacity {
                write = 0;
            }
        }
        self.scope_write = write;
        self.scope_filled = capacity.min(self.scope_filled + (n - start));
    }

    /// The newest `scope_point_count` points, newest last, spread evenly over
    /// the stored `SCOPE_SECONDS` of audio.
    fn gather_scope_points(&self) -> Vec<[f32; 2]> {
        let capacity = self.scope.len();
        let window = self.scope_filled;
        let wanted = self.scope_point_count.min(window);
        if wanted == 0 || capacity == 0 {
            return Vec::new();
        }
        let oldest = (self.scope_write + capacity - window) % capacity;
        if wanted == 1 {
            return vec![self.scope[(oldest + window - 1) % capacity]];
        }
        let span = (window - 1) as f64;
        let steps = (wanted - 1) as f64;
        (0..wanted)
            .map(|i| {
                let position = (i as f64 * span / steps).round() as usize;
                self.scope[(oldest + position) % capacity]
            })
            .collect()
    }
}

impl StereoAnalyzing for StereoAnalyzer {
    fn scope_point_count(&self) -> usize {
        self.scope_point_count
    }

    fn set_scope_point_count(&mut self, count: usize) {
        self.scope_point_count = count;
    }

    fn process(&mut self, left: &[f32], right: &[f32], sample_rate: f64) {
        let count = left.len().min(right.len());
        if !(count > 0 && sample_rate > 0.0 && sample_rate.is_finite()) {
            return;
        }
        if sample_rate != self.configured_sample_rate {
            self.configure(sample_rate);
        }
        for (l, r) in left[..count]
            .chunks(MAX_CHUNK_FRAMES)
            .zip(right[..count].chunks(MAX_CHUNK_FRAMES))
        {
            self.process_chunk(l, r);
        }
    }

    fn read(&mut self) -> StereoReading {
        let mut reading = StereoReading {
            scope_points: self.gather_scope_points(),
            ..Default::default()
        };
        if !(self.acc_norm > 0.0) {
            return reading;
        }
        let broadband = (self.acc_ll + self.acc_rr) / (2.0 * self.acc_norm);
        if !(broadband.is_finite() && broadband >= SILENCE_MEAN_SQUARE) {
            return reading;
        }
        reading.correlation = correlation(self.acc_ll, self.acc_rr, self.acc_lr);
        reading.balance = balance(self.acc_ll, self.acc_rr);
        reading.width = width(self.acc_mm, self.acc_ss);

        let band_floor = SILENCE_MEAN_SQUARE.max(broadband * BAND_RELATIVE_FLOOR);
        for band in 0..BAND_COUNT {
            let mean_square = (self.band_ll[band] + self.band_rr[band]) / (2.0 * self.acc_norm);
            if !(mean_square.is_finite() && mean_square >= band_floor) {
                continue;
            }
            reading.band_active[band] = true;
            reading.band_correlation[band] = correlation(self.band_ll[band], self.band_rr[band], self.band_lr[band]);
            reading.band_balance[band] = balance(self.band_ll[band], self.band_rr[band]);
        }
        reading
    }

    fn reset(&mut self) {
        self.clear_running_state();
    }
}

/// Side RMS over mid RMS. 0 for mono, 1 for a hard-panned channel, clamped at `MAX_WIDTH`.
fn width(mid_sum: f64, side_sum: f64) -> f32 {
    let (mid, side) = (mid_sum.max(0.0), side_sum.max(0.0));
    if !(side > 0.0) {
        return 0.0;
    }
    if !(mid > 0.0) {
        return MAX_WIDTH;
    }
    let ratio = (side / mid).sqrt();
    if !ratio.is_finite() {
        return MAX_WIDTH;
    }
    ratio.min(MAX_WIDTH as f64) as f32
}

fn correlation(ll: f64, rr: f64, lr: f64) -> f32 {
    let denominator = (ll.max(0.0) * rr.max(0.0)).sqrt();
    if !(denominator > 0.0 && denominator.is_finite()) {
        return 0.0;
    }
    let c = lr / denominator;
    if !c.is_finite() {
        return 0.0;
    }
    c.clamp(-1.0, 1.0) as f32
}

fn balance(ll: f64, rr: f64) -> f32 {
    let (l, r) = (ll.max(0.0).sqrt(), rr.max(0.0).sqrt());
    let sum = l + r;
    if !(sum > 0.0 && sum.is_finite()) {
        return 0.0;
    }
    let b = (r - l) / sum;
    if !b.is_finite() {
        return 0.0;
    }
    b.clamp(-1.0, 1.0) as f32
}
