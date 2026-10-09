//! Every color a panel uses, made from `Theme` (plus the high-contrast setting).
//! Panels never state a color outside this file.
//!
//! Port source: `Theme` in `RenderContracts.swift` and `Palette.swift` in
//! `Sources/JoseonRender`.

use crate::gpu::Color;

pub type Rgb = [f32; 3];

/// Colors and type for all panels. One theme object, so the panels read as one system.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Theme {
    pub background: Color,
    pub panel: Color,
    pub grid: Color,
    pub text: Color,
    pub accent: Color,
    pub warn: Color,
    pub danger: Color,
}

impl Default for Theme {
    fn default() -> Self {
        Self {
            background: [0.035, 0.047, 0.086, 1.0],
            panel: [0.055, 0.075, 0.13, 1.0],
            grid: [0.35, 0.45, 0.65, 0.22],
            text: [0.80, 0.86, 0.95, 1.0],
            accent: [0.20, 0.55, 1.0, 1.0],
            warn: [1.0, 0.65, 0.15, 1.0],
            danger: [1.0, 0.25, 0.25, 1.0],
        }
    }
}

/// The same color with another alpha.
pub fn with_alpha(c: Color, a: f32) -> Color {
    [c[0], c[1], c[2], a]
}

/// The same color with its alpha multiplied by `k`.
pub fn scaled_alpha(c: Color, k: f32) -> Color {
    [c[0], c[1], c[2], c[3] * k]
}

/// `a` at `t` = 0, `b` at `t` = 1.
pub fn mix(a: Color, b: Color, t: f32) -> Color {
    [
        a[0] + (b[0] - a[0]) * t,
        a[1] + (b[1] - a[1]) * t,
        a[2] + (b[2] - a[2]) * t,
        a[3] + (b[3] - a[3]) * t,
    ]
}

pub const WHITE: Color = [1.0, 1.0, 1.0, 1.0];

pub fn rgba(c: Rgb, a: f32) -> Color {
    [c[0], c[1], c[2], a]
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Palette {
    pub background: Color,
    pub panel: Color,
    /// Plot area fill, a little deeper than the panel.
    pub plot: Color,
    pub grid: Color,
    pub grid_strong: Color,
    /// Labeled grid lines: 12 % white.
    pub grid_major: Color,
    /// Minor log lines: 5 % white.
    pub grid_minor: Color,
    pub rms: Color,
    pub text: Color,
    pub text_dim: Color,
    pub text_faint: Color,
    pub accent: Color,
    pub warn: Color,
    pub danger: Color,
    pub good: Color,
    pub left: Color,
    pub right: Color,
    /// L and R in the spectrum: desaturated tints at 75 %, so neither can be taken for the Mid hue.
    pub spectrum_left: Color,
    pub spectrum_right: Color,
    pub peak_hold: Color,
    pub average: Color,
    pub side: Color,
    pub hp_response: Color,
    pub hp_target: Color,
    pub hp_at_ear: Color,
    /// Third-octave SPL bars of the spectrum and their right-hand axis: a pale ice blue that no curve uses.
    pub spl_band: Color,
    /// The ghost trace of the linked cursor: the cyan of the spectrogram at 85 %, drawn dashed.
    pub cursor_ghost: Color,
    /// Empty meter track.
    pub track: Color,
    pub high_contrast: bool,
}

impl Palette {
    pub fn new(theme: &Theme, high_contrast: bool) -> Self {
        let hc = high_contrast;
        let pick = |high: f32, normal: f32| if hc { high } else { normal };
        let bg = theme.background;
        let mut g = theme.grid;
        if hc {
            g[3] = (g[3] * 2.3).min(0.75);
        }
        // The grid alpha of the theme (default 0.22) scales both: a theme can still make the grid stronger or lighter.
        let gk = theme.grid[3] / 0.22 * pick(2.2, 1.0);
        let t = if hc { [1.0, 1.0, 1.0, 1.0] } else { theme.text };
        // Opaque, so the contrast is known: ticks and headers stay at 7:1 or better on the panel background.
        let toward_background = |k: f32| -> Color {
            [
                t[0] + (bg[0] - t[0]) * k,
                t[1] + (bg[1] - t[1]) * k,
                t[2] + (bg[2] - t[2]) * k,
                1.0,
            ]
        };
        Self {
            background: bg,
            panel: theme.panel,
            plot: [bg[0] * 0.72, bg[1] * 0.72, bg[2] * 0.78, 1.0],
            grid: g,
            grid_strong: [g[0], g[1], g[2], (g[3] * 1.9).min(0.9)],
            grid_major: [1.0, 1.0, 1.0, (0.12 * gk).min(0.5)],
            grid_minor: [1.0, 1.0, 1.0, (0.05 * gk).min(0.25)],
            rms: [0.357, 0.420, 0.541, 1.0],
            text: t,
            text_dim: if hc {
                [0.94, 0.95, 0.97, 1.0]
            } else {
                toward_background(0.16)
            },
            text_faint: if hc {
                [0.86, 0.88, 0.92, 1.0]
            } else {
                toward_background(0.27)
            },
            accent: theme.accent,
            warn: theme.warn,
            danger: theme.danger,
            good: [0.30, 0.92, 0.62, 1.0],
            left: [0.42, 0.70, 1.0, 0.9],
            // A little see-through: where L and R are the same, both colors show.
            right: [1.0, 0.56, 0.42, 0.8],
            spectrum_left: [127.0 / 255.0, 166.0 / 255.0, 217.0 / 255.0, pick(0.9, 0.75)], // #7FA6D9
            spectrum_right: [217.0 / 255.0, 144.0 / 255.0, 127.0 / 255.0, pick(0.9, 0.75)], // #D9907F
            peak_hold: [0.949, 0.914, 0.847, pick(0.95, 0.80)],                            // #F2E9D8 warm white
            average: [0.478, 0.525, 0.627, pick(0.85, 0.60)],                              // #7A86A0 cool grey
            side: [0.780, 0.490, 1.0, pick(0.95, 0.80)],                                   // #C77DFF magenta-violet
            hp_response: theme.warn,
            hp_target: if hc {
                [0.85, 0.88, 0.94, 1.0]
            } else {
                [0.604, 0.643, 0.722, 1.0]
            },
            hp_at_ear: [1.0, 0.36, 0.80, 1.0],
            spl_band: if hc {
                [0.90, 0.95, 1.0, 1.0]
            } else {
                [0.72, 0.84, 1.0, 1.0]
            },
            cursor_ghost: [57.0 / 255.0, 194.0 / 255.0, 1.0, pick(1.0, 0.85)],
            track: [theme.panel[0] * 1.5, theme.panel[1] * 1.5, theme.panel[2] * 1.45, 1.0],
            high_contrast,
        }
    }

    // MARK: A/B compare (from `Comparison.swift`)

    /// The reference "A": amber at 90 %.
    pub fn reference(&self) -> Color {
        [
            227.0 / 255.0,
            179.0 / 255.0,
            65.0 / 255.0,
            if self.high_contrast { 1.0 } else { 0.90 },
        ]
    }

    /// B's long-term curve while a reference is set: the cool grey of "Long-term", lighter and at 90 %.
    pub fn live_long_term(&self) -> Color {
        if self.high_contrast {
            [0.80, 0.86, 0.96, 1.0]
        } else {
            [0.66, 0.73, 0.86, 0.90]
        }
    }

    /// The difference lane: where B has more (above zero) a light neutral tone, where B has less a darker one.
    pub fn delta_above(&self) -> Color {
        if self.high_contrast {
            [0.92, 0.94, 0.98, 1.0]
        } else {
            [0.80, 0.84, 0.92, 1.0]
        }
    }

    pub fn delta_below(&self) -> Color {
        if self.high_contrast {
            [0.56, 0.62, 0.74, 1.0]
        } else {
            [0.40, 0.45, 0.57, 1.0]
        }
    }

    pub fn delta_line(&self) -> Color {
        if self.high_contrast {
            [1.0, 1.0, 1.0, 1.0]
        } else {
            [0.88, 0.91, 0.96, 0.95]
        }
    }

    // MARK: Color maps

    /// Hue across the log frequency axis: warm lows, green mids, cyan to violet
    /// highs. The position is 0 at 10 Hz and 1 at 24 kHz.
    pub const SPECTRUM_STOPS: [(f32, Rgb); 9] = [
        (0.00, [0.96, 0.22, 0.30]), // 10 Hz   deep rose red
        (0.14, [1.00, 0.38, 0.20]), // 30 Hz   red orange
        (0.27, [1.00, 0.62, 0.14]), // 80 Hz   amber
        (0.40, [0.96, 0.86, 0.22]), // 220 Hz  yellow
        (0.53, [0.50, 0.94, 0.34]), // 600 Hz  green
        (0.65, [0.16, 0.92, 0.66]), // 1.6 kHz sea green
        (0.76, [0.14, 0.80, 0.98]), // 3.7 kHz cyan
        (0.87, [0.32, 0.54, 1.00]), // 8.7 kHz blue
        (1.00, [0.70, 0.42, 1.00]), // 24 kHz  violet
    ];

    pub fn spectrum_lut(&self, count: usize) -> Vec<Rgb> {
        color_map::sample(&Self::SPECTRUM_STOPS, count)
    }

    pub fn spectrum_color(hz: f32) -> Rgb {
        let t = (hz.max(10.0) / 10.0).ln() / 2400f32.ln();
        color_map::evaluate(&Self::SPECTRUM_STOPS, t.max(0.0).min(1.0))
    }

    /// Spectrogram map: exactly the plot background at 0 (no haze under the
    /// floor), a short navy toe, then blue, cyan, green, yellow, hot white.
    /// Lightness rises steadily.
    pub fn heat_lut(&self, count: usize) -> Vec<Rgb> {
        let bg = [self.plot[0], self.plot[1], self.plot[2]];
        // Positions are linear in dB between the floor (-75) and the automatic top.
        let stops = [
            (0.00, bg),
            (0.07, [0.030, 0.060, 0.200]),
            (0.18, [0.055, 0.130, 0.420]), // a noise floor 10 dB over the black point: dark navy
            (0.30, [0.075, 0.270, 0.720]),
            (0.42, [0.060, 0.470, 0.880]),
            (0.53, [0.090, 0.680, 0.900]), // 20 dB over that floor: bright cyan-blue
            (0.64, [0.080, 0.810, 0.700]),
            (0.75, [0.350, 0.900, 0.400]),
            (0.86, [0.960, 0.880, 0.240]),
            (0.94, [1.000, 0.640, 0.260]),
            (1.00, [1.000, 0.970, 0.920]),
        ];
        color_map::sample(&stops, count)
    }

    /// Vectorscope density ramp: black, deep blue, cyan, warm white capped at
    /// 90 % luminance. The ramp never reaches pure white: the densest core keeps its texture.
    pub fn scope_lut(&self, count: usize) -> Vec<Rgb> {
        let stops = [
            (0.00, [0.0, 0.0, 0.0]),
            (0.22, [0.043, 0.227, 0.549]),
            (0.62, [0.224, 0.761, 1.000]),
            (0.86, [0.560, 0.860, 0.970]),
            (1.00, [0.800, 0.920, 0.960]),
        ];
        color_map::sample(&stops, count)
    }
}

// The OKLab matrices are the published constants, digit for digit.
#[allow(clippy::excessive_precision)]
pub mod color_map {
    use super::Rgb;

    /// Interpolates in OKLab, so the ramp has no muddy or dark bands between stops.
    pub fn evaluate(stops: &[(f32, Rgb)], t: f32) -> Rgb {
        let (Some(first), Some(last)) = (stops.first(), stops.last()) else {
            return [0.0; 3];
        };
        if t <= first.0 {
            return first.1;
        }
        if t >= last.0 {
            return last.1;
        }
        for i in 1..stops.len() {
            if t <= stops[i].0 {
                let (a, b) = (stops[i - 1], stops[i]);
                let mut k = (t - a.0) / (b.0 - a.0).max(1e-6);
                k = k * k * (3.0 - 2.0 * k) * 0.35 + k * 0.65; // slight ease, keeps the ramp smooth at the stops
                let (la, lb) = (oklab_from_srgb(a.1), oklab_from_srgb(b.1));
                return srgb_from_oklab([
                    la[0] + (lb[0] - la[0]) * k,
                    la[1] + (lb[1] - la[1]) * k,
                    la[2] + (lb[2] - la[2]) * k,
                ]);
            }
        }
        last.1
    }

    pub fn sample(stops: &[(f32, Rgb)], count: usize) -> Vec<Rgb> {
        (0..count)
            .map(|i| evaluate(stops, i as f32 / (count - 1) as f32))
            .collect()
    }

    fn to_linear(c: f32) -> f32 {
        if c <= 0.04045 {
            c / 12.92
        } else {
            ((c + 0.055) / 1.055).powf(2.4)
        }
    }

    fn to_gamma(c: f32) -> f32 {
        let v = c.max(0.0);
        if v <= 0.0031308 {
            v * 12.92
        } else {
            1.055 * v.powf(1.0 / 2.4) - 0.055
        }
    }

    pub fn oklab_from_srgb(c: Rgb) -> Rgb {
        let (r, g, b) = (to_linear(c[0]), to_linear(c[1]), to_linear(c[2]));
        let l = (0.4122214708 * r + 0.5363325363 * g + 0.0514459929 * b).cbrt();
        let m = (0.2119034982 * r + 0.6806995451 * g + 0.1073969566 * b).cbrt();
        let s = (0.0883024619 * r + 0.2817188376 * g + 0.6299787005 * b).cbrt();
        [
            0.2104542553 * l + 0.7936177850 * m - 0.0040720468 * s,
            1.9779984951 * l - 2.4285922050 * m + 0.4505937099 * s,
            0.0259040371 * l + 0.7827717662 * m - 0.8086757660 * s,
        ]
    }

    pub fn srgb_from_oklab(c: Rgb) -> Rgb {
        let l_ = c[0] + 0.3963377774 * c[1] + 0.2158037573 * c[2];
        let m_ = c[0] - 0.1055613458 * c[1] - 0.0638541728 * c[2];
        let s_ = c[0] - 0.0894841775 * c[1] - 1.2914855480 * c[2];
        let (l, m, s) = (l_ * l_ * l_, m_ * m_ * m_, s_ * s_ * s_);
        let r = 4.0767416621 * l - 3.3077115913 * m + 0.2309699292 * s;
        let g = -1.2684380046 * l + 2.6097574011 * m - 0.3413193965 * s;
        let b = -0.0041960863 * l - 0.7034186147 * m + 1.7076147010 * s;
        [to_gamma(r).min(1.0), to_gamma(g).min(1.0), to_gamma(b).min(1.0)]
    }
}
