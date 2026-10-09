//! A band-pass filter bank: one 8th-order Linkwitz-Riley band-pass per listening band.
//!
//! Port source: `Sources/JoseonCore/Stereo/BandSplitFilterBank.swift`.
//!
//! Each band runs a 4-section low-pass at the high edge, then a 4-section
//! high-pass at the low edge. Both skirts fall at 48 dB per octave with −6 dB
//! at the corner. 24 dB per octave is not enough here: a strong tone two and a
//! half octaves away still lands 64 dB into a neighbour band, which is loud
//! enough to give an empty band a confident, wrong correlation and balance.
//!
//! The low-pass runs first on purpose. With the high-pass first, a mid-band
//! tone walks into the 20 Hz high-pass, and its start-up transient then leaks
//! through the 60 Hz low-pass into the sub-bass band.
//!
//! Left and right use the same coefficients and separate delay state. A rate
//! change builds a new bank.
//!
//! Filtering runs in double precision: the lowest band edge (20 Hz) sits at a
//! normalized frequency near 2·10⁻⁴ at 96 kHz, where single-precision biquads
//! lose accuracy.
//!
//! ## All bands of a channel in one pass
//!
//! The bands are independent, so they run together, one lane of an 8-wide
//! array each. The section loop is outside and the sample loop inside, which
//! keeps the two state words of a section in registers for the whole block.
//! The arithmetic is transposed direct form II.
//!
//! Port note: the Swift bank has one call per band and side, and a rule to ask
//! for band 0 first. Here `run` filters both sides of a block, and `left` and
//! `right` give the rows.

/// Sections per band: 4 low-pass then 4 high-pass.
pub(crate) const SECTIONS_PER_BAND: usize = 8;
/// Bands carried together in one group.
const LANES: usize = 8;
/// Frames the scratch is sized for at first. A longer block grows it, once.
const DEFAULT_CAPACITY: usize = 1024;

/// One 8th-order Linkwitz-Riley skirt: two cascaded 4th-order Butterworth
/// filters, each a pair of sections with these Q values.
pub(crate) const LINKWITZ_RILEY_8_QS: [f64; 4] = [
    0.5411961001461969,
    1.3065629648763766,
    0.5411961001461969,
    1.3065629648763766,
];

/// One biquad section for
/// `y[n] = b0·x[n] + b1·x[n-1] + b2·x[n-2] − a1·y[n-1] − a2·y[n-2]`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct BiquadSection {
    pub b0: f64,
    pub b1: f64,
    pub b2: f64,
    pub a1: f64,
    pub a2: f64,
}

impl BiquadSection {
    /// Unity gain, no state.
    pub const PASS_THROUGH: Self = Self {
        b0: 1.0,
        b1: 0.0,
        b2: 0.0,
        a1: 0.0,
        a2: 0.0,
    };

    /// Second-order low-pass (RBJ cookbook).
    pub fn low_pass(hz: f64, sample_rate: f64, q: f64) -> Self {
        let w0 = angular_frequency(hz, sample_rate);
        let cos_w = w0.cos();
        let alpha = w0.sin() / (2.0 * q);
        let a0 = 1.0 + alpha;
        let b = (1.0 - cos_w) / 2.0;
        Self {
            b0: b / a0,
            b1: (1.0 - cos_w) / a0,
            b2: b / a0,
            a1: (-2.0 * cos_w) / a0,
            a2: (1.0 - alpha) / a0,
        }
    }

    /// Second-order high-pass (RBJ cookbook).
    pub fn high_pass(hz: f64, sample_rate: f64, q: f64) -> Self {
        let w0 = angular_frequency(hz, sample_rate);
        let cos_w = w0.cos();
        let alpha = w0.sin() / (2.0 * q);
        let a0 = 1.0 + alpha;
        let b = (1.0 + cos_w) / 2.0;
        Self {
            b0: b / a0,
            b1: (-(1.0 + cos_w)) / a0,
            b2: b / a0,
            a1: (-2.0 * cos_w) / a0,
            a2: (1.0 - alpha) / a0,
        }
    }
}

fn angular_frequency(hz: f64, sample_rate: f64) -> f64 {
    let limit = sample_rate * 0.49;
    let f = hz.max(0.1).min(limit);
    2.0 * std::f64::consts::PI * f / sample_rate
}

/// The eight sections of one band.
pub(crate) fn band_sections(low_edge: f64, high_edge: f64, sample_rate: f64) -> [BiquadSection; SECTIONS_PER_BAND] {
    // A top band that reaches past Nyquist keeps its high-pass only: a low-pass
    // that close to Nyquist buys nothing and is numerically awkward.
    let keep_low_pass = high_edge < sample_rate * 0.45;
    let mut sections = [BiquadSection::PASS_THROUGH; SECTIONS_PER_BAND];
    for (i, &q) in LINKWITZ_RILEY_8_QS.iter().enumerate() {
        if keep_low_pass {
            sections[i] = BiquadSection::low_pass(high_edge, sample_rate, q);
        }
        if low_edge > 0.0 {
            sections[4 + i] = BiquadSection::high_pass(low_edge, sample_rate, q);
        }
    }
    sections
}

type Lane = [f64; LANES];

/// One sample of one section, for the eight bands of a group. Transposed direct form II.
#[inline(always)]
fn step(c: &SectionLanes, state: &mut StateLanes, lane: &mut Lane) {
    for l in 0..LANES {
        let x = lane[l];
        let y = c.b0[l] * x + state.z1[l];
        state.z1[l] = c.b1[l] * x - c.a1[l] * y + state.z2[l];
        state.z2[l] = c.b2[l] * x - c.a2[l] * y;
        lane[l] = y;
    }
}

/// Coefficients of one section for the eight bands of a group.
#[derive(Clone, Copy)]
struct SectionLanes {
    b0: Lane,
    b1: Lane,
    b2: Lane,
    a1: Lane,
    a2: Lane,
}

/// Delay state of one section for the eight bands of a group.
#[derive(Clone, Copy, Default)]
struct StateLanes {
    z1: Lane,
    z2: Lane,
}

pub(crate) struct BandSplitFilterBank {
    band_count: usize,
    group_count: usize,
    /// `group_count * SECTIONS_PER_BAND` entries. A lane with no band carries a
    /// pass-through section: it costs nothing and stays finite.
    coefficients: Vec<SectionLanes>,
    state_left: Vec<StateLanes>,
    state_right: Vec<StateLanes>,
    /// `band_count * capacity`: one filtered row per band.
    rows_left: Vec<f64>,
    rows_right: Vec<f64>,
    /// The block being filtered, all bands of a group side by side, per side.
    lanes_left: Vec<Lane>,
    lanes_right: Vec<Lane>,
    capacity: usize,
    /// Frames in the rows now.
    held: usize,
}

impl BandSplitFilterBank {
    /// Builds the bank. `None` when the edges or the sample rate make no sense.
    pub fn new(edges_hz: &[f32], sample_rate: f64) -> Option<Self> {
        if !(edges_hz.len() >= 2 && sample_rate > 0.0 && sample_rate.is_finite()) {
            return None;
        }
        let band_count = edges_hz.len() - 1;
        let group_count = band_count.div_ceil(LANES);
        let slots = group_count * SECTIONS_PER_BAND;
        let pass = SectionLanes {
            b0: [1.0; LANES],
            b1: [0.0; LANES],
            b2: [0.0; LANES],
            a1: [0.0; LANES],
            a2: [0.0; LANES],
        };
        let mut coefficients = vec![pass; slots];
        for band in 0..band_count {
            let sections = band_sections(edges_hz[band] as f64, edges_hz[band + 1] as f64, sample_rate);
            let (group, slot) = (band / LANES, band % LANES);
            for (i, section) in sections.iter().enumerate() {
                let lanes = &mut coefficients[group * SECTIONS_PER_BAND + i];
                lanes.b0[slot] = section.b0;
                lanes.b1[slot] = section.b1;
                lanes.b2[slot] = section.b2;
                lanes.a1[slot] = section.a1;
                lanes.a2[slot] = section.a2;
            }
        }
        Some(Self {
            band_count,
            group_count,
            coefficients,
            state_left: vec![StateLanes::default(); slots],
            state_right: vec![StateLanes::default(); slots],
            rows_left: vec![0.0; band_count * DEFAULT_CAPACITY],
            rows_right: vec![0.0; band_count * DEFAULT_CAPACITY],
            lanes_left: vec![[0.0; LANES]; DEFAULT_CAPACITY],
            lanes_right: vec![[0.0; LANES]; DEFAULT_CAPACITY],
            capacity: DEFAULT_CAPACITY,
            held: 0,
        })
    }

    #[cfg(test)]
    pub fn band_count(&self) -> usize {
        self.band_count
    }

    /// Clears the filter memory. Coefficients stay.
    pub fn reset_state(&mut self) {
        self.state_left.fill(StateLanes::default());
        self.state_right.fill(StateLanes::default());
        self.held = 0;
    }

    /// Filters one block of both sides through every band. The two slices have
    /// the same length. No allocation, unless the block is longer than any
    /// block before it.
    ///
    /// Left and right go through each section in the same sample loop. A biquad
    /// is a chain: each sample waits for the one before it. Two chains side by
    /// side keep the processor busy while each one waits, so both sides cost
    /// little more than one.
    pub fn run(&mut self, left: &[f64], right: &[f64]) {
        let count = left.len().min(right.len());
        self.grow(count);
        let (lanes_left, lanes_right) = (&mut self.lanes_left[..count], &mut self.lanes_right[..count]);
        for group in 0..self.group_count {
            for i in 0..count {
                lanes_left[i] = [left[i]; LANES];
                lanes_right[i] = [right[i]; LANES];
            }
            for section in 0..SECTIONS_PER_BAND {
                let index = group * SECTIONS_PER_BAND + section;
                let c = self.coefficients[index];
                let (mut left_state, mut right_state) = (self.state_left[index], self.state_right[index]);
                for (lane_left, lane_right) in lanes_left.iter_mut().zip(lanes_right.iter_mut()) {
                    step(&c, &mut left_state, lane_left);
                    step(&c, &mut right_state, lane_right);
                }
                self.state_left[index] = left_state;
                self.state_right[index] = right_state;
            }
            let first = group * LANES;
            for band in first..(first + LANES).min(self.band_count) {
                let row_left = &mut self.rows_left[band * self.capacity..][..count];
                let row_right = &mut self.rows_right[band * self.capacity..][..count];
                for i in 0..count {
                    row_left[i] = lanes_left[i][band - first];
                    row_right[i] = lanes_right[i][band - first];
                }
            }
        }
        self.held = count;
    }

    /// The left side of the last block, through `band`.
    pub fn left(&self, band: usize) -> &[f64] {
        &self.rows_left[band * self.capacity..][..self.held]
    }

    /// The right side of the last block, through `band`.
    pub fn right(&self, band: usize) -> &[f64] {
        &self.rows_right[band * self.capacity..][..self.held]
    }

    /// Only does work when a block is longer than any before it, which in the
    /// analyzer means never.
    fn grow(&mut self, count: usize) {
        if count <= self.capacity {
            return;
        }
        self.capacity = count.max(self.capacity * 2);
        self.rows_left = vec![0.0; self.band_count * self.capacity];
        self.rows_right = vec![0.0; self.band_count * self.capacity];
        self.lanes_left = vec![[0.0; LANES]; self.capacity];
        self.lanes_right = vec![[0.0; LANES]; self.capacity];
        self.held = 0;
    }
}
