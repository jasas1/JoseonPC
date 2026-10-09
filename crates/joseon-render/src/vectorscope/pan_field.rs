//! Stereo placement by frequency: where each display bin sits between left and
//! right, and how much light it gets.
//!
//! pan = (|R|^2 - |L|^2) / (|R|^2 + |L|^2) from per-bin POWER, smoothed over
//! about 150 ms. A tone in one channel is a dot at the far side; the same level
//! in both is the center. The light follows the bin power; bins under the gate
//! get none. Under about 120 Hz several display bins share one FFT bin of the
//! long window: L and R are interpolated apart, and the pan of one display bin
//! zigzags. There the powers are averaged over the width of an FFT bin first.
//!
//! Tones: the skirt of a tone's lobe sinks into the noise, and the noise sits in
//! the center. Bin by bin, a panned tone therefore drew a streak from its true
//! place to the center. A bin that belongs to a tonal peak (a local maximum 8 dB
//! or more over its 1/6-octave surroundings, and the lobe downhill from it)
//! takes ONE pan: that of the peak bin, from the power of the tone alone (the
//! local noise of each channel is subtracted), as a median over about 200 ms.
//! Noise-like bins keep the smoothed-power pan.
//!
//! Port source: `Sources/JoseonRender/PanField.swift`.

use crate::spectrogram::aligner::SpectrumTiming;
use joseon_core::contracts::SpectrumReading;

const CAPACITY: usize = 8192;
const HISTORY_LENGTH: usize = 16;

pub struct PanField {
    /// Time constant of the power smoothing, seconds.
    pub smoothing_seconds: f64,
    /// Bins whose smoothed L + R power is under this get no light.
    pub gate_db: f32,
    /// Length of the time median of a tone's pan, seconds.
    pub median_seconds: f64,
    /// A peak is tonal when it stands this far over the mean level of +-1/6 octave around it.
    pub tonal_prominence_db: f32,
    /// A bin of a tone's skirt more than this under its peak gets no light.
    pub skirt_drop_db: f32,
    /// Under this frequency the pan is damped toward the center by its confidence, fading out to `confidence_fade_hz`.
    pub confidence_hz: f32,
    pub confidence_fade_hz: f32,
    /// Time constant of the pan statistics behind the confidence, seconds.
    pub confidence_seconds: f64,
    /// A tone that was there in the last `tonal_hold_seconds` stays a tone down to this prominence: a tone that
    /// breathes must not flicker between a dot and a streak.
    pub tonal_hold_prominence_db: f32,
    pub tonal_hold_seconds: f64,

    count: usize,
    /// -1 (left) ... +1 (right) per display bin.
    pan: Vec<f32>,
    /// 10 log10(L power + R power), smoothed, per display bin.
    level: Vec<f32>,
    pow_l: Vec<f32>,
    pow_r: Vec<f32>,
    tmp_l: Vec<f32>,
    tmp_r: Vec<f32>,
    /// Half-width of the frequency averaging per bin, in display bins (0 over about 150 Hz).
    spread: Vec<i32>,
    /// 1 where the bin belongs to the lobe of a tonal peak (newest update).
    tonal: Vec<u8>,
    /// Peak bins of the newest update.
    tonal_peaks: Vec<usize>,
    /// 1 where the bin is the skirt of a tone: it gets no light. The skirt is the tone's own leakage. Where it sinks
    /// into the centered noise its pan lies anywhere between the tone and the center, which drew a streak from the
    /// center to every hard-panned tone. The tone itself is already drawn at its place.
    dropped: Vec<u8>,
    /// Confidence 0...1 per bin (1 over `confidence_fade_hz`), newest update. Two factors:
    /// level: the bin stands over the local floor (the lowest level within half an octave), 3 dB = 0, 12 dB = 1;
    /// coherence: E[p]^2 / E[p^2] of the frame-by-frame pan p over `confidence_seconds`. L and R that carry the same
    /// sound give the same pan in every frame (ratio 1). Independent noise in L and R gives a pan that wanders
    /// around 0 (ratio near 0): that wander is estimation noise, not placement.
    confidence: Vec<f32>,
    pan_mean: Vec<f32>,
    pan_square: Vec<f32>,
    frame_pow_l: Vec<f32>,
    frame_pow_r: Vec<f32>,
    confidence_bins: usize,
    confidence_primed: bool,

    /// Tone pans of peak bins, and the frame number of each entry (0 = empty).
    pan_history: Vec<f32>,
    pan_stamp: Vec<i32>,
    frame_number: i32,
    median_scratch: [f32; 64],
    prefix: Vec<f32>,
    prefix_l: Vec<f32>,
    prefix_r: Vec<f32>,
    /// Frame number when a bin was last the peak of a tone (hysteresis), 0 = never.
    last_tonal: Vec<i32>,
    /// The newest trusted tone pan per bin, and its frame number (0 = none).
    last_pan: Vec<f32>,
    last_pan_frame: Vec<i32>,
    bins_per_octave: f32,
    alpha: Vec<f32>,
    alpha_dt: f64,
    band_window: Vec<f32>,
    primed: bool,
    key: (usize, f32, f32, f64),
}

impl Default for PanField {
    fn default() -> Self {
        Self::new()
    }
}

impl PanField {
    pub fn new() -> Self {
        Self {
            smoothing_seconds: 0.15,
            gate_db: -70.0,
            median_seconds: 0.2,
            tonal_prominence_db: 8.0,
            skirt_drop_db: 12.0,
            confidence_hz: 40.0,
            confidence_fade_hz: 60.0,
            confidence_seconds: 3.0,
            tonal_hold_prominence_db: 2.5,
            tonal_hold_seconds: 1.5,
            count: 0,
            pan: vec![0.0; CAPACITY],
            level: vec![0.0; CAPACITY],
            pow_l: vec![0.0; CAPACITY],
            pow_r: vec![0.0; CAPACITY],
            tmp_l: vec![0.0; CAPACITY],
            tmp_r: vec![0.0; CAPACITY],
            spread: Vec::new(),
            tonal: Vec::new(),
            tonal_peaks: Vec::with_capacity(64),
            dropped: Vec::new(),
            confidence: Vec::new(),
            pan_mean: Vec::new(),
            pan_square: Vec::new(),
            frame_pow_l: Vec::new(),
            frame_pow_r: Vec::new(),
            confidence_bins: 0,
            confidence_primed: false,
            pan_history: vec![0.0; CAPACITY * HISTORY_LENGTH],
            pan_stamp: vec![0; CAPACITY * HISTORY_LENGTH],
            frame_number: 0,
            median_scratch: [0.0; 64],
            prefix: Vec::new(),
            prefix_l: Vec::new(),
            prefix_r: Vec::new(),
            last_tonal: Vec::new(),
            last_pan: Vec::new(),
            last_pan_frame: Vec::new(),
            bins_per_octave: 90.0,
            alpha: Vec::new(),
            alpha_dt: -1.0,
            band_window: Vec::new(),
            primed: false,
            key: (0, 0.0, 0.0, 0.0),
        }
    }

    pub fn count(&self) -> usize {
        self.count
    }

    pub fn pan(&self) -> &[f32] {
        &self.pan[..self.count]
    }

    pub fn level_db(&self) -> &[f32] {
        &self.level[..self.count]
    }

    pub fn spread(&self) -> &[i32] {
        &self.spread
    }

    pub fn tonal(&self) -> &[u8] {
        &self.tonal
    }

    pub fn tonal_peaks(&self) -> &[usize] {
        &self.tonal_peaks
    }

    pub fn dropped(&self) -> &[u8] {
        &self.dropped
    }

    pub fn confidence(&self) -> &[f32] {
        &self.confidence
    }

    pub fn reset(&mut self) {
        self.primed = false;
        self.pan_stamp.fill(0);
        self.last_tonal.fill(0);
        self.last_pan_frame.fill(0);
    }

    pub fn update(&mut self, s: &SpectrumReading, dt: f64, sample_rate: f64) {
        let n = s.left.len().min(s.right.len()).min(s.frequencies.len()).min(CAPACITY);
        if !(n >= 8 && s.frequencies[0] > 0.0 && s.frequencies[n - 1] > s.frequencies[0]) {
            self.count = 0;
            return;
        }
        let key = (n, s.frequencies[0], s.frequencies[n - 1], sample_rate);
        if self.key != key {
            self.key = key;
            self.count = n;
            let timing = SpectrumTiming::new(sample_rate);
            // Width of one FFT bin of the long window, in display bins.
            let df = (1.0 / timing.window[0]) as f32;
            let bins_per_octave = (n - 1) as f32 / (s.frequencies[n - 1] / s.frequencies[0]).log2();
            self.bins_per_octave = bins_per_octave;
            self.tonal = vec![0; n];
            self.dropped = vec![0; n];
            self.confidence = vec![1.0; n];
            self.confidence_bins = s.frequencies[..n]
                .iter()
                .position(|f| *f >= self.confidence_fade_hz)
                .unwrap_or(0);
            self.pan_mean = vec![0.0; self.confidence_bins];
            self.pan_square = vec![0.0; self.confidence_bins];
            self.confidence_primed = false;
            self.prefix = vec![0.0; n + 1];
            self.prefix_l = vec![0.0; n + 1];
            self.prefix_r = vec![0.0; n + 1];
            self.last_tonal = vec![0; n];
            self.last_pan = vec![0.0; n];
            self.last_pan_frame = vec![0; n];
            self.pan_stamp.fill(0);
            self.spread = s.frequencies[..n]
                .iter()
                .map(|f| {
                    let w = df / (f * std::f32::consts::LN_2) * bins_per_octave; // display bins per FFT bin
                    if *f < 150.0 {
                        w.round().min(14.0) as i32
                    } else {
                        0
                    }
                })
                .collect();
            // The analysis window behind each bin: a pan reading cannot settle faster than its window renews.
            self.band_window = s.frequencies[..n]
                .iter()
                .map(|f| {
                    let w = SpectrumTiming::weights(*f);
                    w[0] * timing.window[0] as f32 + w[1] * timing.window[1] as f32 + w[2] * timing.window[2] as f32
                })
                .collect();
            self.alpha_dt = -1.0;
            self.primed = false;
        }
        let k = (std::f64::consts::LN_10 / 10.0) as f32;
        if self.primed {
            // One pole: p += (new - p) * a
            // Time constant per bin: `smoothing_seconds`, or 1.2 analysis windows where that is longer (the lows).
            if (dt - self.alpha_dt).abs() > 1e-4 {
                self.alpha_dt = dt;
                let smoothing = self.smoothing_seconds;
                self.alpha = self
                    .band_window
                    .iter()
                    .map(|w| (1.0 - (-dt.max(0.0) / smoothing.max(*w as f64 * 1.2).max(1e-3)).exp()) as f32)
                    .collect();
            }
            for i in 0..n {
                let l = (s.left[i] * k).exp();
                let r = (s.right[i] * k).exp();
                self.pow_l[i] += (l - self.pow_l[i]) * self.alpha[i];
                self.pow_r[i] += (r - self.pow_r[i]) * self.alpha[i];
            }
        } else {
            for i in 0..n {
                self.pow_l[i] = (s.left[i] * k).exp();
                self.pow_r[i] = (s.right[i] * k).exp();
            }
        }
        self.primed = true;
        // Frequency averaging under 150 Hz, plain copy above. tmp_l = L, tmp_r = R after this.
        self.tmp_l[..n].copy_from_slice(&self.pow_l[..n]);
        self.tmp_r[..n].copy_from_slice(&self.pow_r[..n]);
        let mut i = 0;
        while i < n && self.spread[i] > 0 {
            let r = self.spread[i] as usize;
            let (lo, hi) = (i.saturating_sub(r), (i + r).min(n - 1));
            let (mut l, mut rr) = (0.0f32, 0.0f32);
            for j in lo..=hi {
                l += self.pow_l[j];
                rr += self.pow_r[j];
            }
            self.tmp_l[i] = l / (hi - lo + 1) as f32;
            self.tmp_r[i] = rr / (hi - lo + 1) as f32;
            i += 1;
        }
        for i in 0..n {
            let difference = self.tmp_r[i] - self.tmp_l[i]; // R - L
            let total = self.tmp_l[i] + self.tmp_r[i] + 1e-30;
            self.tmp_l[i] = total;
            self.pan[i] = difference / total;
            self.level[i] = total.log10() * 10.0;
        }
        self.damp_lows_by_confidence(s, n, dt);
        // tmp_l = total, tmp_r = R (frequency averaged): L = total - R.
        self.place_tones(n, dt, &s.left, &s.right);
    }

    /// Under 40 Hz: pan *= confidence (see `confidence`). Tones are placed after this and keep their own pan.
    fn damp_lows_by_confidence(&mut self, s: &SpectrumReading, n: usize, dt: f64) {
        let m = self.confidence_bins.min(n);
        if m == 0 {
            return;
        }
        let k = (1.0 - (-dt.max(0.0) / self.confidence_seconds.max(1e-3)).exp()) as f32;
        let half = ((self.bins_per_octave / 2.0) as usize).max(1);
        // This frame's powers of the bins the lows average over.
        let reach_bins = (m + 15).min(n);
        if self.frame_pow_l.len() != reach_bins {
            self.frame_pow_l = vec![0.0; reach_bins];
            self.frame_pow_r = vec![0.0; reach_bins];
        }
        for j in 0..reach_bins {
            self.frame_pow_l[j] = 10f32.powf(s.left[j] / 10.0);
            self.frame_pow_r[j] = 10f32.powf(s.right[j] / 10.0);
        }
        let level = &self.level;
        for i in 0..m {
            // This frame's pan from this frame's powers, averaged over the same bins as the smoothed pan.
            let r = self.spread[i] as usize;
            let (lo, hi) = (i.saturating_sub(r), (i + r).min(n - 1));
            let (mut l, mut rr) = (0.0f32, 0.0f32);
            for j in lo..=hi.min(reach_bins - 1) {
                l += self.frame_pow_l[j];
                rr += self.frame_pow_r[j];
            }
            let p = (rr - l) / (rr + l).max(1e-30);
            if self.confidence_primed {
                self.pan_mean[i] += (p - self.pan_mean[i]) * k;
                self.pan_square[i] += (p * p - self.pan_square[i]) * k;
            } else {
                // One reading says nothing about steadiness: start without confidence in a pan away from the center.
                self.pan_mean[i] = p;
                self.pan_square[i] = p * p + 0.05;
            }
            let coherence = (self.pan_mean[i] * self.pan_mean[i] / self.pan_square[i].max(1e-6)).min(1.0);
            let mut floor = level[i];
            let mut j = i.saturating_sub(half);
            while j <= (i + half).min(n - 1) {
                floor = floor.min(level[j]);
                j += 2;
            }
            // A flat stretch has no floor to stand over: the level factor asks only that the bin is not the hole itself.
            let over = ((level[i] - floor - 3.0) / 9.0).max(0.0).min(1.0);
            let flat = level[(i + half).min(n - 1)] - floor < 6.0 && level[i.saturating_sub(half)] - floor < 6.0;
            let c = coherence * if flat { 1.0 } else { over };
            let f = s.frequencies[i];
            let fade = ((f - self.confidence_hz) / (self.confidence_fade_hz - self.confidence_hz).max(1.0))
                .max(0.0)
                .min(1.0);
            self.confidence[i] = c + (1.0 - c) * fade;
            self.pan[i] *= self.confidence[i];
        }
        self.confidence_primed = true;
    }

    /// Finds the tonal peaks and gives every bin of a tone's lobe the pan of the tone.
    /// A peak is a local maximum of the smoothed L + R level. Its prominence is the largest of three: in L + R, in
    /// L alone and in R alone (a tone in one channel stands 3 dB higher over that channel's noise than over the
    /// noise of both).
    fn place_tones(&mut self, n: usize, dt: f64, left: &[f32], right_db: &[f32]) {
        self.frame_number = self.frame_number.wrapping_add(1);
        if self.frame_number <= 0 || self.frame_number > i32::MAX - 8 {
            self.frame_number = 1;
            self.pan_stamp.fill(0);
            self.last_tonal[..n].fill(0);
            self.last_pan_frame[..n].fill(0);
        }
        let frame_number = self.frame_number;
        for i in 0..n {
            self.tonal[i] = 0;
            self.dropped[i] = 0;
            self.prefix[i + 1] = self.prefix[i] + self.level[i];
            self.prefix_l[i + 1] = self.prefix_l[i] + left[i];
            self.prefix_r[i + 1] = self.prefix_r[i] + right_db[i];
        }
        self.tonal_peaks.clear();
        let level = &self.level;
        let total = &self.tmp_l;
        let right = &self.tmp_r;
        let gate = self.gate_db;
        let r = ((self.bins_per_octave / 6.0).round() as usize).max(3);
        let reach = r * 2;
        let flank = (r / 2).max(2);
        let frames = ((self.median_seconds / dt.max(1e-3)).round() as i64).clamp(1, HISTORY_LENGTH as i64) as i32;
        let hold_frames = (self.tonal_hold_seconds / dt.max(1e-3)).max(2.0) as i32;
        let max3 = |v: &[f32], i: usize| v[i - 1].max(v[i]).max(v[i + 1]);
        let mut i = 2;
        while i + 2 < n {
            let v = level[i];
            if !(v > gate && v >= level[i - 1] && v > level[i + 1]) {
                i += 1;
                continue;
            }
            let (a0, b0) = (i.saturating_sub(r), (i + r).min(n - 1));
            let c0 = (b0 - a0 + 1) as f32;
            let prominence = (v - (self.prefix[b0 + 1] - self.prefix[a0]) / c0)
                .max(max3(left, i) - (self.prefix_l[b0 + 1] - self.prefix_l[a0]) / c0)
                .max(max3(right_db, i) - (self.prefix_r[b0 + 1] - self.prefix_r[a0]) / c0);
            let held = self.last_tonal[i - 1]
                .max(self.last_tonal[i])
                .max(self.last_tonal[i + 1]);
            let was_tonal = held > 0 && frame_number - held <= hold_frames;
            let needed = if was_tonal {
                self.tonal_hold_prominence_db
            } else {
                self.tonal_prominence_db
            };
            if !(prominence >= needed) {
                i += 1;
                continue;
            }
            // Only a full detection renews the hold: a held tone cannot hold itself for ever.
            if prominence >= self.tonal_prominence_db {
                self.last_tonal[i] = frame_number;
            }
            // The lobe: downhill from the peak, at most 1/3 octave, not under the gate.
            let (mut a, mut b) = (i, i);
            // The skirt of a lobe falls steeply; where the fall flattens (under 0.8 dB over two bins) the background
            // begins, and the background keeps its own pan.
            let falls = |from: usize, step: isize| {
                let next = (from as isize + step) as usize;
                let after = (next as isize + step).clamp(0, n as isize - 1) as usize;
                level[next] < level[from] + 0.3 && level[after] < level[from] - 0.8
            };
            while a > 0 && i - a < reach && falls(a, -1) && level[a - 1] > gate - 6.0 && self.tonal[a - 1] == 0 {
                a -= 1;
            }
            while b < n - 1 && b - i < reach && falls(b, 1) && level[b + 1] > gate - 6.0 {
                b += 1;
            }
            // Noise of each channel beside the lobe (mean power of the two flanks). A flank inside another tone does
            // not count.
            let (mut nt, mut nr, mut c) = (0.0f32, 0.0f32, 0.0f32);
            for j in a.saturating_sub(flank)..a {
                if self.tonal[j] == 0 {
                    nt += total[j];
                    nr += right[j];
                    c += 1.0;
                }
            }
            if b + 1 < n {
                for j in (b + 1)..=(b + flank).min(n - 1) {
                    nt += total[j];
                    nr += right[j];
                    c += 1.0;
                }
            }
            if c > 0.0 {
                nt /= c;
                nr /= c;
            }
            // The tone's power per channel: what stands over that channel's noise (1.5 times the flank mean: the
            // peak bin of a channel without the tone reads a little over its flanks). A one-sided tone sits at the
            // far side, not where the noise of the other channel pulls it.
            let pr = right[i];
            let pl = (total[i] - right[i]).max(0.0);
            let nl = (nt - nr).max(0.0);
            let tone_r = (pr - 1.5 * nr).max(0.0);
            let tone_l = (pl - 1.5 * nl).max(0.0);
            let sum = tone_l + tone_r;
            // Only a tone that stands 6 dB over the noise in its louder channel gives a pan that can be trusted. A
            // weaker one keeps the last trusted pan: see below. And the tone must still sound: the smoothed power of
            // the lows outlives a note by a second. Once the frame's own level has fallen 10 dB under the smoothed
            // level, what is left is the tail of the smoothing, not a reading.
            let live = max3(left, i).max(max3(right_db, i)) + 3.0;
            let trusted = sum > 0.0 && live >= v - 10.0 && (pr / nr.max(1e-30)).max(pl / nl.max(1e-30)) >= 4.0;
            if trusted {
                let slot = frame_number as usize % HISTORY_LENGTH;
                self.pan_history[i * HISTORY_LENGTH + slot] = (tone_r - tone_l) / sum;
                self.pan_stamp[i * HISTORY_LENGTH + slot] = frame_number;
                self.last_pan[i] = (tone_r - tone_l) / sum;
                self.last_pan_frame[i] = frame_number;
            }
            // Median of the tone's pan over the last `frames` frames. The peak may sit one bin higher or lower from
            // frame to frame.
            let scratch = &mut self.median_scratch;
            let mut m = 0;
            for j in (i - 1)..=(i + 1).min(n - 1) {
                for k in 0..HISTORY_LENGTH {
                    let stamp = self.pan_stamp[j * HISTORY_LENGTH + k];
                    if stamp > 0 && frame_number - stamp < frames && m < scratch.len() {
                        scratch[m] = self.pan_history[j * HISTORY_LENGTH + k];
                        m += 1;
                    }
                }
            }
            if m == 0 {
                // No trusted reading in the last 200 ms: the newest one of the hold time, if there is one.
                let mut newest = 0;
                for j in (i - 1)..=(i + 1).min(n - 1) {
                    if self.last_pan_frame[j] > newest && frame_number - self.last_pan_frame[j] <= hold_frames {
                        newest = self.last_pan_frame[j];
                        scratch[0] = self.last_pan[j];
                        m = 1;
                    }
                }
            }
            if m > 0 {
                // Insertion sort: m is at most 48.
                for x in 1..m {
                    let key = scratch[x];
                    let mut y = x;
                    while y > 0 && scratch[y - 1] > key {
                        scratch[y] = scratch[y - 1];
                        y -= 1;
                    }
                    scratch[y] = key;
                }
                let median = if m % 2 == 1 {
                    scratch[m / 2]
                } else {
                    (scratch[m / 2 - 1] + scratch[m / 2]) / 2.0
                };
                for j in a..=b {
                    self.pan[j] = median;
                    self.tonal[j] = 1;
                    if level[j] < v - self.skirt_drop_db {
                        self.dropped[j] = 1;
                    }
                }
                self.tonal_peaks.push(i);
                // Past the lobe the skirt goes on under the noise. While a bin there is more than 12 dB under the
                // peak and its pan is still pulled from the background toward the tone, it is skirt: no light. The
                // walk ends where the pull ends, where the level climbs again (the next sound) and after `reach` bins.
                let (mut bg, mut bc) = (0.0f32, 0.0f32);
                for j in a.saturating_sub(reach + flank)..a.saturating_sub(reach) {
                    if self.tonal[j] == 0 {
                        bg += self.pan[j];
                        bc += 1.0;
                    }
                }
                for j in (b + reach + 1).min(n)..(b + reach + flank + 1).min(n) {
                    bg += self.pan[j];
                    bc += 1.0;
                }
                if bc > 0.0 {
                    bg /= bc;
                }
                let pull = median - bg;
                if pull.abs() > 0.2 {
                    for step in [-1isize, 1] {
                        let mut j = if step < 0 { a as isize - 1 } else { b as isize + 1 };
                        let mut lowest = level[if step < 0 { a } else { b }];
                        let mut walked = 0;
                        while j >= 0
                            && (j as usize) < n
                            && walked < reach
                            && self.tonal[j as usize] == 0
                            && level[j as usize] < v - self.skirt_drop_db
                            && level[j as usize] <= lowest + 1.0
                            && (self.pan[j as usize] - bg) / pull > 0.15
                        {
                            self.dropped[j as usize] = 1;
                            lowest = lowest.min(level[j as usize]);
                            j += step;
                            walked += 1;
                        }
                    }
                }
            }
            i = b + 1;
        }
    }

    /// Light of a bin, 0...1: 0 at or under the gate or `bottom_db`, 1 at `top_db`.
    pub fn light(&self, i: usize, top_db: f32, bottom_db: f32) -> f32 {
        let v = self.level[i];
        let floor = bottom_db.max(self.gate_db);
        if !(v > floor && self.dropped[i] == 0) {
            return 0.0;
        }
        ((v - floor) / (top_db - floor).max(1.0)).min(1.0)
    }
}
