//! ITU-R BS.1770-4 / EBU R128 loudness, EBU Tech 3342 loudness range, and
//! inter-sample true peak.
//!
//! Port source: `Sources/JoseonCore/Loudness/LoudnessMeter.swift`.
//!
//! Design notes
//! - K-weighting coefficients are derived from the analogue prototype at the
//!   live sample rate, never read from the 48 kHz table.
//! - Audio is cut into 100 ms blocks. Every finished block updates the 400 ms
//!   momentary window, the 3 s short-term window and the 300 ms RMS window, and
//!   files one value into each histogram. Memory is therefore constant: an
//!   eight-hour listen costs the same as one second.
//! - `process` allocates nothing. Every buffer is sized in `new`.
//! - One thread owns the meter.

mod histogram;
mod k_weighting;
mod peak_hold;
mod true_peak;

#[cfg(test)]
mod tests;

use crate::contracts::{LoudnessMetering, LoudnessReading};
use histogram::LoudnessHistogram;
use k_weighting::KWeightingFilter;
use peak_hold::PeakHoldBallistics;
use true_peak::TruePeakDetector;

/// Gating block hop. BS.1770 overlaps 400 ms blocks by 75 %.
const BLOCK_SECONDS: f64 = 0.1;
/// 400 ms.
const MOMENTARY_BLOCKS: usize = 4;
/// 3 s.
const SHORT_TERM_BLOCKS: usize = 30;
/// 300 ms.
const RMS_BLOCKS: usize = 3;
/// BS.1770 loudness offset, −0.691 dB.
pub(crate) const LOUDNESS_OFFSET: f64 = -0.691;
const INTEGRATED_RELATIVE_GATE_LU: f64 = -10.0;
const RANGE_RELATIVE_GATE_LU: f64 = -20.0;
const RANGE_LOW_PERCENTILE: f64 = 0.10;
const RANGE_HIGH_PERCENTILE: f64 = 0.95;
/// Channel weight for left and right, BS.1770 Table 3.
const CHANNEL_WEIGHT: f64 = 1.0;
/// True-peak display ballistics. The hold is a sliding maximum over this many
/// seconds, so a repeating peak keeps the meter up, and two channels with the
/// same peak read the same number (see `PeakHoldBallistics`).
pub(crate) const TRUE_PEAK_HOLD_SECONDS: f64 = 1.5;
const TRUE_PEAK_FALL_DB_PER_SECOND: f64 = 20.0;
/// A sample at or above this magnitude counts as full scale.
const CLIP_THRESHOLD: f32 = 0.9999;
/// How many full-scale samples in a row make one clip event.
const CLIP_RUN_LENGTH: u32 = 3;
/// Lowest number any dB field reports. Matches `LoudnessReading::SILENCE_LUFS`.
const FLOOR_DB: f64 = LoudnessReading::SILENCE_LUFS as f64;

const RING_CAPACITY: usize = SHORT_TERM_BLOCKS;

/// Per channel: filter, peak detector, display ballistics, and the sums of the
/// current 100 ms block.
struct Channel {
    weighting: KWeightingFilter,
    true_peak: TruePeakDetector,
    ballistics: PeakHoldBallistics,
    block_sum_weighted: f64,
    block_sum_raw: f64,
    /// Mean square per finished block, K-weighted.
    ring_weighted: [f64; RING_CAPACITY],
    /// Mean square per finished block, unweighted.
    ring_raw: [f64; RING_CAPACITY],
    clip_run: u32,
}

impl Channel {
    fn new() -> Self {
        Self {
            weighting: KWeightingFilter::new(),
            true_peak: TruePeakDetector::new(),
            ballistics: PeakHoldBallistics::new(TRUE_PEAK_HOLD_SECONDS, TRUE_PEAK_FALL_DB_PER_SECOND, FLOOR_DB),
            block_sum_weighted: 0.0,
            block_sum_raw: 0.0,
            ring_weighted: [0.0; RING_CAPACITY],
            ring_raw: [0.0; RING_CAPACITY],
            clip_run: 0,
        }
    }

    fn clear_windows(&mut self) {
        self.block_sum_weighted = 0.0;
        self.block_sum_raw = 0.0;
        self.ring_weighted = [0.0; RING_CAPACITY];
        self.ring_raw = [0.0; RING_CAPACITY];
        self.ballistics.clear();
    }

    /// Filters, squares, peak-detects and clip-detects the samples. Returns the
    /// true peak level of the chunk in dB and the number of new clip events.
    fn consume(&mut self, x: &[f32]) -> (f64, u64) {
        self.block_sum_weighted += self.weighting.sum_of_squares(x);

        let mut sum_raw = 0.0;
        let mut run = self.clip_run;
        let mut clips = 0;
        for &sample in x {
            let value = sample as f64;
            sum_raw += value * value;
            if sample.abs() >= CLIP_THRESHOLD {
                run += 1;
                if run == CLIP_RUN_LENGTH {
                    clips += 1;
                }
            } else {
                run = 0;
            }
        }
        self.block_sum_raw += sum_raw;
        self.clip_run = run;

        (decibels_from_amplitude(self.true_peak.max_abs(x) as f64), clips)
    }
}

pub struct LoudnessMeter {
    sample_rate: f64,
    block_samples: usize,
    left: Channel,
    right: Channel,

    /// Samples in the current 100 ms block.
    block_fill: usize,
    ring_write: usize,
    ring_filled: usize,
    /// Blocks finished since the windows last started. Only whole 400 ms and
    /// 3 s windows feed the histograms and the maxima.
    blocks_since_reset: usize,

    integrated_histogram: LoudnessHistogram,
    range_histogram: LoudnessHistogram,
    momentary_max: f64,
    short_term_max: f64,
    stats_dirty: bool,
    cached_integrated: f64,
    cached_range: f64,

    true_peak_max: f64,
    clip_count: u64,
    measured_seconds: f64,
}

impl Default for LoudnessMeter {
    fn default() -> Self {
        Self::new()
    }
}

impl LoudnessMeter {
    pub fn new() -> Self {
        Self {
            sample_rate: 0.0,
            block_samples: 0,
            left: Channel::new(),
            right: Channel::new(),
            block_fill: 0,
            ring_write: 0,
            ring_filled: 0,
            blocks_since_reset: 0,
            integrated_histogram: LoudnessHistogram::new(),
            range_histogram: LoudnessHistogram::new(),
            momentary_max: FLOOR_DB,
            short_term_max: FLOOR_DB,
            stats_dirty: true,
            cached_integrated: FLOOR_DB,
            cached_range: 0.0,
            true_peak_max: FLOOR_DB,
            clip_count: 0,
            measured_seconds: 0.0,
        }
    }

    /// A new sample rate means new filter coefficients and a new block length,
    /// so the sliding windows and the filter states start again. The
    /// histograms, the maxima and the clip count survive: they belong to the
    /// measurement, not to the window, and only `reset` clears them.
    fn configure(&mut self, sample_rate: f64) {
        self.sample_rate = sample_rate;
        self.block_samples = ((sample_rate * BLOCK_SECONDS).round() as usize).max(1);
        let factor = true_peak::factor_for_sample_rate(sample_rate);
        for channel in [&mut self.left, &mut self.right] {
            channel.weighting.configure(sample_rate);
            channel.true_peak.configure(factor);
        }
        self.clear_windows();
    }

    fn clear_windows(&mut self) {
        self.block_fill = 0;
        self.left.clear_windows();
        self.right.clear_windows();
        self.ring_write = 0;
        self.ring_filled = 0;
        self.blocks_since_reset = 0;
    }

    /// One chunk that does not cross a 100 ms block boundary, so the step of
    /// the true-peak ballistics is bounded.
    fn consume(&mut self, left: &[f32], right: &[f32]) {
        let (level_left, clips_left) = self.left.consume(left);
        let (level_right, clips_right) = self.right.consume(right);
        self.clip_count += clips_left + clips_right;
        self.block_fill += left.len();

        let dt = left.len() as f64 / self.sample_rate;
        if level_left > self.true_peak_max {
            self.true_peak_max = level_left;
        }
        if level_right > self.true_peak_max {
            self.true_peak_max = level_right;
        }
        // About 1.5 s hold, then about 20 dB/s fall, like a hardware peak
        // meter. The two channels run the same arithmetic on their own levels,
        // so identical input gives identical readings.
        self.left.ballistics.update(level_left, dt);
        self.right.ballistics.update(level_right, dt);
    }

    fn finish_block(&mut self) {
        let inverse = 1.0 / self.block_samples as f64;
        for channel in [&mut self.left, &mut self.right] {
            channel.ring_weighted[self.ring_write] = channel.block_sum_weighted * inverse;
            channel.ring_raw[self.ring_write] = channel.block_sum_raw * inverse;
            channel.block_sum_weighted = 0.0;
            channel.block_sum_raw = 0.0;
        }
        self.ring_write = (self.ring_write + 1) % RING_CAPACITY;
        if self.ring_filled < RING_CAPACITY {
            self.ring_filled += 1;
        }
        self.block_fill = 0;
        self.blocks_since_reset = self.blocks_since_reset.saturating_add(1);

        // Only whole windows count towards the gated measures and the maxima.
        if self.blocks_since_reset >= MOMENTARY_BLOCKS {
            let power = self.weighted_power(MOMENTARY_BLOCKS);
            let level = loudness_from_power(power);
            if level > self.momentary_max {
                self.momentary_max = level;
            }
            self.integrated_histogram.add(level, power);
            self.stats_dirty = true;
        }
        if self.blocks_since_reset >= SHORT_TERM_BLOCKS {
            let power = self.weighted_power(SHORT_TERM_BLOCKS);
            let level = loudness_from_power(power);
            if level > self.short_term_max {
                self.short_term_max = level;
            }
            self.range_histogram.add(level, power);
            self.stats_dirty = true;
        }
    }

    // MARK: Window maths

    /// Sum of the newest `blocks` finished blocks of a ring, newest first, and
    /// how many blocks that was.
    fn newest_sum(&self, ring: &[f64; RING_CAPACITY], blocks: usize) -> (f64, usize) {
        let n = blocks.min(self.ring_filled);
        let mut sum = 0.0;
        let mut index = self.ring_write;
        for _ in 0..n {
            index = if index == 0 { RING_CAPACITY - 1 } else { index - 1 };
            sum += ring[index];
        }
        (sum, n)
    }

    /// BS.1770 weighted channel sum over the newest `blocks` finished blocks.
    fn weighted_power(&self, blocks: usize) -> f64 {
        let (left, n) = self.newest_sum(&self.left.ring_weighted, blocks);
        let (right, _) = self.newest_sum(&self.right.ring_weighted, blocks);
        if n == 0 {
            return 0.0;
        }
        let inverse = 1.0 / n as f64;
        CHANNEL_WEIGHT * left * inverse + CHANNEL_WEIGHT * right * inverse
    }

    fn rms_db(&self, ring: &[f64; RING_CAPACITY]) -> f64 {
        let (sum, n) = self.newest_sum(ring, RMS_BLOCKS);
        if n == 0 {
            return FLOOR_DB;
        }
        decibels_from_power(sum / n as f64)
    }

    // MARK: Gated measures

    fn refresh_stats_if_needed(&mut self) {
        if !self.stats_dirty {
            return;
        }
        self.stats_dirty = false;
        self.cached_integrated = self.compute_integrated();
        self.cached_range = self.compute_range();
    }

    /// BS.1770-4 gated loudness: absolute gate at −70 LUFS, then a relative
    /// gate 10 LU below the mean of what survived.
    fn compute_integrated(&self) -> f64 {
        let histogram = &self.integrated_histogram;
        if histogram.total_count() == 0 {
            return FLOOR_DB;
        }
        let mean = histogram.total_power() / histogram.total_count() as f64;
        let threshold = loudness_from_power(mean) + INTEGRATED_RELATIVE_GATE_LU;
        let (count, power) = histogram.gated(threshold);
        if count == 0 {
            return FLOOR_DB;
        }
        loudness_from_power(power / count as f64)
    }

    /// EBU Tech 3342 loudness range: short-term values, absolute gate at
    /// −70 LUFS, relative gate 20 LU down, then the 10th to 95th percentile.
    fn compute_range(&self) -> f64 {
        let histogram = &self.range_histogram;
        if histogram.total_count() == 0 {
            return 0.0;
        }
        let mean = histogram.total_power() / histogram.total_count() as f64;
        let threshold = loudness_from_power(mean) + RANGE_RELATIVE_GATE_LU;
        match (
            histogram.percentile(RANGE_LOW_PERCENTILE, threshold),
            histogram.percentile(RANGE_HIGH_PERCENTILE, threshold),
        ) {
            (Some(low), Some(high)) => (high - low).max(0.0),
            _ => 0.0,
        }
    }
}

impl LoudnessMetering for LoudnessMeter {
    fn process(&mut self, left: &[f32], right: &[f32], sample_rate: f64) {
        let count = left.len().min(right.len());
        if !(count > 0 && sample_rate.is_finite() && sample_rate > 0.0) {
            return;
        }
        if sample_rate != self.sample_rate {
            self.configure(sample_rate);
        }
        let mut done = 0;
        while done < count {
            let chunk = (self.block_samples - self.block_fill).min(count - done);
            self.consume(&left[done..done + chunk], &right[done..done + chunk]);
            done += chunk;
            if self.block_fill >= self.block_samples {
                self.finish_block();
            }
        }
        self.measured_seconds += count as f64 / self.sample_rate;
    }

    fn read(&mut self) -> LoudnessReading {
        self.refresh_stats_if_needed();
        let short_term = loudness_from_power(self.weighted_power(SHORT_TERM_BLOCKS)) as f32;
        let (display_left, display_right) = (self.left.ballistics.display(), self.right.ballistics.display());
        let current_true_peak = display_left.max(display_right);
        let (integrated, true_peak_max) = (self.cached_integrated, self.true_peak_max);
        LoudnessReading {
            momentary_lufs: loudness_from_power(self.weighted_power(MOMENTARY_BLOCKS)) as f32,
            short_term_lufs: short_term,
            integrated_lufs: integrated as f32,
            momentary_max_lufs: self.momentary_max as f32,
            short_term_max_lufs: self.short_term_max as f32,
            loudness_range_lu: self.cached_range as f32,
            true_peak_left_dbtp: display_left as f32,
            true_peak_right_dbtp: display_right as f32,
            true_peak_max_dbtp: true_peak_max as f32,
            rms_left_db: self.rms_db(&self.left.ring_raw) as f32,
            rms_right_db: self.rms_db(&self.right.ring_raw) as f32,
            plr_db: if integrated > FLOOR_DB && true_peak_max > FLOOR_DB {
                (true_peak_max - integrated) as f32
            } else {
                0.0
            },
            psr_db: if short_term as f64 > FLOOR_DB && current_true_peak > FLOOR_DB {
                (current_true_peak - short_term as f64) as f32
            } else {
                0.0
            },
            clip_count: self.clip_count,
            measured_seconds: self.measured_seconds,
            is_integrated_valid: integrated > FLOOR_DB,
        }
    }

    fn reset(&mut self) {
        for channel in [&mut self.left, &mut self.right] {
            channel.weighting.clear();
            channel.true_peak.clear();
            channel.clip_run = 0;
        }
        self.clear_windows();
        self.integrated_histogram.remove_all();
        self.range_histogram.remove_all();
        self.momentary_max = FLOOR_DB;
        self.short_term_max = FLOOR_DB;
        self.true_peak_max = FLOOR_DB;
        self.clip_count = 0;
        self.measured_seconds = 0.0;
        self.cached_integrated = FLOOR_DB;
        self.cached_range = 0.0;
        self.stats_dirty = false;
    }
}

// MARK: Scalar helpers

/// BS.1770 loudness from the weighted channel power sum. Silence reads the
/// floor, never NaN and never −inf.
pub(crate) fn loudness_from_power(power: f64) -> f64 {
    if !(power > 0.0 && power.is_finite()) {
        return FLOOR_DB;
    }
    let value = LOUDNESS_OFFSET + 10.0 * power.log10();
    if value.is_finite() {
        value.max(FLOOR_DB)
    } else {
        FLOOR_DB
    }
}

fn decibels_from_power(power: f64) -> f64 {
    if !(power > 0.0 && power.is_finite()) {
        return FLOOR_DB;
    }
    let value = 10.0 * power.log10();
    if value.is_finite() {
        value.max(FLOOR_DB)
    } else {
        FLOOR_DB
    }
}

fn decibels_from_amplitude(amplitude: f64) -> f64 {
    if !(amplitude > 0.0 && amplitude.is_finite()) {
        return FLOOR_DB;
    }
    let value = 20.0 * amplitude.log10();
    if value.is_finite() {
        value.max(FLOOR_DB)
    } else {
        FLOOR_DB
    }
}
