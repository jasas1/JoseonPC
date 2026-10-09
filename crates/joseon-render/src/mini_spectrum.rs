//! The mini spectrum: a small silhouette of the music for the tray icon and the
//! mini strip. Must be cheap: it is made up to 30 times per second.
//!
//! Column level = the mean power of the spectrum bins under a triangle two
//! columns wide, centered on the column. (A maximum per column made a tone a
//! flat-topped box; the mean follows the lobe.) The silhouette runs through the
//! column centers, so a tone has sloped flanks and a thin peak stays about one
//! column wide at half height.
//!
//! The model keeps a little state between calls (column ballistics and the
//! adaptive range). The time step comes from `AnalysisFrame::host_time`, so the
//! motion does not depend on the call rate.
//!
//! Port source: `Sources/JoseonRender/MiniSpectrumRenderer.swift`. The Mac
//! draws a template image that the menu bar tints; here the caller gives the
//! color.

use joseon_core::contracts::{AnalysisFrame, SpectrumReading};

const CAPACITY: usize = 320;
/// Distance of the loudest column to the top of the adaptive window.
const HEADROOM_DB: f32 = 3.0;
/// The soft knee starts here (0...1 of the window).
const KNEE_START: f32 = 0.94;
pub const DEFAULT_TILT_DB_PER_OCTAVE: f32 = 4.5;

/// The renderer tilt for spectrum data the engine tilted by `engine_tilt` already: the total stays +4.5.
pub fn tilt(engine_tilt: f32) -> f32 {
    (DEFAULT_TILT_DB_PER_OCTAVE - engine_tilt).max(0.0)
}

pub struct MiniSpectrum {
    /// Log columns across the image, when `scales_band_count_with_width` is off.
    pub band_count: usize,
    /// On: one column per `column_pixels` device pixels, so the column edges sit on whole pixels.
    pub scales_band_count_with_width: bool,
    pub column_pixels: usize,
    /// The outer columns are scaled down over this width (in points, at least 2 device pixels), so neither strong
    /// bass nor a bright top end makes a full-height wall at an edge of the picture.
    pub edge_taper_points: f32,
    /// The first column starts here. 30 Hz stays clear of the level shelf below 20-30 Hz in the spectrum data.
    pub min_hz: f32,
    /// The last column ends here: the picture ends where the music ends.
    pub max_hz: f32,
    /// Lowest level of interest. The adaptive window top never goes below `min_db + adaptive_window_db`.
    pub min_db: f32,
    pub max_db: f32,
    /// The mini graph's own display tilt in dB per octave, pivot 1 kHz. Music falls about 4.5 dB per octave:
    /// with less tilt real music is a bass blob and a flat line. A drawing choice, not a measurement.
    pub tilt_db_per_octave: f32,
    /// Contrast curve on the 0...1 level. Above 1 quiet parts sink and peaks stand out.
    pub contrast: f32,
    /// Columns fall with this time constant.
    pub release_seconds: f32,
    /// Columns rise with this time constant.
    pub attack_seconds: f32,
    /// Follows the music level: the window `adaptive_window_db` slides inside min_db...max_db so the shape
    /// stays readable at any volume. Off = fixed min_db...max_db.
    pub adaptive_range: bool,
    pub adaptive_window_db: f32,
    pub range_attack_seconds: f32,
    pub range_release_seconds: f32,

    ceiling_db: Option<f32>,
    last_host_time: Option<f64>,
    last_count: usize,
    tilted: Vec<f32>,
    // Per column: first bin, bin count and offset into `weights`. Cached while the spectrum layout stays the same.
    band_lo: Vec<usize>,
    band_bins: Vec<usize>,
    band_offset: Vec<usize>,
    /// Triangle weights of every column, one after the other. Each column's weights sum to 1.
    weights: Vec<f32>,
    power: Vec<f32>,
    band_octave: Vec<f32>,
    layout_key: (usize, f32, f32, usize, f32, f32),
    edge_gain: Vec<f32>,
    edge_key: (usize, f32),
    points_per_column_key: f32,
    column_db: Vec<f32>,
    shown: Vec<f32>,
    count: usize,
    pixmap: Option<tiny_skia::Pixmap>,
}

impl Default for MiniSpectrum {
    fn default() -> Self {
        Self::new()
    }
}

impl MiniSpectrum {
    pub fn new() -> Self {
        Self {
            band_count: 56,
            scales_band_count_with_width: true,
            column_pixels: 1,
            edge_taper_points: 1.0,
            min_hz: 30.0,
            max_hz: 20_000.0,
            min_db: -96.0,
            max_db: -6.0,
            tilt_db_per_octave: DEFAULT_TILT_DB_PER_OCTAVE,
            contrast: 1.6,
            release_seconds: 0.15,
            attack_seconds: 0.02,
            adaptive_range: true,
            adaptive_window_db: 48.0,
            range_attack_seconds: 0.2,
            range_release_seconds: 8.0,
            ceiling_db: None,
            last_host_time: None,
            last_count: 0,
            tilted: vec![-200.0; CAPACITY],
            band_lo: vec![0; CAPACITY],
            band_bins: vec![0; CAPACITY],
            band_offset: vec![0; CAPACITY],
            weights: Vec::new(),
            power: Vec::new(),
            band_octave: vec![0.0; CAPACITY],
            layout_key: (0, 0.0, 0.0, 0, 0.0, 0.0),
            edge_gain: vec![1.0; CAPACITY],
            edge_key: (0, 0.0),
            points_per_column_key: 0.0,
            column_db: vec![-200.0; CAPACITY],
            shown: vec![0.0; CAPACITY],
            count: 0,
            pixmap: None,
        }
    }

    /// Top of the adaptive window in dB.
    pub fn ceiling_db(&self) -> Option<f32> {
        self.ceiling_db
    }

    /// The level per column in dB before tilt and mapping (newest update).
    pub fn column_db(&self) -> &[f32] {
        &self.column_db[..self.count]
    }

    /// Shown level per column, 0...1, after ballistics (newest update).
    pub fn shown(&self) -> &[f32] {
        &self.shown[..self.count]
    }

    /// Columns used for an image `pixel_width` pixels wide.
    pub fn column_count(&self, pixel_width: usize) -> usize {
        if !self.scales_band_count_with_width {
            return self.band_count.max(8).min(CAPACITY).min(pixel_width.max(1));
        }
        (pixel_width / self.column_pixels.max(1)).max(1).min(CAPACITY)
    }

    /// Updates the columns from the frame, for a picture of `count` columns, each `points_per_column` wide.
    /// False when there is nothing to draw but the hairline.
    pub fn update(&mut self, frame: &AnalysisFrame, count: usize, points_per_column: f32) -> bool {
        let n = count.max(1).min(CAPACITY);
        self.count = n;
        let s = &frame.spectrum;
        // Time step from the frame clock. The same frame again = no time passed. A jump = start over.
        let mut dt = 0.0f32;
        let mut restart = n != self.last_count;
        match self.last_host_time {
            Some(last) => {
                let d = frame.host_time - last;
                if d < 0.0 || d > 1.0 {
                    restart = true;
                } else {
                    dt = d as f32;
                }
            }
            None => restart = true,
        }
        self.last_host_time = Some(frame.host_time);
        self.last_count = n;
        if restart {
            self.shown[..n].fill(0.0);
            self.ceiling_db = None;
        }

        let bins = s.mid.len().min(s.frequencies.len());
        let usable = bins >= 2 && s.frequencies[0] > 0.0 && s.frequencies[bins - 1] > s.frequencies[0];
        if !usable {
            self.shown[..n].fill(0.0);
            return false;
        }
        let f0 = s.frequencies[0];
        let key = (bins, f0, s.frequencies[bins - 1], n, self.min_hz, self.max_hz);
        if key != self.layout_key {
            self.layout_key = key;
            let ln_f0 = f0.ln();
            let ln_span = s.frequencies[bins - 1].ln() - ln_f0;
            let ln_min = self.min_hz.ln();
            let ln_band = (self.max_hz.ln() - ln_min) / n as f32;
            self.weights.clear();
            for b in 0..n {
                // Bin coordinates of the column center and of the triangle's half width (one column).
                let center = (ln_min + ln_band * (b as f32 + 0.5) - ln_f0) / ln_span * (bins - 1) as f32;
                let half = (ln_band / ln_span * (bins - 1) as f32).max(1.0);
                let i0 = ((center - half).ceil() as i64).clamp(0, bins as i64 - 1) as usize;
                let i1 = ((center + half).floor() as i64).clamp(i0 as i64, bins as i64 - 1) as usize;
                self.band_lo[b] = i0;
                self.band_bins[b] = i1 - i0 + 1;
                self.band_offset[b] = self.weights.len();
                let mut sum = 0.0f32;
                for i in i0..=i1 {
                    let w = (1.0 - (i as f32 - center).abs() / half).max(0.02);
                    self.weights.push(w);
                    sum += w;
                }
                for w in &mut self.weights[self.band_offset[b]..] {
                    *w /= sum;
                }
                self.band_octave[b] = (ln_min + ln_band * (b as f32 + 0.5) - 1000f32.ln()) / 2f32.ln();
            }
        }

        if self.edge_key != (n, self.edge_taper_points) || self.points_per_column_key != points_per_column {
            self.edge_key = (n, self.edge_taper_points);
            self.points_per_column_key = points_per_column;
            // A short linear scale on the real data: with 2 columns the gains are 1/3 and 2/3. No column is
            // forced to the baseline, so the shape of the music is what ends the picture, not a ramp.
            let reach = ((self.edge_taper_points / points_per_column.max(0.01)).round() as usize)
                .max(2)
                .min((n / 4).max(1));
            for b in 0..n {
                let d = b.min(n - 1 - b);
                self.edge_gain[b] = if d >= reach {
                    1.0
                } else {
                    (d + 1) as f32 / (reach + 1) as f32
                };
            }
        }

        // Per column: the mean power of the mid bins under the column's triangle, plus the display tilt.
        let floor_db = SpectrumReading::FLOOR_DB + 1.0;
        let mut top = -200.0f32;
        self.power.resize(bins, 0.0);
        // dB -> power: 10^(dB / 10) = exp(dB * ln(10) / 10)
        for (p, db) in self.power.iter_mut().zip(&s.mid) {
            *p = (db * 0.230_258_51).exp();
        }
        let floor_power = 10f32.powf(floor_db / 10.0);
        for b in 0..n {
            let (lo, len, offset) = (self.band_lo[b], self.band_bins[b], self.band_offset[b]);
            let mut mean = 0.0f32;
            for k in 0..len {
                mean += self.power[lo + k] * self.weights[offset + k];
            }
            let db = if mean <= floor_power {
                -200.0
            } else {
                10.0 * mean.log10()
            };
            self.column_db[b] = db;
            let v = if db < -199.0 {
                -200.0
            } else {
                db + self.tilt_db_per_octave * self.band_octave[b]
            };
            self.tilted[b] = v;
            top = top.max(v);
        }

        // Vertical window. The top follows the loudest column: up within about half a second, down over many
        // seconds. It rides the peaks, so a kick drum does not make the whole picture breathe.
        let (mut lo, mut hi) = (self.min_db, self.max_db);
        if self.adaptive_range {
            let window = self.adaptive_window_db.min(self.max_db - self.min_db);
            if top > -199.0 {
                // The loudest column sits `HEADROOM_DB` under the top of the window, below the soft knee.
                let target = (top + HEADROOM_DB).max(self.min_db + window).min(self.max_db);
                self.ceiling_db = Some(match self.ceiling_db {
                    Some(c) => {
                        let tau = if target > c {
                            self.range_attack_seconds
                        } else {
                            self.range_release_seconds
                        };
                        c + (target - c) * (1.0 - (-dt / tau.max(0.001)).exp())
                    }
                    None => target,
                });
            }
            hi = self.ceiling_db.unwrap_or(self.max_db);
            lo = hi - window;
        }

        let keep = if dt > 0.0 {
            (-dt / self.release_seconds.max(0.001)).exp()
        } else {
            1.0
        };
        // The first picture after a restart shows the level at once.
        let rise = if restart {
            1.0
        } else if dt > 0.0 {
            1.0 - (-dt / self.attack_seconds.max(0.001)).exp()
        } else {
            0.0
        };
        let inv_range = 1.0 / (hi - lo).max(1.0);
        let gamma = self.contrast;
        let mut any = false;
        for b in 0..n {
            let mut t = if self.tilted[b] < -199.0 {
                0.0
            } else {
                ((self.tilted[b] - lo) * inv_range).max(0.0)
            };
            // Soft knee: only a transient that beats the window (before the ceiling follows) bends toward the
            // top edge.
            if t > KNEE_START {
                t = KNEE_START + (1.0 - KNEE_START) * (1.0 - (-(t - KNEE_START) / (1.0 - KNEE_START)).exp());
            }
            let mut level = if gamma == 1.0 { t } else { t.powf(gamma) };
            // The outer columns are scaled down: no full-height wall at the image border.
            level *= self.edge_gain[b];
            let old = self.shown[b];
            // Fast attack, exponential release.
            let v = if level > old {
                old + (level - old) * rise
            } else {
                level.max(old * keep)
            };
            self.shown[b] = if v < 0.004 { 0.0 } else { v };
            any |= self.shown[b] > 0.0;
        }
        any
    }

    /// The picture of the frame: `width` x `height` points at `scale` pixels per point, in `color`, on a clear
    /// background. Gives the pixel size and the RGBA bytes (not premultiplied), row by row from the top.
    pub fn render(
        &mut self,
        frame: &AnalysisFrame,
        width: f32,
        height: f32,
        scale: f32,
        color: [f32; 4],
    ) -> (u32, u32, Vec<u8>) {
        let (w, h, sc) = (width.max(1.0), height.max(1.0), scale.max(1.0));
        let (pw, ph) = (((w * sc).round() as u32).max(1), ((h * sc).round() as u32).max(1));
        let n = self.column_count(pw as usize);
        let any = self.update(frame, n, w / n as f32);
        if !self
            .pixmap
            .as_ref()
            .is_some_and(|p| (p.width(), p.height()) == (pw, ph))
        {
            self.pixmap = tiny_skia::Pixmap::new(pw, ph);
        }
        let Some(pixmap) = &mut self.pixmap else {
            return (pw, ph, vec![0; (pw * ph * 4) as usize]);
        };
        pixmap.fill(tiny_skia::Color::TRANSPARENT);
        let mut paint = tiny_skia::Paint::default();
        paint.set_color(
            tiny_skia::Color::from_rgba(color[0], color[1], color[2], color[3]).unwrap_or(tiny_skia::Color::WHITE),
        );
        paint.anti_alias = true;
        // One point tall baseline: the whole picture at silence.
        let hair = sc.round().max(1.0);
        let (fw, fh) = (pw as f32, ph as f32);
        if let Some(rect) = tiny_skia::Rect::from_xywh(0.0, fh - hair, fw, hair) {
            pixmap.fill_rect(rect, &paint, tiny_skia::Transform::identity(), None);
        }
        if any {
            // One filled silhouette through the column centers. y grows down here: the top of a column is
            // `usable` times its level over the hairline.
            let usable = fh - 1.0 - hair;
            let edge = |i: usize| ((i * pw as usize + n / 2) / n) as f32;
            let y = |level: f32| fh - hair - level * usable;
            let mut path = tiny_skia::PathBuilder::new();
            path.move_to(0.0, fh - hair);
            path.line_to(0.0, y(self.shown[0]));
            for i in 0..n {
                path.line_to((edge(i) + edge(i + 1)) / 2.0, y(self.shown[i]));
            }
            path.line_to(fw, y(self.shown[n - 1]));
            path.line_to(fw, fh - hair);
            path.close();
            if let Some(path) = path.finish() {
                pixmap.fill_path(
                    &path,
                    &paint,
                    tiny_skia::FillRule::Winding,
                    tiny_skia::Transform::identity(),
                    None,
                );
            }
        }
        // Straight alpha for the receivers (icons, textures).
        let mut rgba = Vec::with_capacity((pw * ph * 4) as usize);
        for p in pixmap.pixels() {
            let c = p.demultiply();
            rgba.extend([c.red(), c.green(), c.blue(), c.alpha()]);
        }
        (pw, ph, rgba)
    }
}
