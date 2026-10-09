//! A-weighting, third-octave band geometry, and the diffuse-field reference.
//!
//! Port source: `Sources/JoseonHeadphones/SPLWeighting.swift` and
//! `DiffuseFieldOffset.swift` in the macOS repo.

use crate::curve::{CurveInterpolator, HeadphoneCurve};
use crate::embedded_curves::DIFFUSE_FIELD_KEMAR;
use std::sync::OnceLock;

// MARK: A-weighting

/// Frequency weighting A of IEC 61672-1, as the analytical transfer function, not the table.
///
/// The standard defines the weighting by a pole-zero network plus a
/// normalization that makes it exactly 0 dB at 1 kHz. This module divides by
/// the network's own 1 kHz response, so `db(1000.0)` is exactly 0.
///
/// Joseon evaluates the weighting at the **nominal** third-octave center. The
/// printed IEC tables use the exact base-10 midband frequencies. The nominal
/// center reproduces the printed values to within 0.16 dB (worst at 160 Hz).
pub mod a_weighting {
    /// Pole frequencies of IEC 61672-1 clause 5.4.6, in Hz.
    pub const F1: f64 = 20.598_997;
    pub const F2: f64 = 107.652_65;
    pub const F3: f64 = 737.862_23;
    pub const F4: f64 = 12_194.217;

    /// The un-normalized network response in dB.
    fn network_db(hz: f64) -> f64 {
        let w = hz * hz;
        let numerator = F4 * F4 * w * w;
        let denominator = (w + F1 * F1) * (w + F4 * F4) * ((w + F2 * F2) * (w + F3 * F3)).sqrt();
        if !(denominator > 0.0 && numerator > 0.0) {
            return f64::NEG_INFINITY;
        }
        20.0 * (numerator / denominator).log10()
    }

    /// A-weighting in dB at one frequency. 0 dB at 1 kHz by construction.
    pub fn db(hz: f64) -> f64 {
        if !(hz > 0.0) {
            return f64::NEG_INFINITY;
        }
        network_db(hz) - network_db(1_000.0)
    }

    /// A-weighting in dB at every frequency of `grid`.
    pub fn db_on(grid: &[f32]) -> Vec<f64> {
        grid.iter().map(|&hz| db(hz as f64)).collect()
    }
}

// MARK: Third-octave band geometry

/// Edges and in-band sampling for the IEC 61260 third-octave bands Joseon uses.
pub mod third_octave_bands {
    use crate::curve::CurveInterpolator;

    /// Log-spaced sample frequencies inside one band, edges included.
    ///
    /// Nine points over a third octave is one sample every 1/24 octave. That is
    /// finer than the 1/10-octave grid of the embedded curves, so the mean is a
    /// true band mean of the stored curve.
    pub const SAMPLES_PER_BAND: usize = 9;

    /// Base-2 band edge ratio: a third-octave band spans `fc · 2^(±1/6)`.
    pub fn edge_ratio() -> f64 {
        2.0f64.powf(1.0 / 6.0)
    }

    /// Lower and upper edge of the band with nominal center `center_hz`.
    pub fn edges(center_hz: f64) -> (f64, f64) {
        let ratio = edge_ratio();
        (center_hz / ratio, center_hz * ratio)
    }

    /// The exact base-10 midband frequency, `10^(n/10)`, behind a nominal center.
    ///
    /// IEC 61260 labels the bands with round numbers but defines them on the
    /// base-10 grid, so nominal 16 000 Hz is exactly 15 848.9 Hz.
    pub fn exact_center_hz(nominal_hz: f64) -> f64 {
        if !(nominal_hz > 0.0) {
            return 0.0;
        }
        10.0f64.powf((10.0 * nominal_hz.log10()).round() / 10.0)
    }

    pub fn sample_frequencies(center_hz: f64) -> [f64; SAMPLES_PER_BAND] {
        let mut out = [0.0; SAMPLES_PER_BAND];
        if !(center_hz > 0.0) {
            return out;
        }
        let (low, high) = edges(center_hz);
        let step = (high / low).ln() / (SAMPLES_PER_BAND - 1) as f64;
        for (i, slot) in out.iter_mut().enumerate() {
            *slot = low * (step * i as f64).exp();
        }
        out
    }

    /// Mean level of a curve across one band, averaged **in the dB domain**.
    ///
    /// The curve is a transfer function, not a spectrum. Its band value is the
    /// mean gain in dB, not the energy mean of a gain.
    pub fn mean_db(interpolator: &CurveInterpolator, center_hz: f64) -> f64 {
        if !(center_hz > 0.0) {
            return 0.0;
        }
        let mut sum = 0.0;
        for f in sample_frequencies(center_hz) {
            sum += interpolator.level(f as f32) as f64;
        }
        sum / SAMPLES_PER_BAND as f64
    }
}

// MARK: Diffuse-field reference

/// The absolute diffuse-field-to-eardrum offset at 1 kHz, and where it comes from.
///
/// The value is **4.1 dB**, from a published table, not estimated:
/// Hammershøi, D. and Møller, H., "Determination of Noise Immission From Sound
/// Sources Close to the Ears", Acta Acustica united with Acustica 94(1), 2008,
/// 114–129. Table II, column `ΔL_DF [dB] / ED`, row 1000 Hz.
///
/// Sign: ISO 11904 subtracts. A 90 dB SPL at the eardrum at 1 kHz is an 85.9 dB
/// diffuse-field-equivalent level. See `docs/third-party-sensitivity.md`.
pub mod diffuse_field_offset_decision {
    /// dB, added to the normalized diffuse-field shape.
    pub const VALUE_DB: f64 = 4.1;

    /// Shown in the calibration sheet next to the level.
    pub const SOURCE: &str =
        "Hammershøi & Møller 2008, Acta Acustica 94(1), Table II, ΔL_DF eardrum at 1 kHz (ISO 11904-1 literature data)";

    /// Table II's `ΔL_DF [dB] / ED` column, by nominal third-octave center, as
    /// `(hz, dB)`. The paper prints one row "≤ 100" for 100 Hz and below.
    pub const PUBLISHED_EARDRUM_DIFFUSE_FIELD_DB: [(f64, f64); 23] = [
        (100.0, 0.0),
        (125.0, 0.2),
        (160.0, 0.4),
        (200.0, 0.6),
        (250.0, 0.8),
        (315.0, 1.1),
        (400.0, 1.5),
        (500.0, 2.1),
        (630.0, 2.8),
        (800.0, 3.3),
        (1_000.0, 4.1),
        (1_250.0, 5.5),
        (1_600.0, 7.7),
        (2_000.0, 11.0),
        (2_500.0, 15.3),
        (3_150.0, 15.7),
        (4_000.0, 12.9),
        (5_000.0, 10.6),
        (6_300.0, 9.4),
        (8_000.0, 9.5),
        (10_000.0, 6.8),
        (12_500.0, 3.8),
        (16_000.0, 0.7),
    ];
}

/// The diffuse-field ear response that turns an eardrum level into a
/// diffuse-field-equivalent level (the ISO 11904-1 idea).
///
/// The shape is the embedded "Diffuse field GRAS KEMAR" target relative to its
/// own 800–1250 Hz mean. `ABSOLUTE_OFFSET_AT_1KHZ_DB` supplies the absolute level.
pub mod diffuse_field_reference {
    use super::*;

    pub const ABSOLUTE_OFFSET_AT_1KHZ_DB: f64 = diffuse_field_offset_decision::VALUE_DB;
    pub const ABSOLUTE_OFFSET_SOURCE: &str = diffuse_field_offset_decision::SOURCE;

    /// The interpolator of the embedded diffuse-field target, normalized to 0 dB over 800–1250 Hz.
    pub fn interpolator() -> &'static CurveInterpolator {
        static CELL: OnceLock<CurveInterpolator> = OnceLock::new();
        CELL.get_or_init(|| CurveInterpolator::new(DIFFUSE_FIELD_KEMAR.curve().normalized_to_1khz()))
    }

    /// The embedded diffuse-field target, normalized to 0 dB over 800–1250 Hz.
    pub fn normalized_curve() -> &'static HeadphoneCurve {
        interpolator().curve()
    }

    /// The full diffuse-field ear response in dB: the normalized shape plus the absolute offset.
    pub fn response_db(hz: f64) -> f64 {
        interpolator().level(hz as f32) as f64 + ABSOLUTE_OFFSET_AT_1KHZ_DB
    }

    /// The term subtracted from the eardrum level in the band at `center_hz`:
    /// the normalized shape averaged over the band, plus the absolute offset.
    pub fn band_term_db(center_hz: f64) -> f64 {
        third_octave_bands::mean_db(interpolator(), center_hz) + ABSOLUTE_OFFSET_AT_1KHZ_DB
    }
}
