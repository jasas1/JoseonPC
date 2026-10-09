//! Fixed-size loudness distribution.
//!
//! Port source: `Sources/JoseonCore/Loudness/LoudnessHistogram.swift`.
//!
//! Gating needs the mean power of the blocks that pass a threshold, and EBU
//! Tech 3342 needs percentiles. Both come from a histogram, so memory stays
//! constant no matter how long the measurement runs. Each bin keeps a count and
//! the exact sum of the block powers that landed in it, so the gated mean is
//! exact. Only the threshold comparison is quantised, to one bin.

/// Bin width in LU.
pub(crate) const BIN_WIDTH: f64 = 0.05;
/// Bottom of the histogram, which is also the BS.1770 absolute gate.
pub(crate) const MIN_LUFS: f64 = -70.0;
/// −70 LUFS up to +30 LUFS.
pub(crate) const BIN_COUNT: usize = 2000;

#[derive(Debug, Clone)]
pub(crate) struct LoudnessHistogram {
    counts: Vec<u32>,
    powers: Vec<f64>,
    total_count: u64,
    total_power: f64,
}

impl LoudnessHistogram {
    pub fn new() -> Self {
        Self {
            counts: vec![0; BIN_COUNT],
            powers: vec![0.0; BIN_COUNT],
            total_count: 0,
            total_power: 0.0,
        }
    }

    pub fn remove_all(&mut self) {
        self.counts.fill(0);
        self.powers.fill(0.0);
        self.total_count = 0;
        self.total_power = 0.0;
    }

    pub fn total_count(&self) -> u64 {
        self.total_count
    }

    pub fn total_power(&self) -> f64 {
        self.total_power
    }

    /// Bins in the histogram. It never grows.
    #[cfg(test)]
    pub fn bin_storage(&self) -> (usize, usize) {
        (self.counts.len(), self.powers.len())
    }

    /// Loudness that represents bin `index` (its centre).
    fn loudness_of_bin(index: usize) -> f64 {
        MIN_LUFS + (index as f64 + 0.5) * BIN_WIDTH
    }

    /// First bin that can hold `loudness`. Clamped to the histogram.
    fn bin_for(loudness: f64) -> usize {
        let raw = ((loudness - MIN_LUFS) / BIN_WIDTH).floor();
        if raw >= (BIN_COUNT - 1) as f64 {
            BIN_COUNT - 1
        } else if raw > 0.0 {
            raw as usize
        } else {
            0
        }
    }

    /// Adds one gating block. `power` is the BS.1770 weighted channel sum for
    /// the block. `loudness` is −0.691 + 10 log10(power). Blocks at or below
    /// the absolute gate are dropped.
    pub fn add(&mut self, loudness: f64, power: f64) {
        if !(loudness.is_finite() && loudness > MIN_LUFS && power > 0.0) {
            return;
        }
        let index = Self::bin_for(loudness);
        self.counts[index] += 1;
        self.powers[index] += power;
        self.total_count += 1;
        self.total_power += power;
    }

    /// Count and power of every block at or above `threshold` LUFS.
    pub fn gated(&self, threshold: f64) -> (u64, f64) {
        let (mut count, mut power) = (0u64, 0.0);
        for i in Self::bin_for(threshold)..BIN_COUNT {
            if self.counts[i] != 0 {
                count += self.counts[i] as u64;
                power += self.powers[i];
            }
        }
        (count, power)
    }

    /// Loudness at percentile `fraction` (0...1) of the blocks at or above
    /// `threshold`. `None` when nothing passes the threshold.
    pub fn percentile(&self, fraction: f64, threshold: f64) -> Option<f64> {
        let start = Self::bin_for(threshold);
        let total: u64 = self.counts[start..].iter().map(|&c| c as u64).sum();
        if total == 0 {
            return None;
        }
        let target = ((total - 1) as f64 * fraction).round() as u64;
        let mut seen = 0u64;
        for i in start..BIN_COUNT {
            seen += self.counts[i] as u64;
            if seen > target {
                return Some(Self::loudness_of_bin(i));
            }
        }
        Some(Self::loudness_of_bin(BIN_COUNT - 1))
    }
}
