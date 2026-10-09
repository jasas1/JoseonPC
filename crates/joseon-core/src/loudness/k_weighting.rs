//! K-weighting for ITU-R BS.1770-4.
//!
//! Port source: `Sources/JoseonCore/Loudness/KWeighting.swift`.
//!
//! The recommendation prints coefficients for 48 kHz only. The app plays at
//! 44.1 kHz through 768 kHz, so the coefficients are derived from the analogue
//! prototype at the actual sample rate. At 48 kHz this derivation reproduces
//! the table in BS.1770-4 to the printed precision.

use std::f64::consts::PI;

/// One second-order section, direct form II transposed.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct BiquadCoefficients {
    pub b0: f64,
    pub b1: f64,
    pub b2: f64,
    pub a1: f64,
    pub a2: f64,
}

impl BiquadCoefficients {
    pub const IDENTITY: Self = Self {
        b0: 1.0,
        b1: 0.0,
        b2: 0.0,
        a1: 0.0,
        a2: 0.0,
    };
}

/// Double-precision state. It keeps the RLB high-pass stable over hours of listening.
#[derive(Debug, Clone, Copy, Default)]
struct BiquadState {
    z1: f64,
    z2: f64,
}

#[inline(always)]
fn biquad_step(c: &BiquadCoefficients, s: &mut BiquadState, x: f64) -> f64 {
    let y = c.b0 * x + s.z1;
    s.z1 = c.b1 * x - c.a1 * y + s.z2;
    s.z2 = c.b2 * x - c.a2 * y;
    y
}

// Analogue prototype of stage 1, the head-shadow high shelf. These are the
// values that regenerate the 48 kHz table in BS.1770-4.
const SHELF_CENTER_HZ: f64 = 1681.974450955533;
const SHELF_GAIN_DB: f64 = 3.999843853973347;
const SHELF_Q: f64 = 0.7071752369554196;
/// Exponent that links the shelf mid-band gain to its high-band gain.
const SHELF_VB_EXPONENT: f64 = 0.4996667741545416;

// Analogue prototype of stage 2, the RLB high-pass.
const HIGH_PASS_CENTER_HZ: f64 = 38.13547087602444;
const HIGH_PASS_Q: f64 = 0.5003270373238773;

/// Stage 1: high shelf, about +4 dB above 2 kHz.
pub(crate) fn shelf(sample_rate: f64) -> BiquadCoefficients {
    let k = (PI * SHELF_CENTER_HZ / sample_rate).tan();
    let k2 = k * k;
    let vh = 10f64.powf(SHELF_GAIN_DB / 20.0);
    let vb = vh.powf(SHELF_VB_EXPONENT);
    let a0 = 1.0 + k / SHELF_Q + k2;
    BiquadCoefficients {
        b0: (vh + vb * k / SHELF_Q + k2) / a0,
        b1: 2.0 * (k2 - vh) / a0,
        b2: (vh - vb * k / SHELF_Q + k2) / a0,
        a1: 2.0 * (k2 - 1.0) / a0,
        a2: (1.0 - k / SHELF_Q + k2) / a0,
    }
}

/// Stage 2: RLB high-pass. BS.1770 keeps the numerator at (1, −2, 1) and
/// normalises only the denominator, so this form matches the printed table.
pub(crate) fn high_pass(sample_rate: f64) -> BiquadCoefficients {
    let k = (PI * HIGH_PASS_CENTER_HZ / sample_rate).tan();
    let k2 = k * k;
    let d = 1.0 + k / HIGH_PASS_Q + k2;
    BiquadCoefficients {
        b0: 1.0,
        b1: -2.0,
        b2: 1.0,
        a1: 2.0 * (k2 - 1.0) / d,
        a2: (1.0 - k / HIGH_PASS_Q + k2) / d,
    }
}

/// K-weighting for one channel: shelf then high-pass, with its own state.
#[derive(Debug, Clone)]
pub(crate) struct KWeightingFilter {
    shelf: BiquadCoefficients,
    high_pass: BiquadCoefficients,
    shelf_state: BiquadState,
    high_pass_state: BiquadState,
}

impl KWeightingFilter {
    pub fn new() -> Self {
        Self {
            shelf: BiquadCoefficients::IDENTITY,
            high_pass: BiquadCoefficients::IDENTITY,
            shelf_state: BiquadState::default(),
            high_pass_state: BiquadState::default(),
        }
    }

    pub fn configure(&mut self, sample_rate: f64) {
        self.shelf = shelf(sample_rate);
        self.high_pass = high_pass(sample_rate);
        self.clear();
    }

    pub fn clear(&mut self) {
        self.shelf_state = BiquadState::default();
        self.high_pass_state = BiquadState::default();
    }

    /// Filters one sample.
    #[cfg(test)]
    pub fn step(&mut self, x: f64) -> f64 {
        let mid = biquad_step(&self.shelf, &mut self.shelf_state, x);
        biquad_step(&self.high_pass, &mut self.high_pass_state, mid)
    }

    /// Filters the samples and returns the sum of their squares. The state
    /// lives in locals for the whole loop, which keeps the hot path fast.
    pub fn sum_of_squares(&mut self, x: &[f32]) -> f64 {
        let (shelf, high_pass) = (self.shelf, self.high_pass);
        let (mut shelf_state, mut high_pass_state) = (self.shelf_state, self.high_pass_state);
        let mut sum = 0.0;
        for &sample in x {
            let mid = biquad_step(&shelf, &mut shelf_state, sample as f64);
            let out = biquad_step(&high_pass, &mut high_pass_state, mid);
            sum += out * out;
        }
        self.shelf_state = shelf_state;
        self.high_pass_state = high_pass_state;
        sum
    }

    /// Magnitude of the whole K-weighting chain at `hz`. For tests and design checks.
    #[cfg(test)]
    pub fn magnitude(&self, hz: f64, sample_rate: f64) -> f64 {
        let magnitude = |c: &BiquadCoefficients| {
            let w = 2.0 * PI * hz / sample_rate;
            let (cr, ci) = (w.cos(), -w.sin());
            let (c2r, c2i) = ((2.0 * w).cos(), -(2.0 * w).sin());
            let nr = c.b0 + c.b1 * cr + c.b2 * c2r;
            let ni = c.b1 * ci + c.b2 * c2i;
            let dr = 1.0 + c.a1 * cr + c.a2 * c2r;
            let di = c.a1 * ci + c.a2 * c2i;
            ((nr * nr + ni * ni) / (dr * dr + di * di)).sqrt()
        };
        magnitude(&self.shelf) * magnitude(&self.high_pass)
    }
}
