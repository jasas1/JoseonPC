//! The log frequency axis, and the resampling of a spectrum onto plot points.
//!
//! Port source: `LogAxis`, `ResampleTable` and `CurveResampler` at the end of
//! `Sources/JoseonRender/PanelRenderer.swift`.

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LogAxis {
    pub min_hz: f32,
    pub max_hz: f32,
}

impl LogAxis {
    pub const LABELED: [f32; 11] = [
        20.0, 30.0, 50.0, 100.0, 200.0, 500.0, 1000.0, 2000.0, 5000.0, 10_000.0, 20_000.0,
    ];
    pub const MINOR: [f32; 18] = [
        10.0, 40.0, 60.0, 70.0, 80.0, 90.0, 300.0, 400.0, 600.0, 700.0, 800.0, 900.0, 3000.0, 4000.0, 6000.0, 7000.0,
        8000.0, 9000.0,
    ];

    /// 0 at `min_hz`, 1 at `max_hz`.
    pub fn position(&self, hz: f32) -> f32 {
        (hz.max(1e-3) / self.min_hz).ln() / (self.max_hz / self.min_hz).ln()
    }

    pub fn frequency(&self, t: f32) -> f32 {
        self.min_hz * (self.max_hz / self.min_hz).powf(t)
    }
}

/// The fixed part of a resampling job: for every output point the four source
/// bins and their Catmull-Rom weights, or the range of bins to take the maximum
/// of. Bins, axis and point count stay the same from frame to frame, so the
/// table is made once and a frame costs four multiply-adds per point. Same
/// results as `resample`.
pub struct ResampleTable {
    pub count: usize,
    pub source_count: usize,
    key: (f32, f32, f32, f32),
    /// 4 per point: p0 p1 p2 p3, or lo hi - - in max mode; -1 = outside.
    index: Vec<i32>,
    /// 4 per point.
    weight: Vec<f32>,
    max_mode: bool,
}

impl ResampleTable {
    pub fn matches(&self, frequencies: &[f32], axis: LogAxis, count: usize) -> bool {
        let n = frequencies.len();
        count == self.count
            && n == self.source_count
            && n >= 2
            && self.key == (frequencies[0], frequencies[n - 1], axis.min_hz, axis.max_hz)
    }

    pub fn new(frequencies: &[f32], axis: LogAxis, count: usize) -> Option<Self> {
        let n = frequencies.len();
        if !(n >= 2 && count >= 2 && frequencies[0] > 0.0 && frequencies[n - 1] > frequencies[0]) {
            return None;
        }
        let mut index = vec![0i32; count * 4];
        let mut weight = vec![0f32; count * 4];
        let ln_f0 = frequencies[0].ln();
        let ln_span = frequencies[n - 1].ln() - ln_f0;
        let ln_a = axis.min_hz.ln();
        let ln_a_span = axis.max_hz.ln() - ln_a;
        let bins_per_point = (ln_a_span / (count - 1) as f32) / (ln_span / (n - 1) as f32);
        let max_mode = bins_per_point > 1.25;
        for j in 0..count {
            let ln_f = ln_a + ln_a_span * j as f32 / (count - 1) as f32;
            let idx = (ln_f - ln_f0) / ln_span * (n - 1) as f32;
            let o = j * 4;
            if idx < -0.5 || idx > n as f32 - 0.5 {
                index[o..o + 4].fill(-1);
            } else if max_mode {
                let mut lo = ((idx - bins_per_point * 0.5).ceil() as i64).max(0);
                let mut hi = ((idx + bins_per_point * 0.5).floor() as i64).min(n as i64 - 1);
                if lo > hi {
                    lo = (idx.round() as i64).max(0).min(n as i64 - 1);
                    hi = lo;
                }
                index[o] = lo as i32;
                index[o + 1] = hi as i32;
            } else {
                let c = idx.max(0.0).min((n - 1) as f32);
                let i1 = (c as usize).min(n - 2);
                let t = c - i1 as f32;
                index[o] = i1.saturating_sub(1) as i32;
                index[o + 1] = i1 as i32;
                index[o + 2] = (i1 + 1) as i32;
                index[o + 3] = (i1 + 2).min(n - 1) as i32;
                let (t2, t3) = (t * t, t * t * t);
                weight[o] = 0.5 * (-t + 2.0 * t2 - t3);
                weight[o + 1] = 0.5 * (2.0 - 5.0 * t2 + 3.0 * t3);
                weight[o + 2] = 0.5 * (t + 4.0 * t2 - 3.0 * t3);
                weight[o + 3] = 0.5 * (-t2 + t3);
            }
        }
        Some(Self {
            count,
            source_count: n,
            key: (frequencies[0], frequencies[n - 1], axis.min_hz, axis.max_hz),
            index,
            weight,
            max_mode,
        })
    }

    /// Writes `count` values, 0 (`min_db`) ... 1 (`max_db`).
    pub fn apply(&self, src: &[f32], min_db: f32, max_db: f32, offset_db: f32, out: &mut [f32]) {
        let out = &mut out[..self.count];
        if src.len() < self.source_count {
            out.fill(0.0);
            return;
        }
        let scale = 1.0 / (max_db - min_db);
        let bias = offset_db - min_db;
        if self.max_mode {
            for (j, slot) in out.iter_mut().enumerate() {
                let (lo, hi) = (self.index[j * 4], self.index[j * 4 + 1]);
                if lo < 0 {
                    *slot = 0.0;
                    continue;
                }
                let mut db = src[lo as usize];
                for k in (lo + 1)..=hi {
                    db = db.max(src[k as usize]);
                }
                *slot = ((db + bias) * scale).max(0.0).min(1.0);
            }
        } else {
            for (j, slot) in out.iter_mut().enumerate() {
                let o = j * 4;
                let i0 = self.index[o];
                if i0 < 0 {
                    *slot = 0.0;
                    continue;
                }
                let p1 = src[self.index[o + 1] as usize];
                let p2 = src[self.index[o + 2] as usize];
                let mut db = self.weight[o] * src[i0 as usize]
                    + self.weight[o + 1] * p1
                    + self.weight[o + 2] * p2
                    + self.weight[o + 3] * src[self.index[o + 3] as usize];
                // No overshoot past the local range: keeps peaks honest.
                db = db.max(p1.min(p2) - 0.5).min(p1.max(p2) + 0.5);
                *slot = ((db + bias) * scale).max(0.0).min(1.0);
            }
        }
    }
}

/// Resamples a log-spaced spectrum onto `out.len()` points that are uniform on
/// a log axis. More points than bins: Catmull-Rom (smooth, never stair-stepped).
/// Fewer: the maximum of the covered bins (peaks stay).
pub fn resample(
    src: &[f32],
    frequencies: &[f32],
    axis: LogAxis,
    min_db: f32,
    max_db: f32,
    offset_db: f32,
    out: &mut [f32],
) {
    let n = src.len().min(frequencies.len());
    let count = out.len();
    if !(n >= 2 && count >= 2 && frequencies[0] > 0.0 && frequencies[n - 1] > frequencies[0]) {
        out.fill(0.0);
        return;
    }
    let ln_f0 = frequencies[0].ln();
    let ln_span = frequencies[n - 1].ln() - ln_f0;
    let ln_a = axis.min_hz.ln();
    let ln_a_span = axis.max_hz.ln() - ln_a;
    let bins_per_point = (ln_a_span / (count - 1) as f32) / (ln_span / (n - 1) as f32);
    let range = max_db - min_db;
    for (j, slot) in out.iter_mut().enumerate() {
        let ln_f = ln_a + ln_a_span * j as f32 / (count - 1) as f32;
        let idx = (ln_f - ln_f0) / ln_span * (n - 1) as f32;
        let mut db: f32;
        if idx < -0.5 || idx > n as f32 - 0.5 {
            db = -1000.0;
        } else if bins_per_point > 1.25 {
            let lo = ((idx - bins_per_point * 0.5).ceil() as i64).max(0);
            let hi = ((idx + bins_per_point * 0.5).floor() as i64).min(n as i64 - 1);
            db = -1000.0;
            if lo <= hi {
                for k in lo..=hi {
                    db = db.max(src[k as usize]);
                }
            } else {
                db = src[(idx.round() as i64).max(0).min(n as i64 - 1) as usize];
            }
        } else {
            let c = idx.max(0.0).min((n - 1) as f32);
            let i1 = (c as usize).min(n - 2);
            let t = c - i1 as f32;
            let (p0, p1, p2, p3) = (
                src[i1.saturating_sub(1)],
                src[i1],
                src[i1 + 1],
                src[(i1 + 2).min(n - 1)],
            );
            let a = 2.0 * p1;
            let b = p2 - p0;
            let cc = 2.0 * p0 - 5.0 * p1 + 4.0 * p2 - p3;
            let d = -p0 + 3.0 * p1 - 3.0 * p2 + p3;
            db = 0.5 * (a + b * t + cc * t * t + d * t * t * t);
            // No overshoot past the local range: keeps peaks honest.
            db = db.max(p1.min(p2) - 0.5).min(p1.max(p2) + 0.5);
        }
        *slot = ((db + offset_db - min_db) / range).max(0.0).min(1.0);
    }
}
