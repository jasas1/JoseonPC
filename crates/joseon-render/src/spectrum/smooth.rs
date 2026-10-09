//! Display smoothing of the thin curves of the spectrum.
//!
//! Port source: `smoothHighs` and `boxBlur` in `Sources/JoseonRender/SpectrumRenderer.swift`.

use super::MAX_POINTS;
use crate::axis::LogAxis;

/// What `smooth_highs` needs from the plot.
#[derive(Clone, Copy)]
pub(super) struct SmoothContext {
    pub axis: LogAxis,
    /// The dB range of the plot (`hi - lo`): the curves are 0...1 over it.
    pub range_db: f32,
}

/// True when a curve of `n` points goes through the smoothing at this plot size.
pub(super) fn applies(axis: LogAxis, n: usize) -> bool {
    limits(axis, n).is_some()
}

/// `(r_max, from, full, m, start)`, or `None` when the plot is too small to smooth.
fn limits(axis: LogAxis, n: usize) -> Option<(usize, usize, usize, usize, usize)> {
    if n < 2 {
        return None;
    }
    let per_octave = (n - 1) as f32 / (axis.max_hz / axis.min_hz).log2();
    let r_max = ((per_octave / 12.0).round() as i64).max(1);
    let from = (axis.position(1_400.0) * (n - 1) as f32) as i64;
    let full = (axis.position(2_800.0) * (n - 1) as f32) as i64;
    let m = n.min(MAX_POINTS - 1) as i64;
    let start = from - r_max - 4;
    (start >= 0 && from < m - 2 && full > from).then_some((
        r_max as usize,
        from as usize,
        full as usize,
        m as usize,
        start as usize,
    ))
}

/// L, R and Side above 2 kHz: about 1/6 octave of smoothing (fading in from
/// 1.4 kHz), so the thin lines read as curves and not as hash. Only the noise
/// residual is smoothed. A tonal peak (a local maximum 8 dB or more over its
/// 1/6-octave surroundings) is taken out before the smoothing, so it does not
/// lift a flat-topped box around itself, and is put back unsmoothed with its
/// whole lobe (at least the peak and 2 points on each side). The join is
/// continuous: the result is the smoothed residual, or the raw curve where
/// that is higher, with a 3-point taper at the feet of the lobe.
///
/// With `other` (the opposite channel) a second kind of peak is kept: a local
/// maximum that stands 4 dB or more over the other channel, where that
/// difference is itself a bump (3 dB over the mean difference within 1/3
/// octave). Noise that the two channels share in level gives a difference near
/// 0 dB, so this finds a one-sided tone only a few dB over the noise of its own
/// channel, which the 8 dB rule cannot see.
///
/// `scratch` is four rows of `MAX_POINTS`. Gives true when smoothing ran.
pub(super) fn smooth_highs(
    v: &mut [f32],
    n: usize,
    other: Option<&[f32]>,
    scratch: &mut [f32],
    c: SmoothContext,
) -> bool {
    let Some((r_max, from, full, m, start)) = limits(c.axis, n) else {
        return false;
    };
    let (prefix, rest) = scratch.split_at_mut(MAX_POINTS);
    let (resid, rest) = rest.split_at_mut(MAX_POINTS);
    let (keep, diff_prefix) = rest.split_at_mut(MAX_POINTS);

    fn mean(prefix: &[f32], i: usize, r: usize, start: usize, m: usize) -> f32 {
        let a = i.saturating_sub(r).max(start);
        let b = (i + r).min(m - 1);
        (prefix[b + 1] - prefix[a]) / (b - a + 1) as f32
    }

    prefix[start] = 0.0;
    for i in start..m {
        prefix[i + 1] = prefix[i] + v[i];
        resid[i] = v[i];
        keep[i] = 0.0;
    }
    let eight_db = 8.0 / c.range_db.max(1.0);
    let four_db = eight_db / 2.0;
    let three_db = eight_db * 3.0 / 8.0;
    if let Some(other) = other {
        diff_prefix[start] = 0.0;
        for i in start..m {
            diff_prefix[i + 1] = diff_prefix[i] + (v[i] - other[i]);
        }
    }
    let one_sided = |v: &[f32], diff_prefix: &[f32], i: usize| -> bool {
        let Some(other) = other else { return false };
        if !(v[i] - other[i] >= four_db && v[i] > 0.02) {
            return false;
        }
        let a = i.saturating_sub(r_max * 4).max(start);
        let b = (i + r_max * 4).min(m - 1);
        (v[i] - other[i]) - (diff_prefix[b + 1] - diff_prefix[a]) / (b - a + 1) as f32 >= three_db
    };
    let max_reach = (r_max * 3).max(4);
    let mut i = from;
    while i < m - 1 {
        let is_peak = v[i] >= v[i - 1]
            && v[i] > v[i + 1]
            && (v[i] - mean(prefix, i, r_max, start, m) >= eight_db || one_sided(v, diff_prefix, i));
        if !is_peak {
            i += 1;
            continue;
        }
        // The lobe: downhill from the peak on both sides.
        let (mut a, mut b) = (i, i);
        while a > start + 1 && i - a < max_reach && v[a - 1] < v[a] {
            a -= 1;
        }
        while b < m - 2 && b - i < max_reach && v[b + 1] < v[b] {
            b += 1;
        }
        a = a.min(i.saturating_sub(2)).max(start);
        b = b.max(i + 2).min(m - 1);
        // The noise under the lobe: a line between the mean levels just outside its feet.
        let half = r_max / 2;
        let ya = mean(prefix, a.saturating_sub(half + 1).max(start), half, start, m);
        let yb = mean(prefix, (b + half + 1).min(m - 1), half, start, m);
        for j in a..=b {
            resid[j] = (ya + (yb - ya) * (j - a) as f32 / (b - a).max(1) as f32).min(v[j]);
            keep[j] = 1.0;
        }
        for t in 1..=3usize {
            let w = 1.0 - t as f32 / 4.0;
            if a >= start + t {
                keep[a - t] = keep[a - t].max(w);
            }
            if b + t < m {
                keep[b + t] = keep[b + t].max(w);
            }
        }
        i = b + 1;
    }
    prefix[start] = 0.0;
    for j in start..m {
        prefix[j + 1] = prefix[j] + resid[j];
    }
    for j in from..m {
        let k = ((j - from) as f32 / (full - from) as f32).min(1.0);
        let r = (r_max as f32 * k * k * (3.0 - 2.0 * k)).round() as usize;
        let smooth = if r > 0 { mean(prefix, j, r, start, m) } else { resid[j] };
        v[j] = smooth + (v[j] - smooth).max(0.0) * keep[j];
    }
    true
}

/// A box blur of radius `r` from `src` into `dst`. At the ends the box is shorter.
pub(super) fn box_blur(src: &[f32], dst: &mut [f32], n: usize, r: usize) {
    let mut sum = 0f32;
    let mut count = 0usize;
    for value in &src[..(r + 1).min(n)] {
        sum += value;
        count += 1;
    }
    for i in 0..n {
        dst[i] = sum / count.max(1) as f32;
        let add = i + r + 1;
        if add < n {
            sum += src[add];
            count += 1;
        }
        if i >= r {
            sum -= src[i - r];
            count -= 1;
        }
    }
}
