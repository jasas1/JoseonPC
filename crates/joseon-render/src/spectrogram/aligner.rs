//! Puts the bands of the spectrogram on one time axis.
//!
//! Port source: `Sources/JoseonRender/SpectrogramTimeAligner.swift`.
//!
//! The live spectrum is instant: every band shows its newest transform. The
//! window of the lows is 0.68 s long, the window of the highs 0.04 s, so the
//! same click reaches the highs about 0.3 s before it reaches the lows. The
//! spectrogram delays the short-window rows so that every row of a column has
//! the same window CENTER time. The delay follows the crossfade of the
//! analyzer, so there is no seam at the band borders.
//!
//! The second job is interpolation in time: a band that updates every 85 ms is
//! a staircase in the 60 Hz frame stream. Each row is averaged (in dB) over one
//! hop of its band, which is exactly a linear interpolation between transforms.
//!
//! Storage: a short ring of running time integrals per row (`f64`). A box
//! average over any interval is then two lookups per row.

/// Timing of the multi-resolution spectrum, mirrored from the analyzer: three
/// FFT windows of 32768 / 8192 / 2048 samples at 48 kHz, scaled to other rates
/// by the nearest power of two, hops of size / 8, size / 8 and size / 4,
/// blended with a smoothstep in log frequency over half an octave on each side
/// of 200 Hz and 2 kHz. Keep in step with the analyzer.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SpectrumTiming {
    /// Window length in seconds: low, mid, high.
    pub window: [f64; 3],
    /// Seconds between transforms: low, mid, high.
    pub hop: [f64; 3],
}

impl SpectrumTiming {
    pub const LOW_MID_HZ: f32 = 200.0;
    pub const MID_HIGH_HZ: f32 = 2_000.0;
    pub const CROSSFADE_OCTAVES: f32 = 0.5;

    pub fn new(sample_rate: f64) -> Self {
        let rate = if sample_rate > 1000.0 { sample_rate } else { 48_000.0 };
        let size = |at_48k: f64, lo: f64, hi: f64| 2f64.powf((at_48k * rate / 48_000.0).log2().round()).max(lo).min(hi);
        let l = size(32_768.0, 4_096.0, 131_072.0);
        let m = size(8_192.0, 1_024.0, 32_768.0);
        let h = size(2_048.0, 256.0, 8_192.0);
        Self {
            window: [l / rate, m / rate, h / rate],
            hop: [l / 8.0 / rate, m / 8.0 / rate, h / 4.0 / rate],
        }
    }

    /// Blend weights of the three resolutions at a frequency: low, mid, high.
    pub fn weights(f: f32) -> [f32; 3] {
        let step = |centre: f32| {
            let t = ((f.max(1e-3) / centre).log2() + Self::CROSSFADE_OCTAVES) / (2.0 * Self::CROSSFADE_OCTAVES);
            let c = t.max(0.0).min(1.0);
            c * c * (3.0 - 2.0 * c)
        };
        let wl = 1.0 - step(Self::LOW_MID_HZ);
        let wh = step(Self::MID_HIGH_HZ);
        [wl, (1.0 - wl - wh).max(0.0), wh]
    }

    /// Seconds from the middle of the analysis window to the moment a frame
    /// shows it: half the window plus half a hop.
    pub fn center_lag(&self, f: f32) -> f64 {
        let w = Self::weights(f);
        (0..3).map(|k| w[k] as f64 * (self.window[k] + self.hop[k]) / 2.0).sum()
    }

    pub fn hop_at(&self, f: f32) -> f64 {
        let w = Self::weights(f);
        (0..3).map(|k| w[k] as f64 * self.hop[k]).sum()
    }
}

/// Where a time falls in the ring: offsets of the two frames around it and the weight of the second.
#[derive(Clone, Copy)]
struct Cursor {
    s0: usize,
    s1: usize,
    w: f64,
    /// The time is at or outside an end of the ring: only `s0` counts.
    flat: bool,
}

pub struct SpectrogramTimeAligner {
    pub rows: usize,
    capacity: usize,
    /// capacity * rows: integral of the row value up to the time of the frame.
    prefix: Vec<f64>,
    /// Newest values.
    last: Vec<f32>,
    times: Vec<f64>,
    first: usize,
    count: usize,
    row_lag: Vec<f64>,
    row_hop: Vec<f64>,
    key: (f32, f32, f64),
    /// Half the time between frames: the picture of a frame stands until the next frame.
    pub frame_hold: f64,
    row_box: Vec<f64>,
    row_end: Vec<f64>,
    /// Rows with the same two times: start and count.
    runs: Vec<(usize, usize)>,
    box_for: f64,
    first_time: Option<f64>,
}

impl SpectrogramTimeAligner {
    pub fn new(rows: usize, capacity: usize) -> Self {
        Self {
            rows,
            capacity,
            prefix: vec![0.0; capacity * rows],
            last: vec![0.0; rows],
            times: vec![0.0; capacity],
            first: 0,
            count: 0,
            row_lag: Vec::new(),
            row_hop: Vec::new(),
            key: (0.0, 0.0, 0.0),
            frame_hold: 1.0 / 120.0,
            row_box: Vec::new(),
            row_end: Vec::new(),
            runs: Vec::new(),
            box_for: -1.0,
            first_time: None,
        }
    }

    /// Rows are uniform on a log axis from `min_hz` to `max_hz`.
    pub fn configure(&mut self, min_hz: f32, max_hz: f32, sample_rate: f64) {
        if self.key == (min_hz, max_hz, sample_rate) && !self.row_lag.is_empty() {
            return;
        }
        self.key = (min_hz, max_hz, sample_rate);
        let timing = SpectrumTiming::new(sample_rate);
        let rows = self.rows;
        let hz = |r: usize| min_hz * (max_hz / min_hz).powf(r as f32 / (rows - 1).max(1) as f32);
        self.row_lag = (0..rows).map(|r| timing.center_lag(hz(r))).collect();
        self.row_hop = (0..rows).map(|r| timing.hop_at(hz(r))).collect();
        self.box_for = -1.0;
    }

    /// One lag and one hop for every row: a single FFT resolution. `lag` =
    /// seconds from the middle of the analysis window to the moment a frame
    /// shows it, `hop` = seconds between two transforms.
    pub fn configure_uniform(&mut self, lag: f64, hop: f64) {
        if self.row_lag.len() == self.rows
            && self.row_lag[0] == lag
            && self.row_hop[0] == hop
            && self.key == (-1.0, -1.0, -1.0)
        {
            return;
        }
        self.key = (-1.0, -1.0, -1.0);
        self.row_lag = vec![lag; self.rows];
        self.row_hop = vec![hop; self.rows];
        self.box_for = -1.0;
    }

    pub fn reset(&mut self) {
        self.first = 0;
        self.count = 0;
        self.first_time = None;
    }

    /// Time of the first frame after the last reset.
    pub fn first_time(&self) -> Option<f64> {
        self.first_time
    }

    /// Every 16th row and the last one: enough to find the extremes of the lags.
    fn sampled_rows(&self) -> impl Iterator<Item = usize> + '_ {
        let n = self.rows.min(self.row_lag.len()).min(self.row_hop.len());
        (0..n).step_by(16).chain(self.row_lag.len().checked_sub(1))
    }

    /// The earliest column time that `compose` can make from real frames only.
    pub fn earliest_center(&self, min_box: f64) -> Option<f64> {
        let oldest = self.first_time?;
        if self.row_lag.is_empty() {
            return None;
        }
        let least = self
            .sampled_rows()
            .map(|r| self.row_lag[r] - self.row_hop[r].max(min_box) / 2.0)
            .fold(f64::INFINITY, f64::min);
        Some(oldest - least - self.frame_hold)
    }

    /// Seconds between the newest frame and the newest column that can be composed.
    pub fn latency(&self, min_box: f64) -> f64 {
        let m = self
            .sampled_rows()
            .map(|r| self.row_lag[r] + self.row_hop[r].max(min_box) / 2.0)
            .fold(0.0, f64::max);
        m + self.frame_hold
    }

    /// Delay of a row against the slowest row, in seconds.
    pub fn delay(&self, row: usize, min_box: f64) -> f64 {
        if row >= self.row_lag.len() || row >= self.row_hop.len() {
            return 0.0;
        }
        self.latency(min_box) - self.frame_hold - (self.row_lag[row] + self.row_hop[row].max(min_box) / 2.0)
    }

    /// Adds a frame: `values` are the row levels (0...1 of the dB range) at `time`. Times must rise.
    pub fn push(&mut self, values: &[f32], time: f64) {
        let (rows, capacity) = (self.rows, self.capacity);
        if self.count > 0 {
            let newest = self.times[(self.first + self.count - 1) % capacity];
            if time <= newest || time - newest > 0.5 {
                self.reset();
            }
        }
        if self.count == 0 {
            self.first_time = Some(time);
        }
        let slot = if self.count == capacity {
            let slot = self.first;
            self.first = (self.first + 1) % capacity;
            slot
        } else {
            let slot = (self.first + self.count) % capacity;
            self.count += 1;
            slot
        };
        if self.count == 1 {
            self.prefix[slot * rows..(slot + 1) * rows].fill(0.0);
        } else {
            let previous = (slot + capacity - 1) % capacity;
            let dt = time - self.times[previous];
            for r in 0..rows {
                self.prefix[slot * rows + r] = self.last[r] as f64 * dt + self.prefix[previous * rows + r];
            }
        }
        self.times[slot] = time;
        self.last.copy_from_slice(&values[..rows]);
    }

    /// Time of the newest frame.
    pub fn newest_time(&self) -> f64 {
        if self.count > 0 {
            self.times[(self.first + self.count - 1) % self.capacity]
        } else {
            0.0
        }
    }

    /// Writes the column whose window centers are at `center_time`. `min_box`:
    /// shortest averaging time (a column or a frame). Later times than
    /// `newest_time() - latency(min_box)` clamp to the newest frame.
    pub fn compose(&mut self, center_time: f64, min_box: f64, out: &mut [f32]) {
        let rows = self.rows;
        if self.count < 2 || self.row_lag.is_empty() {
            out[..rows].fill(0.0);
            return;
        }
        if self.box_for != min_box {
            self.box_for = min_box;
            // Half-millisecond steps (a column is 17 ms or more): neighbour rows share their times.
            self.row_box = self
                .row_hop
                .iter()
                .map(|h| (h.max(min_box) * 2000.0).round() / 2000.0)
                .collect();
            self.row_end = self
                .row_lag
                .iter()
                .zip(&self.row_box)
                .map(|(lag, b)| ((lag + b / 2.0) * 2000.0).round() / 2000.0)
                .collect();
            self.runs.clear();
            let mut start = 0;
            for r in 1..=rows {
                if r == rows || self.row_box[r] != self.row_box[start] || self.row_end[r] != self.row_end[start] {
                    self.runs.push((start, r - start));
                    start = r;
                }
            }
        }
        let hold = self.frame_hold;
        for &(r0, n) in &self.runs {
            let width = self.row_box[r0];
            let end = center_time + self.row_end[r0] + hold;
            let from = self.locate(end - width);
            let to = self.locate(end);
            let at = |c: &Cursor, r: usize, weight: f64| {
                let p0 = self.prefix[c.s0 + r];
                if c.flat {
                    p0
                } else {
                    p0 + (self.prefix[c.s1 + r] - p0) * weight
                }
            };
            if n >= 16 {
                // The macOS app multiplies a long run by 1 / box. The same here, so the bits are the same.
                let inverse = 1.0 / width;
                for r in r0..r0 + n {
                    out[r] = ((at(&to, r, to.w) - at(&from, r, from.w)) * inverse) as f32;
                }
            } else {
                for r in r0..r0 + n {
                    out[r] = ((at(&to, r, to.w) - at(&from, r, from.w)) / width) as f32;
                }
            }
        }
    }

    fn locate(&self, asked: f64) -> Cursor {
        let (n, cap, f0, rows) = (self.count, self.capacity, self.first, self.rows);
        let newest = self.times[(f0 + n - 1) % cap];
        let oldest = self.times[f0];
        let t = asked.min(newest);
        if t <= oldest {
            return Cursor {
                s0: f0 * rows,
                s1: 0,
                w: 0.0,
                flat: true,
            };
        }
        // A guess from the mean frame step, then a short walk.
        let mean_step = ((newest - oldest) / (n - 1) as f64).max(1e-4);
        let mut k = (n as i64 - 1 - ((newest - t) / mean_step) as i64)
            .max(0)
            .min(n as i64 - 1) as usize;
        while k < n - 1 && self.times[(f0 + k + 1) % cap] <= t {
            k += 1;
        }
        while k > 0 && self.times[(f0 + k) % cap] > t {
            k -= 1;
        }
        if k >= n - 1 {
            return Cursor {
                s0: ((f0 + n - 1) % cap) * rows,
                s1: 0,
                w: 0.0,
                flat: true,
            };
        }
        let (a, b) = ((f0 + k) % cap, (f0 + k + 1) % cap);
        Cursor {
            s0: a * rows,
            s1: b * rows,
            w: (t - self.times[a]) / (self.times[b] - self.times[a]).max(1e-9),
            flat: false,
        }
    }
}
